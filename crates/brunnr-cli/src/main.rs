// SPDX-License-Identifier: Apache-2.0

use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use brunnr_core::{AgentBinding, BrunnrConfig, Role, SpawnRequest};
use clap::{Parser, Subcommand};
use mimisbrunnr::{FilesBackend, MemoryBackend, MemoryQuery, MemoryTier, StoreMemory};

#[derive(Debug, Parser)]
#[command(name = "brunnr", about = "Multi-agent context orchestration")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init {
        #[arg(long, default_value = ".brunnr")]
        memory_root: PathBuf,
    },
    Spawn {
        role: String,
        agent: String,
    },
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    Store {
        content: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        node_id: Option<String>,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
    },
    Find {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        node_id: Option<String>,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { memory_root } => init(memory_root),
        Command::Spawn { role, agent } => spawn(&role, &agent),
        Command::Memory { command } => memory(command).await,
    }
}

fn init(memory_root: PathBuf) -> Result<()> {
    fs::create_dir_all(memory_root.join("memory"))
        .with_context(|| format!("create memory root {}", memory_root.display()))?;
    let agents = detect_agents();
    let config = BrunnrConfig::memory_files(memory_root.display().to_string(), agents);
    let config_path = Path::new("brunnr.toml");
    if config_path.exists() {
        bail!("brunnr.toml already exists");
    }
    fs::write(config_path, config.to_toml()?)?;
    println!(
        "initialized Brunnr memory mode at {}",
        memory_root.display()
    );
    Ok(())
}

fn spawn(role: &str, agent: &str) -> Result<()> {
    let role = Role::from_str(role)?;
    let request = SpawnRequest {
        role,
        agent: agent.to_string(),
        model: None,
        working_dir: env::current_dir()
            .ok()
            .map(|path| path.display().to_string()),
    };
    println!(
        "spawn request accepted: role={} alias={} agent={} cwd={}",
        request.role.canonical_alias(),
        request.role.norse_alias(),
        request.agent,
        request.working_dir.as_deref().unwrap_or(".")
    );
    Ok(())
}

async fn memory(command: MemoryCommand) -> Result<()> {
    match command {
        MemoryCommand::Store {
            content,
            tags,
            node_id,
            root,
        } => {
            let backend = FilesBackend::new(root);
            let record = backend
                .store(StoreMemory {
                    content,
                    tags,
                    metadata: Default::default(),
                    tier: MemoryTier::L1Atom,
                    node_id,
                })
                .await?;
            println!("stored memory id={} node_id={}", record.id, record.node_id);
        }
        MemoryCommand::Find {
            query,
            limit,
            node_id,
            root,
        } => {
            let backend = FilesBackend::new(root);
            let mut memory_query = MemoryQuery::new(query).with_limit(limit);
            memory_query.node_id = node_id;
            for hit in backend.find(memory_query).await? {
                println!(
                    "{:.4}\t{}\t{}\t{}",
                    hit.score, hit.record.id, hit.record.node_id, hit.record.content
                );
            }
        }
    }
    Ok(())
}

fn detect_agents() -> Vec<AgentBinding> {
    let detected = [
        "claude",
        "claude-code",
        "codex",
        "gemini",
        "opencode",
        "ollama",
    ]
    .into_iter()
    .filter(|name| command_exists(name))
    .map(str::to_string)
    .collect::<Vec<_>>();

    let master = pick(&detected, &["claude-code", "claude", "codex"]);
    let worker = pick(&detected, &["codex", "opencode", "claude"]);
    let judge = pick(&detected, &["claude-code", "claude", "gemini", "codex"]);

    [
        (Role::Master, master),
        (Role::Worker, worker),
        (Role::Judge, judge),
    ]
    .into_iter()
    .filter_map(|(role, agent)| {
        agent.map(|agent| AgentBinding {
            role,
            agent,
            model: None,
        })
    })
    .collect()
}

fn pick(detected: &[String], preferred: &[&str]) -> Option<String> {
    preferred
        .iter()
        .find_map(|candidate| detected.iter().find(|agent| agent == candidate).cloned())
}

fn command_exists(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| dir.join(command).is_file())
}
