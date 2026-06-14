// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use brunnr_core::{
    Agent, AgentCapabilities, AgentEvent, AgentEventStream, AgentMessage, AgentResponse,
    AgentResult, AgentSession, EventType, Mode, ResourceQuota, SpawnRequest,
};
use brunnr_test_support::TempDir;
use futures_util::{future::BoxFuture, stream, FutureExt};
use hvergelmir::ScratchWorkspaceProvider;
use mimisbrunnr::FilesBackend;
use thingr::{
    FilesTaskStore, NewTask, Task, TaskResult, TaskStatus, TaskStore, Verifier, VerifierGate,
    VerifierOutcome,
};
use urdarbrunnr::{Orchestrator, OrchestratorConfig, OrchestratorError};

#[tokio::test]
async fn three_task_dag_dispatches_parallel_then_synthesis_task() {
    let tempdir = TempDir::new("urdar-dag");
    let store = Arc::new(FilesTaskStore::new(tempdir.join("tasks")));
    create_task(store.as_ref(), "A", "Collect data", []).await;
    create_task(store.as_ref(), "B", "Check constraints", []).await;
    create_task(store.as_ref(), "C", "Synthesize", ["A", "B"]).await;
    let agent = Arc::new(MockAgent::new(Duration::from_millis(50)));
    let mut orchestrator = test_orchestrator(tempdir.path(), store.clone(), agent.clone(), None, 2);

    let report = orchestrator
        .run_until_idle(5)
        .await
        .expect("orchestrator should complete DAG");

    assert_eq!(report.completed, 3);
    assert!(agent.max_active() >= 2, "A and B should run in parallel");
    let tasks = store.list().await.expect("tasks should list");
    assert!(tasks.iter().all(|task| task.status == TaskStatus::Done));
    let events = &orchestrator.run_log().events;
    let c_claimed = event_position(events, EventType::TaskClaimed, "C").expect("C claimed");
    let a_verdict = event_position(events, EventType::Verdict, "A").expect("A verdict");
    let b_verdict = event_position(events, EventType::Verdict, "B").expect("B verdict");
    assert!(c_claimed > a_verdict);
    assert!(c_claimed > b_verdict);
    for event in events
        .iter()
        .filter(|event| matches!(event.event_type, EventType::Result | EventType::Verdict))
    {
        assert!(["A", "B", "C"].contains(&event.correlation_id.as_str()));
    }
}

#[tokio::test]
async fn verifier_failure_retries_then_blocks_and_pass_creates_galdr() {
    let tempdir = TempDir::new("urdar-retry");
    let store = Arc::new(FilesTaskStore::new(tempdir.join("tasks")));
    create_task(store.as_ref(), "fail", "Fails verification", []).await;
    let agent = Arc::new(MockAgent::new(Duration::ZERO));
    let mut orchestrator = test_orchestrator(
        tempdir.path(),
        store.clone(),
        agent,
        Some(VerifierGate::new(vec![Arc::new(StaticVerifier::fail(
            "test",
        ))])),
        1,
    );
    orchestrator.config_mut().max_retries = 1;

    let report = orchestrator
        .run_until_idle(5)
        .await
        .expect("failed verifier should not abort loop");

    assert_eq!(report.blocked, 1);
    let task = store
        .get("fail")
        .await
        .expect("task lookup should succeed")
        .expect("task exists");
    assert_eq!(task.status, TaskStatus::Blocked);
    assert!(orchestrator.run_log().galdr.is_empty());

    let tempdir = TempDir::new("urdar-pass");
    let store = Arc::new(FilesTaskStore::new(tempdir.join("tasks")));
    create_task(store.as_ref(), "pass", "Passes verification", []).await;
    let agent = Arc::new(MockAgent::new(Duration::ZERO));
    let mut orchestrator = test_orchestrator(
        tempdir.path(),
        store.clone(),
        agent,
        Some(VerifierGate::new(vec![Arc::new(StaticVerifier::pass(
            "test",
        ))])),
        1,
    );
    orchestrator
        .run_until_idle(3)
        .await
        .expect("passed verifier should complete");

    let task = store
        .get("pass")
        .await
        .expect("task lookup should succeed")
        .expect("task exists");
    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(orchestrator.run_log().galdr.len(), 1);
}

#[tokio::test]
async fn concurrency_limit_and_resource_quota_are_honored() {
    let tempdir = TempDir::new("urdar-limit");
    let store = Arc::new(FilesTaskStore::new(tempdir.join("tasks")));
    create_task(store.as_ref(), "one", "One", []).await;
    create_task(store.as_ref(), "two", "Two", []).await;
    let agent = Arc::new(MockAgent::new(Duration::from_millis(50)));
    let mut orchestrator = test_orchestrator(tempdir.path(), store.clone(), agent.clone(), None, 1);

    let report = orchestrator.run_once().await.expect("tick should run");

    assert_eq!(report.dispatched, 1);
    assert_eq!(agent.max_active(), 1);
    let tasks = store.list().await.expect("tasks should list");
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Done)
            .count(),
        1
    );
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Todo)
            .count(),
        1
    );

    let tempdir = TempDir::new("urdar-quota");
    let store = Arc::new(FilesTaskStore::new(tempdir.join("tasks")));
    create_task(store.as_ref(), "quota-one", "One", []).await;
    create_task(store.as_ref(), "quota-two", "Two", []).await;
    let agent = Arc::new(MockAgent::new(Duration::ZERO));
    let mut orchestrator = test_orchestrator(tempdir.path(), store.clone(), agent, None, 2);
    orchestrator.config_mut().quotas = vec![ResourceQuota {
        agent_id: Some("worker".to_string()),
        user_id: None,
        max_prompt_tokens: None,
        max_requests_per_minute: Some(1),
    }];

    let report = orchestrator.run_once().await.expect("tick should run");

    assert_eq!(report.dispatched, 1);
    assert!(orchestrator
        .run_log()
        .events
        .iter()
        .any(|event| event.event_type == EventType::Status));
}

