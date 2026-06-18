// SPDX-License-Identifier: Apache-2.0

//! ACC control-quality benchmark.
//!
//! The benchmark measures what a *memory controller* — not just a retriever — is supposed to
//! get right. Over a labeled set of recall candidates it runs one ACC cycle and reports:
//!
//! - **footprint** — committed tokens vs. the raw recall dump (the moat-#1 token-efficiency
//!   number, deterministic);
//! - **precision / recall** — of the gold (relevant, true) facts;
//! - **drift rate** — fraction of admitted facts that contradict a gold fact;
//! - **hallucination rate** — fraction of admitted facts that are unsupported/fabricated.
//!
//! Footprint and the label-based rates are fully deterministic, so the benchmark runs in CI
//! with the default gate. The LLM judge gate ([`headgate::JudgeQualifyGate`], feature `llm`)
//! is what *reduces* drift and hallucination; pass it to [`run_bench`] to measure that gain.
//!
//! ## Competitor-comparable framing
//!
//! The metrics are chosen to line up with the agent-memory literature: `footprint_tokens` is
//! directly comparable to mem0's reported tokens/query, and precision/recall map onto
//! LoCoMo / LongMemEval scoring. To benchmark a real dataset, load each turn's candidate facts
//! and gold/contradiction labels into [`BenchCase`] (see [`demo_case`] for the shape) and
//! aggregate [`BenchResult`] across cases.

use std::collections::HashMap;
use std::sync::Arc;

use headgate::{
    count_tokens, DefaultQualifyGate, Headgate, HeadgateConfig, HeadgateResult, QualifyGate,
    RecallItem, StaticRecallStore,
};
use serde::{Deserialize, Serialize};

/// Ground-truth label for a candidate fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactLabel {
    /// Relevant and true — admitting it is correct.
    Gold,
    /// Irrelevant noise — admitting it wastes footprint.
    Distractor,
    /// Contradicts a gold fact — admitting it is drift.
    Contradiction,
    /// Unsupported / fabricated — admitting it is hallucination.
    Fabrication,
}

/// A labeled recall candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabeledFact {
    pub id: String,
    pub content: String,
    pub label: FactLabel,
    /// Backend-relative retrieval score the recall store would assign.
    pub score: f32,
}

impl LabeledFact {
    pub fn new(
        id: impl Into<String>,
        content: impl Into<String>,
        label: FactLabel,
        score: f32,
    ) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            label,
            score,
        }
    }
}

/// One query plus its labeled candidate facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchCase {
    pub query: String,
    pub facts: Vec<LabeledFact>,
}

/// Metrics for one arm (one gate) over one or more cases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchResult {
    pub arm: String,
    pub candidates: usize,
    pub admitted: usize,
    pub admitted_gold: usize,
    pub total_gold: usize,
    pub raw_recall_tokens: usize,
    pub footprint_tokens: usize,
    pub precision: f32,
    pub recall: f32,
    pub drift_rate: f32,
    pub hallucination_rate: f32,
    /// Committed footprint as a fraction of the raw recall dump (lower is better).
    pub footprint_ratio: f32,
}

fn ratio(numerator: usize, denominator: usize) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f32 / denominator as f32
    }
}

/// Run one ACC cycle for `case` with `gate` and score the committed state against the labels.
pub async fn run_bench(
    arm: impl Into<String>,
    case: &BenchCase,
    gate: Arc<dyn QualifyGate>,
    config: HeadgateConfig,
) -> HeadgateResult<BenchResult> {
    let labels: HashMap<&str, FactLabel> = case
        .facts
        .iter()
        .map(|fact| (fact.id.as_str(), fact.label))
        .collect();
    let raw_recall_tokens = case
        .facts
        .iter()
        .map(|fact| count_tokens(&fact.content))
        .sum();
    let total_gold = case
        .facts
        .iter()
        .filter(|fact| fact.label == FactLabel::Gold)
        .count();

    let items: Vec<RecallItem> = case
        .facts
        .iter()
        .map(|fact| RecallItem::new(fact.id.clone(), fact.content.clone(), fact.score))
        .collect();
    let recall = Arc::new(StaticRecallStore::new(items));
    let mut headgate = Headgate::new(recall, config).with_gate(gate);
    let metrics = headgate.cycle(&case.query).await?;

    let mut admitted_gold = 0usize;
    let mut admitted_contradiction = 0usize;
    let mut admitted_fabrication = 0usize;
    for entry in headgate.ccs().entries() {
        match labels.get(entry.id.as_str()) {
            Some(FactLabel::Gold) => admitted_gold += 1,
            Some(FactLabel::Contradiction) => admitted_contradiction += 1,
            Some(FactLabel::Fabrication) => admitted_fabrication += 1,
            _ => {}
        }
    }

    Ok(BenchResult {
        arm: arm.into(),
        candidates: metrics.candidates,
        admitted: metrics.admitted,
        admitted_gold,
        total_gold,
        raw_recall_tokens,
        footprint_tokens: metrics.footprint_tokens,
        precision: ratio(admitted_gold, metrics.admitted),
        recall: ratio(admitted_gold, total_gold),
        drift_rate: ratio(admitted_contradiction, metrics.admitted),
        hallucination_rate: ratio(admitted_fabrication, metrics.admitted),
        footprint_ratio: ratio(metrics.footprint_tokens, raw_recall_tokens),
    })
}

