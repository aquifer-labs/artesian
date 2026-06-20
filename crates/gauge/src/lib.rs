// SPDX-License-Identifier: Apache-2.0

//! Gauge — Artesian observability: the ACC control-quality benchmark (drift / hallucination /
//! footprint), QA recall eval (LoCoMo / LongMemEval), and agentic task eval (memory-guides-action).

pub mod agentic;
pub mod bench;
pub mod eval;

pub use agentic::{load_agent_tasks, AgentTask, ScaleLane, TaskSession};
#[cfg(feature = "llm")]
pub use agentic::{run_agent_task, run_agentic_eval, AgentTaskOutcome, AgenticEvalSummary};
pub use bench::{
    demo_case, render_markdown, run_bench, run_default_arm, BenchCase, BenchResult, FactLabel,
    LabeledFact,
};
#[cfg(all(feature = "llm", feature = "vector"))]
pub use eval::VectorRecall;
pub use eval::{load_locomo, load_longmemeval, LoadReport, QaCase};
#[cfg(feature = "llm")]
pub use eval::{
    run_case, run_qa_eval, CaseOutcome, EvalSummary, ExpandingRecall, ExpandingRecallStore,
    LexicalRecall, RecallFactory,
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
