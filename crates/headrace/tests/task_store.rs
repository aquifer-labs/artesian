// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use aquifer::FilesBackend;
use artesian_test_support::TempDir;
use futures_util::{FutureExt, future::BoxFuture};
use headrace::{
    ClaimRequest, FilesTaskStore, NewTask, Task, TaskKind, TaskStatus, TaskStore, VectorTaskStore,
    Verifier, VerifierGate, VerifierOutcome,
};

#[tokio::test]
async fn files_task_store_claims_and_transitions_task() {
    let tempdir = TempDir::new("task-store");
    let store = FilesTaskStore::new(tempdir.path());
    let mut new_task = NewTask::primitive("write task contract");
    new_task.id = Some("task-contract".to_string());
    new_task.description = "exercise the file-backed task lifecycle".to_string();

    let created = store.create(new_task).await.expect("create should succeed");
    let claimed = store
        .claim(ClaimRequest {
            task_id: Some(created.id.clone()),
            claimant: "worker-1".to_string(),
        })
        .await
        .expect("claim should succeed")
        .expect("task should be claimed");
    let done = store
        .transition(headrace::TransitionTask {
            id: created.id.clone(),
            status: TaskStatus::Done,
        })
        .await
        .expect("transition should succeed");

    assert_eq!(claimed.status, TaskStatus::Doing);
    assert_eq!(claimed.claimed_by.as_deref(), Some("worker-1"));
    assert_eq!(done.status, TaskStatus::Done);
    assert!(tempdir.join("tasks/done/task-contract.md").exists());
}

#[tokio::test]
async fn concurrent_claim_race_has_single_winner() {
    let tempdir = TempDir::new("task-claim-race");
    let store = FilesTaskStore::new(tempdir.path());
    let mut new_task = NewTask::primitive("race-safe claim");
    new_task.id = Some("task-race".to_string());
    store.create(new_task).await.expect("create should succeed");

    let left_store = store.clone();
    let right_store = store.clone();
    let left = tokio::spawn(async move {
        left_store
            .claim(ClaimRequest {
                task_id: Some("task-race".to_string()),
                claimant: "left".to_string(),
            })
            .await
    });
    let right = tokio::spawn(async move {
        right_store
            .claim(ClaimRequest {
                task_id: Some("task-race".to_string()),
                claimant: "right".to_string(),
            })
            .await
    });

    let results = [
        left.await
            .expect("left should join")
            .expect("left should run"),
        right
            .await
            .expect("right should join")
            .expect("right should run"),
    ];

    assert_eq!(results.iter().filter(|task| task.is_some()).count(), 1);
}

#[tokio::test]
async fn atomic_claim_stress_has_no_double_dispatch() {
    let tempdir = TempDir::new("task-claim-stress");
    let store = FilesTaskStore::new(tempdir.path());
    let mut new_task = NewTask::primitive("stress claim");
    new_task.id = Some("task-stress".to_string());
    store.create(new_task).await.expect("create should succeed");

    let mut handles = Vec::new();
    for index in 0..32 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .claim(ClaimRequest {
                    task_id: Some("task-stress".to_string()),
                    claimant: format!("worker-{index}"),
                })
                .await
        }));
    }

    let mut winners = Vec::new();
    for handle in handles {
        if let Some(task) = handle
            .await
            .expect("claimer should join")
            .expect("claim should run")
        {
            winners.push(task);
        }
    }

    assert_eq!(winners.len(), 1);
    assert_eq!(winners[0].status, TaskStatus::Doing);
    assert_eq!(
        store
            .list()
            .await
            .expect("list should run")
            .into_iter()
            .filter(|task| task.id == "task-stress" && task.status == TaskStatus::Doing)
            .count(),
        1
    );
}