/// Convenience: run the deterministic [`DefaultQualifyGate`] arm.
pub async fn run_default_arm(
    case: &BenchCase,
    config: HeadgateConfig,
) -> HeadgateResult<BenchResult> {
    let gate = Arc::new(DefaultQualifyGate::new(
        config.min_score,
        config.redundancy_threshold,
    ));
    run_bench("default-gate", case, gate, config).await
}

/// A small, self-contained labeled case: one gold decision, a redundant restatement, a
/// distractor, a contradiction, and a fabrication — enough to exercise every metric.
pub fn demo_case() -> BenchCase {
    BenchCase {
        query: "what language and deployment did the team choose".to_string(),
        facts: vec![
            LabeledFact::new(
                "g1",
                "the team chose Rust for the core crates",
                FactLabel::Gold,
                3.0,
            ),
            LabeledFact::new(
                "g2",
                "deployments run nightly on the kubernetes cluster",
                FactLabel::Gold,
                2.0,
            ),
            LabeledFact::new(
                "r1",
                "the team chose Rust for the core crates",
                FactLabel::Distractor,
                1.5,
            ),
            LabeledFact::new(
                "d1",
                "the office coffee machine was replaced in March",
                FactLabel::Distractor,
                1.0,
            ),
            LabeledFact::new(
                "c1",
                "the team chose Go for the core crates",
                FactLabel::Contradiction,
                2.5,
            ),
            LabeledFact::new(
                "f1",
                "the team deployed to a quantum datacenter on Mars",
                FactLabel::Fabrication,
                1.2,
            ),
        ],
    }
}

/// Render a [`BenchResult`] as a compact markdown block.
pub fn render_markdown(result: &BenchResult) -> String {
    format!(
        "### arm: {arm}\n\
         | metric | value |\n|---|---|\n\
         | candidates | {candidates} |\n\
         | admitted | {admitted} |\n\
         | precision | {precision:.3} |\n\
         | recall | {recall:.3} |\n\
         | drift_rate | {drift:.3} |\n\
         | hallucination_rate | {halluc:.3} |\n\
         | footprint_tokens | {footprint} |\n\
         | raw_recall_tokens | {raw} |\n\
         | footprint_ratio | {ratio:.3} |\n",
        arm = result.arm,
        candidates = result.candidates,
        admitted = result.admitted,
        precision = result.precision,
        recall = result.recall,
        drift = result.drift_rate,
        halluc = result.hallucination_rate,
        footprint = result.footprint_tokens,
        raw = result.raw_recall_tokens,
        ratio = result.footprint_ratio,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_gate_admits_gold_and_bounds_footprint() {
        let case = demo_case();
        let result = run_default_arm(&case, HeadgateConfig::default())
            .await
            .expect("bench runs");

        assert!(result.admitted_gold >= 1, "at least one gold fact admitted");
        assert!(result.total_gold == 2);
        // The redundant restatement of g1 should be rejected, so footprint < raw recall.
        assert!(
            result.footprint_ratio < 1.0,
            "committed footprint is smaller than the raw recall dump: {}",
            result.footprint_ratio
        );
        assert!(result.precision > 0.0);
    }

    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn judge_gate_eliminates_the_admitted_fabrication() {
        use futures_util::{future::BoxFuture, FutureExt};
        use headgate::{HeadgateResult, JudgeQualifyGate, LlmClient, LlmRequest};

        // A scripted judge: flags content mentioning Mars/quantum as high-drift fabrication,
        // everything else as clean. (Stands in for a real LLM judge.)
        struct ScriptedJudge;
        impl LlmClient for ScriptedJudge {
            fn complete(&self, request: LlmRequest) -> BoxFuture<'_, HeadgateResult<String>> {
                let prompt = request.prompt.to_lowercase();
                let verdict = if prompt.contains("mars") || prompt.contains("quantum") {
                    "{\"relevance\":0.9,\"novelty\":0.9,\"drift\":0.95,\"reason\":\"fabricated\"}"
                } else {
                    "{\"relevance\":0.9,\"novelty\":0.8,\"drift\":0.1,\"reason\":\"ok\"}"
                };
                let verdict = verdict.to_string();
                async move { Ok(verdict) }.boxed()
            }
        }

        let case = demo_case();
        let gate = Arc::new(JudgeQualifyGate::new(Arc::new(ScriptedJudge)));
        let result = run_bench("judge-gate", &case, gate, HeadgateConfig::default())
            .await
            .expect("bench runs");

        assert_eq!(
            result.hallucination_rate, 0.0,
            "the judge rejects the fabrication"
        );
        assert!(result.admitted_gold >= 1);
    }

    #[tokio::test]
    async fn default_gate_does_not_invent_metrics_on_empty_admits() {
        let case = BenchCase {
            query: "nothing relevant".to_string(),
            facts: vec![LabeledFact::new(
                "d",
                "irrelevant",
                FactLabel::Distractor,
                0.0,
            )],
        };
        // min_score above the distractor's score => nothing admitted.
        let config = HeadgateConfig {
            min_score: 0.5,
            ..HeadgateConfig::default()
        };
        let result = run_default_arm(&case, config).await.expect("bench runs");
        assert_eq!(result.admitted, 0);
        assert_eq!(result.precision, 0.0);
        assert_eq!(result.footprint_ratio, 0.0);
    }
}
