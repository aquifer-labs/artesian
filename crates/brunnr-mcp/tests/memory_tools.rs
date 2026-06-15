// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use brunnr_core::{AgentBinding, AgentCatalog, AgentCatalogEntry, AgentModel, Mode, Role};
use brunnr_mcp::{
    AnchorSetRequest, BindRequest, DelegateRequest, FindRequest, MemoryServer, StoreRequest,
    ToolsFindRequest,
};
use brunnr_test_support::TempDir;
use mimisbrunnr::{
    MemoryResult, SqliteVecVectorStore, TextEmbedder, VectorMemoryBackend, VectorMemoryConfig,
};
use rmcp::handler::server::wrapper::Parameters;

#[tokio::test]
async fn memory_tools_store_and_find_with_files_backend() {
    let tempdir = TempDir::new("mcp");
    let server = MemoryServer::new(tempdir.path());

    let stored = server
        .memory_store(Parameters(StoreRequest {
            content: "MCP memory tool round trip".to_string(),
            tags: Some(vec!["mcp".to_string()]),
            node_id: Some("node:mcp".to_string()),
            scope: None,
            agent_id: None,
            session_id: None,
            task_id: None,
            user_id: None,
        }))
        .await
        .expect("store should succeed")
        .0;

    let found = server
        .memory_find(Parameters(FindRequest {
            query: "round".to_string(),
            limit: Some(5),
            node_id: Some("node:mcp".to_string()),
            scope: None,
            agent_id: None,
            session_id: None,
            task_id: None,
            user_id: None,
        }))
        .await
        .expect("find should succeed")
        .0;

    assert_eq!(stored.node_id, "node:mcp");
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].node_id, "node:mcp");
    assert_eq!(found.hits[0].content, "MCP memory tool round trip");
}

#[tokio::test]
async fn memory_anchor_tools_round_trip_with_files_backend() {
    let tempdir = TempDir::new("mcp-anchor");
    let server = MemoryServer::new(tempdir.path());

    server
        .memory_anchor_set(Parameters(AnchorSetRequest {
            current_task: "implement anchor tools".to_string(),
            plan_pointer: Some("docs/self-repair.md#muninn".to_string()),
            last_decisions: Some(vec!["append-only log".to_string()]),
            next_step: "verify MCP round trip".to_string(),
        }))
        .await
        .expect("anchor set should succeed");
    let response = server
        .memory_anchor_get()
        .await
        .expect("anchor get should succeed")
        .0;

    let anchor = response.anchor.expect("anchor should exist");
    assert_eq!(anchor.current_task, "implement anchor tools");
    assert_eq!(anchor.next_step, "verify MCP round trip");
    assert_eq!(anchor.last_decisions, vec!["append-only log"]);
}

#[tokio::test]
async fn tools_find_is_opt_in_and_reports_token_delta() {
    let tempdir = TempDir::new("mcp-tools-find");
    let disabled = MemoryServer::new(tempdir.path());
    assert!(
        disabled
            .tools_find(Parameters(ToolsFindRequest {
                task: "resume from anchor and search memory".to_string(),
                limit: Some(2),
            }))
            .await
            .is_err(),
        "router should be disabled by default"
    );

    let enabled = MemoryServer::new(tempdir.path()).with_router_enabled(true);
    let response = enabled
        .tools_find(Parameters(ToolsFindRequest {
            task: "resume from anchor and search memory".to_string(),
            limit: Some(2),
        }))
        .await
        .expect("tools.find should run when enabled")
        .0;

    assert!(!response.tools.is_empty());
    assert!(response.prompt_tokens_delta > 0);
    assert!(response
        .tools
        .iter()
        .any(|tool| tool.name == "memory.anchor.get" || tool.name == "memory.find"));
}

#[tokio::test]
async fn orchestration_tools_are_mode_gated_and_agents_list_reflects_catalog() {
    let tempdir = TempDir::new("mcp-orchestration-tools");
    let memory = MemoryServer::new(tempdir.path());
    let memory_tools = memory.visible_tool_names();
    assert!(memory_tools.contains(&"memory.find".to_string()));
    assert!(!memory_tools.contains(&"agents.list".to_string()));
    assert!(!memory_tools.contains(&"orchestrate.delegate".to_string()));

    let catalog = AgentCatalog {
        generated_at: Some("test".to_string()),
        agents: vec![AgentCatalogEntry {
            agent: "codex".to_string(),
            command: Some("sh".to_string()),
            reachable: true,
            models: vec![AgentModel {
                id: "gpt-5.5".to_string(),
                reachable: true,
                source: "test".to_string(),
            }],
        }],
    };
    let orchestrate = MemoryServer::new(tempdir.path())
        .with_mode(Mode::Orchestrate)
        .with_catalog(catalog.clone());
    let tools = orchestrate.visible_tool_names();
    assert!(tools.contains(&"agents.list".to_string()));
    assert!(tools.contains(&"orchestrate.delegate".to_string()));
    let response = orchestrate
        .agents_list()
        .await
        .expect("agents.list should run")
        .0;
    assert_eq!(response.catalog, catalog);
}

#[tokio::test]
async fn orchestrate_bind_rejects_unavailable_model() {
    let tempdir = TempDir::new("mcp-bind-unavailable");
    let server = MemoryServer::new(tempdir.path()).with_mode(Mode::Orchestrate);

    let result = server
        .orchestrate_bind(Parameters(BindRequest {
            role: "worker".to_string(),
            agent: "codex".to_string(),
            model: "not-a-codex-model".to_string(),
            command: Some("sh".to_string()),
            args: None,
            timeout_seconds: None,
        }))
        .await;
    let Err(error) = result else {
        panic!("unknown model should fail early");
    };

    assert!(error.to_string().contains("not-a-codex-model"));
}

