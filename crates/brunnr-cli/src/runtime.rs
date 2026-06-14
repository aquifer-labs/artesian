// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(feature = "qdrant")]
use std::env;

use anyhow::{bail, Context, Result};
use brunnr_core::{Agent, AgentBinding, BrunnrConfig, MemoryBackendKind, MemoryConfig, Role};
use brunnr_process_agent::{ProcessAgent, ProcessAgentConfig};
use hvergelmir::ScratchWorkspaceProvider;
use mimisbrunnr::{
    FilesBackend, MemoryBackend, SqliteVecVectorStore, SqliteVecVectorStoreConfig,
    VectorMemoryBackend, VectorMemoryConfig,
};
use thingr::{CommandVerifier, FilesTaskStore, Verifier, VerifierGate};
use urdarbrunnr::{DryRunAgent, Orchestrator, OrchestratorConfig};

#[cfg(feature = "qdrant")]
use mimisbrunnr::{QdrantVectorStore, QdrantVectorStoreConfig};

pub fn build_orchestrator(
    config: BrunnrConfig,
    root: PathBuf,
    repo_root: PathBuf,
    dry_run: bool,
) -> Result<Orchestrator> {
    let memory = open_memory_backend(&config.memory)?;
    let task_store = Arc::new(FilesTaskStore::new(&root));
    let workspace_provider = Arc::new(ScratchWorkspaceProvider::new(root.join("workspaces")));
    let verifier_gate = verifier_gate_from_config(&config);
    let worker: Arc<dyn Agent> = if dry_run {
        Arc::new(DryRunAgent::new("dry-run-worker"))
    } else {
        Arc::new(process_agent_from_binding(
            &config,
            Role::Worker,
            &repo_root,
        )?)
    };
    let judge = if dry_run {
        Some(Arc::new(DryRunAgent::new("dry-run-judge")) as Arc<dyn Agent>)
    } else {
        config
            .agents
            .iter()
            .find(|binding| binding.role == Role::Judge)
            .map(|binding| process_agent_from_binding_value(binding, &repo_root))
            .transpose()?
            .map(|agent| Arc::new(agent) as Arc<dyn Agent>)
    };
    let orchestrator_config = OrchestratorConfig::from_brunnr(&config, repo_root);
    Ok(Orchestrator::new(
        orchestrator_config,
        task_store,
        memory,
        workspace_provider,
        worker,
        judge,
        verifier_gate,
    ))
}

pub fn load_config(config_path: &Path) -> Result<BrunnrConfig> {
    let text = fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    BrunnrConfig::from_toml(&text).with_context(|| format!("parse {}", config_path.display()))
}

pub fn open_memory_backend(config: &MemoryConfig) -> Result<Arc<dyn MemoryBackend>> {
    match config.backend {
        MemoryBackendKind::Files => Ok(Arc::new(FilesBackend::new(&config.root))),
        MemoryBackendKind::SqliteVec => {
            let store = SqliteVecVectorStore::open(SqliteVecVectorStoreConfig::new(sqlite_path(
                &config.root,
            )))?;
            Ok(Arc::new(VectorMemoryBackend::new(
                store,
                VectorMemoryConfig::new(&config.collection),
            )?))
        }
        MemoryBackendKind::Qdrant => open_qdrant_backend(config),
        MemoryBackendKind::TencentDb => bail!("TencentDB backend is not available yet"),
    }
}

fn verifier_gate_from_config(config: &BrunnrConfig) -> VerifierGate {
    let verifiers = config
        .coordination
        .verifiers
        .iter()
        .map(|verifier| {
            Arc::new(
                CommandVerifier::new(verifier.name.clone(), verifier.command.clone())
                    .with_args(verifier.args.clone()),
            ) as Arc<dyn Verifier>
        })
        .collect();
    VerifierGate::new(verifiers)
}

fn process_agent_from_binding(
    config: &BrunnrConfig,
    role: Role,
    repo_root: &Path,
) -> Result<ProcessAgent> {
    let binding = config
        .agents
        .iter()
        .find(|binding| binding.role == role)
        .with_context(|| format!("missing agent binding for role {}", role.canonical_alias()))?;
    process_agent_from_binding_value(binding, repo_root)
}

fn process_agent_from_binding_value(
    binding: &AgentBinding,
    repo_root: &Path,
) -> Result<ProcessAgent> {
    let command = binding
        .command
        .clone()
        .unwrap_or_else(|| binding.agent.clone());
    Ok(ProcessAgent::new(
        ProcessAgentConfig::new(command)
            .with_args(binding.args.clone())
            .with_working_dir(repo_root)
            .with_timeout(Duration::from_secs(binding.timeout_seconds.unwrap_or(120))),
    ))
}

#[cfg(feature = "qdrant")]
fn open_qdrant_backend(config: &MemoryConfig) -> Result<Arc<dyn MemoryBackend>> {
    let url = config
        .qdrant_url
        .clone()
        .or_else(|| env::var("QDRANT_URL").ok())
        .context("Qdrant backend requires qdrant_url in config or QDRANT_URL")?;
    let mut vector_config = QdrantVectorStoreConfig::new(url);
    if let Some(env_name) = &config.qdrant_api_key_env {
        vector_config.api_key = env::var(env_name).ok();
    }
    let store = QdrantVectorStore::connect(vector_config)?;
    Ok(Arc::new(VectorMemoryBackend::new(
        store,
        VectorMemoryConfig::new(&config.collection),
    )?))
}

#[cfg(not(feature = "qdrant"))]
fn open_qdrant_backend(_config: &MemoryConfig) -> Result<Arc<dyn MemoryBackend>> {
    bail!("Qdrant backend requires building brunnr-cli with the qdrant feature")
}

fn sqlite_path(root: &str) -> PathBuf {
    let path = PathBuf::from(root);
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "db" | "sqlite" | "sqlite3"))
    {
        path
    } else {
        path.join("memory.sqlite3")
    }
}
