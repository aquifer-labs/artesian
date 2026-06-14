// SPDX-License-Identifier: Apache-2.0

use std::{env, path::PathBuf, time::Duration};

use anyhow::{bail, Context, Result};
use brunnr_core::Mode;
use clap::Parser;
use serde_json::json;

mod runtime;
use runtime::{build_orchestrator, load_config};

const DEFAULT_CONFIG: &str = "brunnr.toml";

#[derive(Debug, Parser)]
#[command(name = "brunnrd", about = "Brunnr orchestration daemon")]
struct Cli {
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    once: bool,
    #[arg(long, default_value_t = 1000)]
    interval_millis: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config(&cli.config)?;
    if !matches!(config.mode, Mode::Orchestrate | Mode::Full) {
        bail!(
            "brunnrd requires mode orchestrate or full, got {:?}",
            config.mode
        );
    }
    let root = cli
        .root
        .unwrap_or_else(|| PathBuf::from(&config.memory.root));
    let repo_root = env::current_dir()?;
    let mut orchestrator = build_orchestrator(config, root, repo_root, cli.dry_run)?;

    loop {
        let report = orchestrator.run_once().await?;
        println!(
            "{}",
            serde_json::to_string(&json!({
                "dispatched": report.dispatched,
                "completed": report.completed,
                "blocked": report.blocked,
                "idle": report.idle,
                "events": orchestrator.run_log().events.len()
            }))?
        );
        if cli.once {
            break;
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("listen for ctrl-c")?;
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(cli.interval_millis)) => {}
        }
    }
    Ok(())
}