#[tokio::test]
async fn orchestrate_bind_uses_cached_catalog_models() {
    let tempdir = TempDir::new("mcp-bind-catalog");
    let catalog = AgentCatalog {
        generated_at: Some("test".to_string()),
        agents: vec![AgentCatalogEntry {
            agent: "custom-agent".to_string(),
            command: Some("sh".to_string()),
            reachable: true,
            models: vec![AgentModel {
                id: "provider-only-model".to_string(),
                reachable: true,
                source: "provider-api".to_string(),
            }],
        }],
    };
    let server = MemoryServer::new(tempdir.path())
        .with_mode(Mode::Orchestrate)
        .with_catalog(catalog);

    let response = server
        .orchestrate_bind(Parameters(BindRequest {
            role: "worker".to_string(),
            agent: "custom-agent".to_string(),
            model: "provider-only-model".to_string(),
            command: Some("sh".to_string()),
            args: None,
            timeout_seconds: None,
        }))
        .await
        .expect("cached catalog model should bind")
        .0;

    assert_eq!(response.binding.agent, "custom-agent");
    assert_eq!(
        response.binding.model.as_deref(),
        Some("provider-only-model")
    );
}

#[tokio::test]
#[cfg(unix)]
async fn orchestrate_delegate_timeout_uses_supervised_cleanup() {
    let tempdir = TempDir::new("mcp-delegate-timeout");
    let parent_pid_file = tempdir.join("worker.pid");
    let child_pid_file = tempdir.join("grandchild.pid");
    let script = format!(
        "echo $$ > \"{}\"; sleep 30 & echo $! > \"{}\"; wait",
        parent_pid_file.display(),
        child_pid_file.display()
    );
    let server = MemoryServer::new(tempdir.join("memory"))
        .with_mode(Mode::Orchestrate)
        .with_task_root(tempdir.join("tasks"))
        .with_repo_root(tempdir.path())
        .with_process_registry_dir(tempdir.join("spawns"))
        .with_bindings(vec![AgentBinding {
            role: Role::Worker,
            agent: "codex".to_string(),
            model: Some("gpt-5.5".to_string()),
            command: Some("sh".to_string()),
            args: vec!["-c".to_string(), script],
            timeout_seconds: Some(1),
        }]);

    let result = server
        .orchestrate_delegate(Parameters(DelegateRequest {
            role: "worker".to_string(),
            task: "Keep a child process alive until timeout".to_string(),
        }))
        .await;
    let Err(error) = result else {
        panic!("delegation should time out");
    };

    assert!(error.to_string().contains("timed out"));
    assert_pid_gone(read_pid(&parent_pid_file));
    assert_pid_gone(read_pid(&child_pid_file));
}

#[tokio::test]
async fn memory_tools_store_and_find_with_sqlite_vec_backend() {
    let store = SqliteVecVectorStore::in_memory().expect("sqlite-vec should open");
    let backend = VectorMemoryBackend::with_embedder(
        store,
        VectorMemoryConfig {
            collection: "mcp_sqlite".to_string(),
            dimensions: TEST_DIMENSIONS,
            ..VectorMemoryConfig::new("mcp_sqlite")
        },
        Arc::new(TestEmbedder),
    )
    .expect("backend should construct");
    let server = MemoryServer::with_backend(Arc::new(backend));

    server
        .memory_store(Parameters(StoreRequest {
            content: "MCP sqlite vector memory round trip".to_string(),
            tags: Some(vec!["mcp".to_string()]),
            node_id: Some("node:mcp-sqlite".to_string()),
            scope: None,
            agent_id: None,
            session_id: None,
            task_id: None,
            user_id: None,
        }))
        .await
        .expect("store should succeed");

    let found = server
        .memory_find(Parameters(FindRequest {
            query: "vector".to_string(),
            limit: Some(5),
            node_id: Some("node:mcp-sqlite".to_string()),
            scope: None,
            agent_id: None,
            session_id: None,
            task_id: None,
            user_id: None,
        }))
        .await
        .expect("find should succeed")
        .0;

    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].node_id, "node:mcp-sqlite");
}

const TEST_DIMENSIONS: usize = 8;

struct TestEmbedder;

impl TextEmbedder for TestEmbedder {
    fn embed_query(&self, text: &str) -> MemoryResult<Vec<f32>> {
        Ok(test_embedding(text))
    }

    fn embed_passage(&self, text: &str) -> MemoryResult<Vec<f32>> {
        Ok(test_embedding(text))
    }
}

fn test_embedding(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0; TEST_DIMENSIONS];
    for token in text.split_whitespace() {
        let index = token.bytes().fold(0usize, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(byte as usize)
        }) % TEST_DIMENSIONS;
        vector[index] += 1.0;
    }
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        for value in &mut vector {
            *value /= magnitude;
        }
    }
    vector
}

#[cfg(unix)]
fn read_pid(path: &std::path::Path) -> u32 {
    wait_for_file(path);
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .trim()
        .parse()
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{} was not written", path.display());
}

#[cfg(unix)]
fn assert_pid_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("pid {pid} survived delegated timeout cleanup");
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
