// SPDX-License-Identifier: Apache-2.0

//! `gauge-ci-eval` - deterministic retrieval and partition regression gate for CI.
//!
//! Usage:
//!   gauge-ci-eval [--baseline PATH] [--summary-dir DIR] [--json]
//!                 [--k N] [--tolerance FLOAT] [--skip-baseline]

#[cfg(not(feature = "ci-eval"))]
fn main() {
    eprintln!("gauge-ci-eval requires building gauge with --features ci-eval");
    std::process::exit(2);
}

#[cfg(feature = "ci-eval")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use gauge::{
        compare_to_baseline, load_report, render_regression_markdown, run_regression_suite,
        write_report, DEFAULT_K, DEFAULT_TOLERANCE,
    };
    use std::path::PathBuf;

    let default_baseline = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benchmarks")
        .join("retrieval")
        .join("baseline.json");
    let mut baseline_path = default_baseline;
    let mut summary_dir: Option<PathBuf> = None;
    let mut json = false;
    let mut skip_baseline = false;
    let mut requested_k: Option<usize> = None;
    let mut requested_tolerance: Option<f32> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--baseline" => {
                let Some(path) = iter.next() else {
                    eprintln!("--baseline requires a path");
                    std::process::exit(2);
                };
                baseline_path = PathBuf::from(path);
            }
            "--summary-dir" => {
                let Some(path) = iter.next() else {
                    eprintln!("--summary-dir requires a path");
                    std::process::exit(2);
                };
                summary_dir = Some(PathBuf::from(path));
            }
            "--json" => json = true,
            "--skip-baseline" => skip_baseline = true,
            "--k" => requested_k = iter.next().and_then(|value| value.parse().ok()),
            "--tolerance" => {
                requested_tolerance = iter.next().and_then(|value| value.parse().ok());
            }
            other => {
                eprintln!("unknown flag: {other}");
                eprintln!(
                    "usage: gauge-ci-eval [--baseline PATH] [--summary-dir DIR] [--json] \
[--k N] [--tolerance FLOAT] [--skip-baseline]"
                );
                std::process::exit(2);
            }
        }
    }

    let baseline = if skip_baseline {
        None
    } else {
        Some(load_report(&baseline_path)?)
    };
    let k = requested_k
        .or_else(|| baseline.as_ref().map(|report| report.k))
        .unwrap_or(DEFAULT_K);
    let tolerance = requested_tolerance
        .or_else(|| baseline.as_ref().map(|report| report.tolerance))
        .unwrap_or(DEFAULT_TOLERANCE);

    let report = run_regression_suite(k, tolerance).await?;
    let comparison = baseline
        .as_ref()
        .map(|baseline| compare_to_baseline(&report, baseline));
    let markdown = render_regression_markdown(&report, comparison.as_ref());

    if let Some(summary_dir) = summary_dir {
        std::fs::create_dir_all(&summary_dir)?;
        write_report(summary_dir.join("retrieval-regression.json"), &report)?;
        std::fs::write(summary_dir.join("retrieval-regression.md"), &markdown)?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{markdown}");
    }

    if let Some(comparison) = comparison {
        if !comparison.passed {
            std::process::exit(1);
        }
    }
    Ok(())
}