#[tokio::test]
async fn dag_blockers_control_dispatch_readiness() {
    let tempdir = TempDir::new("task-dag");
    let store = FilesTaskStore::new(tempdir.path());
    let mut blocker = NewTask::primitive("blocker");
    blocker.id = Some("task-blocker".to_string());
    let mut dependent = NewTask::primitive("dependent");
    dependent.id = Some("task-dependent".to_string());
    dependent.kind = TaskKind::Primitive;
    dependent.blockers = vec!["task-blocker".to_string()];
    store
        .create(blocker)
        .await
        .expect("blocker should be created");
    store
        .create(dependent)
        .await
        .expect("dependent should be created");

    let blocked_claim = store
        .claim(ClaimRequest {
            task_id: Some("task-dependent".to_string()),
            claimant: "worker".to_string(),
        })
        .await
        .expect("claim should run");
    assert!(blocked_claim.is_none());

    store
        .transition(headrace::TransitionTask {
            id: "task-blocker".to_string(),
            status: TaskStatus::Done,
        })
        .await
        .expect("blocker should transition");
    let ready_claim = store
        .claim(ClaimRequest {
            task_id: Some("task-dependent".to_string()),
            claimant: "worker".to_string(),
        })
        .await
        .expect("claim should run")
        .expect("dependent should become dispatch-eligible");

    assert_eq!(ready_claim.id, "task-dependent");
}

#[tokio::test]
async fn verifier_gate_blocks_done_until_all_verifiers_pass() {
    let tempdir = TempDir::new("task-verifier");
    let store = FilesTaskStore::new(tempdir.path());
    let mut new_task = NewTask::primitive("verify before done");
    new_task.id = Some("task-verify".to_string());
    store.create(new_task).await.expect("create should succeed");
    store
        .claim(ClaimRequest {
            task_id: Some("task-verify".to_string()),
            claimant: "worker".to_string(),
        })
        .await
        .expect("claim should run");

    let failing_gate = VerifierGate::new(vec![Arc::new(StaticVerifier::new("lint", false))]);
    assert!(failing_gate.mark_done(&store, "task-verify").await.is_err());
    assert_eq!(
        store
            .get("task-verify")
            .await
            .expect("get should run")
            .expect("task should exist")
            .status,
        TaskStatus::Doing
    );

    let passing_gate = VerifierGate::new(vec![Arc::new(StaticVerifier::new("lint", true))]);
    let done = passing_gate
        .mark_done(&store, "task-verify")
        .await
        .expect("passing gate should mark done");
    assert_eq!(done.status, TaskStatus::Done);
}

#[tokio::test]
async fn vector_task_store_indexes_tasks_for_find() {
    let tempdir = TempDir::new("task-vector");
    let source = FilesTaskStore::new(tempdir.join("source"));
    let memory = Arc::new(FilesBackend::new(tempdir.join("memory")));
    let store = VectorTaskStore::new(source, memory);
    let mut new_task = NewTask::primitive("indexed task search");
    new_task.id = Some("task-indexed".to_string());
    new_task.description = "find this task through Aquifer".to_string();
    store.create(new_task).await.expect("create should succeed");

    let found = store.find("Aquifer").await.expect("find should succeed");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "task-indexed");
}

#[tokio::test]
async fn task_like_markdown_import_is_idempotent_and_keeps_path_status() {
    let tempdir = TempDir::new("task-import");
    let task_path = tempdir.join("tasks/doing/imported-task.md");
    let text = "# Imported Task\n\nCarry this task through the importer.";
    let task = FilesTaskStore::parse_task_like_markdown(&task_path, text)
        .expect("task-like markdown should parse");
    let store = FilesTaskStore::new(tempdir.join("store"));

    let first = store
        .import_task(task.clone())
        .await
        .expect("first import should succeed");
    let second = store
        .import_task(task)
        .await
        .expect("second import should succeed");
    let listed = store.list().await.expect("list should succeed");

    assert!(first.imported());
    assert!(!second.imported());
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Imported Task");
    assert_eq!(listed[0].status, TaskStatus::Doing);
}

struct StaticVerifier {
    name: String,
    passed: bool,
}

impl StaticVerifier {
    fn new(name: impl Into<String>, passed: bool) -> Self {
        Self {
            name: name.into(),
            passed,
        }
    }
}

