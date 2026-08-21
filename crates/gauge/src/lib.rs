// SPDX-License-Identifier: Apache-2.0

//! Gauge — Artesian observability: the ACC control-quality benchmark (drift / hallucination /
//! footprint), QA recall eval (LoCoMo / LongMemEval), and agentic task eval (memory-guides-action).

pub mod agentic;
pub mod bench;
pub mod eval;
#[cfg(feature = "ci-eval")]
pub mod retrieval_regression;

pub use agentic::{AgentTask, ScaleLane, TaskSession, load_agent_tasks};
#[cfg(feature = "llm")]
pub use agentic::{AgentTaskOutcome, AgenticEvalSummary, run_agent_task, run_agentic_eval};
pub use bench::{
    BenchCase, BenchResult, FactLabel, LabeledFact, demo_case, render_markdown, run_bench,
    run_default_arm,
};
#[cfg(all(feature = "llm", feature = "vector"))]
pub use eval::VectorRecall;
#[cfg(feature = "llm")]
pub use eval::{
    CaseOutcome, EvalSummary, ExpandingRecall, ExpandingRecallStore, LexicalRecall, RecallFactory,
    run_case, run_qa_eval,
};
pub use eval::{LoadReport, QaCase, load_locomo, load_longmemeval};
#[cfg(feature = "ci-eval")]
pub use retrieval_regression::{
    BackendMetrics, BaselineComparison, CaseMetrics, DEFAULT_K, DEFAULT_TOLERANCE, LeakGateReport,
    RegressionReport, compare_to_baseline, load_report, render_regression_markdown,
    run_regression_suite, write_report,
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
