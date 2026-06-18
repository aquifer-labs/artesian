// SPDX-License-Identifier: Apache-2.0

//! `gauge-bench` — run the deterministic ACC control-quality benchmark and print a report.
//!
//! Usage:
//!   gauge-bench           # markdown report for the built-in demo case
//!   gauge-bench --json    # machine-readable JSON

use gauge::{demo_case, render_markdown, run_default_arm};
use headgate::HeadgateConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let json = std::env::args().any(|arg| arg == "--json");

    let case = demo_case();
    let result = run_default_arm(&case, HeadgateConfig::default()).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("# ACC control-quality benchmark (demo case)\n");
        println!("Query: {}\n", case.query);
        println!("{}", render_markdown(&result));
        println!(
            "Interpretation: footprint_ratio < 1 means the committed context is smaller than \
the raw recall dump; drift_rate / hallucination_rate are the fraction of admitted facts that \
contradict gold or are fabricated. Run with the LLM judge gate (feature `llm`) to drive those \
toward zero."
        );
    }
    Ok(())
}