impl Verifier for StaticVerifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn verify(&self, _task: &Task) -> BoxFuture<'_, headrace::TaskResult<VerifierOutcome>> {
        async move {
            Ok(VerifierOutcome {
                name: self.name.clone(),
                passed: self.passed,
                output: String::new(),
            })
        }
        .boxed()
    }
}

#[tokio::test]
async fn transition_preserves_unmodelled_front_matter() {
    let tempdir = TempDir::new("task-extra-keys");
    let store = FilesTaskStore::new(tempdir.path());
    let mut new_task = NewTask::primitive("carry a downstream schema");
    new_task.id = Some("task-extra".to_string());
    let created = store.create(new_task).await.expect("create should succeed");

    // A downstream schema adds front matter headrace itself does not model.
    let authored = tempdir.join("tasks/todo/task-extra.md");
    let text = std::fs::read_to_string(&authored).expect("task file should exist");
    std::fs::write(
        &authored,
        text.replacen("---\n", "---\npriority: \"P0\"\nphase: null\n", 1),
    )
    .expect("rewrite should succeed");

    let reloaded = store
        .get(&created.id)
        .await
        .expect("get should succeed")
        .expect("task should still exist");
    assert!(reloaded.extra.contains_key("priority"));
    assert!(reloaded.extra.contains_key("phase"));

    store
        .transition(headrace::TransitionTask {
            id: created.id.clone(),
            status: TaskStatus::Done,
        })
        .await
        .expect("transition should succeed");

    let rewritten = std::fs::read_to_string(tempdir.join("tasks/done/task-extra.md"))
        .expect("task file should have moved");
    assert!(
        rewritten.contains("priority: P0"),
        "unmodelled keys must survive the rewrite: {rewritten}"
    );
    assert!(
        rewritten.contains("phase: null"),
        "an explicit null is a value, not an absence: {rewritten}"
    );
}

/// Hand-authored backlogs name files `<id>-<slug>.md`, and `list` has always read
/// them. Acting on them is what was broken: the path was rebuilt from the id, so
/// `claim` renamed a `0034.md` that did not exist, got NotFound, and reported "no
/// dispatch-eligible task" — indistinguishable from an empty queue. The file also
/// has to keep its name, or the first transition silently strips the slug.
#[tokio::test]
async fn claims_and_transitions_a_task_whose_file_name_carries_a_slug() {
    let tempdir = TempDir::new("task-slug-name");
    let store = FilesTaskStore::new(tempdir.path());
    std::fs::create_dir_all(tempdir.join("tasks/todo")).expect("todo dir");
    std::fs::write(
        tempdir.join("tasks/todo/0034-unified-vps-management.md"),
        concat!(
            "---\n",
            "type: \"task\"\n",
            "id: \"0034\"\n",
            "title: \"unified vps management\"\n",
            "role: \"worker\"\n",
            "status: \"todo\"\n",
            "kind: \"primitive\"\n",
            "blockers: []\n",
            "children: []\n",
            "claimed_by: null\n",
            "verifier_names: []\n",
            "created_at: \"2026-08-12T00:00:00Z\"\n",
            "updated_at: \"2026-08-12T00:00:00Z\"\n",
            "---\n\nbody\n"
        ),
    )
    .expect("task file should be written");

    let claimed = store
        .claim(ClaimRequest {
            task_id: None,
            claimant: "worker-1".to_string(),
        })
        .await
        .expect("claim should succeed")
        .expect("a slug-named task is still dispatch-eligible");
    assert_eq!(claimed.id, "0034");
    assert!(
        tempdir
            .join("tasks/doing/0034-unified-vps-management.md")
            .exists(),
        "the file keeps its name when it changes status"
    );

    store
        .transition(headrace::TransitionTask {
            id: "0034".to_string(),
            status: TaskStatus::Done,
        })
        .await
        .expect("transition should find the file by id, not by name");
    assert!(
        tempdir
            .join("tasks/done/0034-unified-vps-management.md")
            .exists()
    );
    assert!(!tempdir.join("tasks/done/0034.md").exists());
}
