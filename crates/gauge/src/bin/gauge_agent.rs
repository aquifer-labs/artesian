// SPDX-License-Identifier: Apache-2.0

//! `gauge-agent` — run the agentic task eval: **memory-guides-action**, not just recall.
//!
//! Requires building with `--features llm`. The action-choice LLM is reached through a command
//! (default `benchmarks/comparison/codex-complete`, wrapping `codex exec`).
//!
//! ## Protocol
//! Sessions are replayed in order; each session's facts are accumulated in memory. After all
//! sessions the LLM is asked to choose the correct next action from a presented set. Success =
//! picking the action the accumulated evidence supports.
//!
//! ## Usage
//!   gauge-agent <fixture.json> [--limit N] [--llm-command CMD] [--json]
//!
//! The fixture is a JSON array (or NDJSON) of `AgentTask` objects — see
//! `benchmarks/comparison/samples/agent-smoke.json` for the format.

#[cfg(not(feature = "llm"))]
fn main() {
    eprintln!("gauge-agent requires building gauge with --features llm");
    std::process::exit(2);
}

#[cfg(feature = "llm")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use gauge::{load_agent_tasks, run_agentic_eval};
    use headgate::{CommandLlmClient, HeadgateConfig, LlmClient};
    use std::sync::Arc;

    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_default();
    if path.is_empty() {
        eprintln!("usage: gauge-agent <fixture.json> [--limit N] [--llm-command CMD] [--json]");
        std::process::exit(2);
    }

    let mut limit: Option<usize> = None;
    let mut llm_command = "benchmarks/comparison/codex-complete".to_string();
    let mut json_out = false;
    let rest: Vec<String> = args.collect();
    let mut iter = rest.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--limit" => limit = iter.next().and_then(|v| v.parse().ok()),
            "--llm-command" => {
                if let Some(value) = iter.next() {
                    llm_command = value.clone();
                }
            }
            "--json" => json_out = true,
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }

    let raw = std::fs::read_to_string(&path)?;
    let mut tasks = load_agent_tasks(&raw)?;
    if let Some(limit) = limit {
        tasks.truncate(limit);
    }
    eprintln!(
        "loaded {} agentic tasks from {path}; running...",
        tasks.len()
    );

    let client: Arc<dyn LlmClient> = Arc::new(CommandLlmClient::new(llm_command, Vec::new()));
    let config = HeadgateConfig::default();

    let dataset = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("agentic")
        .to_string();
    let (summary, _outcomes) = run_agentic_eval(dataset, &tasks, client.as_ref(), config).await;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("dataset:               {}", summary.dataset);
        println!("tasks:                 {}", summary.tasks);
        println!("graded:                {}", summary.graded);
        println!("accuracy:              {:.3}", summary.accuracy);
        println!(
            "mean tokens/query:     {:.1}",
            summary.mean_committed_tokens
        );
    }
    Ok(())
}
