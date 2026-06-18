// SPDX-License-Identifier: Apache-2.0

//! Gauge — Artesian observability: the ACC control-quality benchmark (drift / hallucination /
//! footprint) plus a TUI status placeholder.

pub mod bench;

pub use bench::{
    demo_case, render_markdown, run_bench, run_default_arm, BenchCase, BenchResult, FactLabel,
    LabeledFact,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuiStatus {
    pub mode: String,
    pub backend: String,
}

impl TuiStatus {
    pub fn memory_files() -> Self {
        Self {
            mode: "memory".to_string(),
            backend: "files".to_string(),
        }
    }
}