#[tokio::test]
async fn memory_mode_keeps_orchestration_disabled_without_side_effects() {
    let tempdir = TempDir::new("urdar-memory-disabled");
    let store = Arc::new(FilesTaskStore::new(tempdir.join("tasks")));
    create_task(store.as_ref(), "idle", "Do nothing", []).await;
    let agent = Arc::new(MockAgent::new(Duration::ZERO));
    let mut orchestrator = test_orchestrator(tempdir.path(), store.clone(), agent, None, 1);
    orchestrator.config_mut().mode = Mode::Memory;

    let error = orchestrator
        .run_once()
        .await
        .expect_err("memory mode must reject orchestration");

    assert!(matches!(error, OrchestratorError::Disabled(Mode::Memory)));
    assert!(orchestrator.run_log().events.is_empty());
    let task = store
        .get("idle")
        .await
        .expect("task lookup should succeed")
        .expect("task exists");
    assert_eq!(task.status, TaskStatus::Todo);
}

async fn create_task<const N: usize>(
    store: &FilesTaskStore,
    id: &str,
    title: &str,
    blockers: [&str; N],
) {
    let mut task = NewTask::primitive(title);
    task.id = Some(id.to_string());
    task.blockers = blockers.into_iter().map(str::to_string).collect();
    store.create(task).await.expect("task should be created");
}

fn test_orchestrator(
    root: &std::path::Path,
    store: Arc<FilesTaskStore>,
    agent: Arc<MockAgent>,
    verifier_gate: Option<VerifierGate>,
    concurrency_limit: usize,
) -> Orchestrator {
    let config = OrchestratorConfig {
        mode: Mode::Orchestrate,
        repo_root: root.to_path_buf(),
        concurrency_limit,
        max_retries: 0,
        retry_backoff: Duration::ZERO,
        memory_limit: 3,
        quotas: Vec::new(),
        topology: Default::default(),
    };
    Orchestrator::new(
        config,
        store,
        Arc::new(FilesBackend::new(root.join("memory"))),
        Arc::new(ScratchWorkspaceProvider::new(root.join("scratch"))),
        agent,
        None,
        verifier_gate.unwrap_or_default(),
    )
}

fn event_position(events: &[brunnr_core::EventEnvelope], ty: EventType, id: &str) -> Option<usize> {
    events
        .iter()
        .position(|event| event.event_type == ty && event.correlation_id == id)
}

#[derive(Debug)]
struct MockAgent {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    delay: Duration,
}

impl MockAgent {
    fn new(delay: Duration) -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            delay,
        }
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

impl Agent for MockAgent {
    fn spawn(&self, request: SpawnRequest) -> BoxFuture<'_, AgentResult<AgentSession>> {
        async move {
            Ok(AgentSession {
                id: format!("mock-{}-{}", request.role.canonical_alias(), request.agent),
                role: request.role,
                agent: request.agent,
            })
        }
        .boxed()
    }

    fn send(
        &self,
        _session: &AgentSession,
        message: AgentMessage,
    ) -> BoxFuture<'_, AgentResult<AgentResponse>> {
        async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            update_max(&self.max_active, active);
            if self.delay > Duration::ZERO {
                tokio::time::sleep(self.delay).await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(AgentResponse {
                content: format!(
                    "done {}",
                    message.content.lines().next().unwrap_or_default()
                ),
            })
        }
        .boxed()
    }

    fn stream(
        &self,
        _session: &AgentSession,
        message: AgentMessage,
    ) -> BoxFuture<'_, AgentResult<AgentEventStream>> {
        async move {
            Ok(Box::pin(stream::iter([
                Ok(AgentEvent::Text(message.content)),
                Ok(AgentEvent::Done),
            ])) as AgentEventStream)
        }
        .boxed()
    }

    fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities {
            streaming: false,
            tools: false,
            mcp: false,
        }
    }
}

fn update_max(max_active: &AtomicUsize, value: usize) {
    let mut current = max_active.load(Ordering::SeqCst);
    while value > current {
        match max_active.compare_exchange(current, value, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[derive(Debug)]
struct StaticVerifier {
    name: &'static str,
    passed: bool,
}

impl StaticVerifier {
    fn pass(name: &'static str) -> Self {
        Self { name, passed: true }
    }

    fn fail(name: &'static str) -> Self {
        Self {
            name,
            passed: false,
        }
    }
}

impl Verifier for StaticVerifier {
    fn name(&self) -> &str {
        self.name
    }

    fn verify(&self, _task: &Task) -> BoxFuture<'_, TaskResult<VerifierOutcome>> {
        async move {
            Ok(VerifierOutcome {
                name: self.name.to_string(),
                passed: self.passed,
                output: "static".to_string(),
            })
        }
        .boxed()
    }
}
