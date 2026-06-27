// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "llm")]

//! Council qualify-gate: a panel of N parallel judges + an arbiter that reconciles verdicts.
//!
//! ## Design
//!
//! The "council decides, a cheaper agent executes" pattern:
//! 1. All panel judges run concurrently (`tokio::join_all`) and each returns a [`JudgeVerdict`].
//! 2. The arbiter receives all verdicts as a JSON array and produces the final decision.
//! 3. If the arbiter is unreachable or returns garbage, the gate falls back to **majority vote**
//!    across the panel verdicts using the same thresholds as [`JudgeQualifyGate`].
//! 4. If fewer than half the panel judges respond, the gate **fails closed** (rejects).
//!
//! The arbiter itself is an [`LlmClient`] — it can be a cheap/fast model (e.g. a local Ollama
//! instance with a 1B model) while the panel uses larger or more diverse models.

use std::sync::Arc;

use futures_util::{future::BoxFuture, FutureExt};
use serde_json::json;

use crate::{
    judge::{parse_verdict_pub, JudgeVerdict},
    CommittedContextState, LlmClient, LlmRequest, QualifyAudit, QualifyDecision, QualifyGate,
    QualifySignal, RecallItem,
};

const PANEL_SYSTEM: &str = "You are a memory-control judge for an AI agent. Score whether a \
candidate fact should enter the agent's bounded committed context. Reply with ONLY a compact JSON \
object, no prose, no code fences. Schema: {\"relevance\": <0..1>, \"novelty\": <0..1>, \"drift\": \
<0..1>, \"slot\": <string>, \"reason\": <short string>}.";

const ARBITER_SYSTEM: &str = "You are a memory-control arbiter. You receive an array of judge \
verdicts and must synthesize a final verdict. Reply with ONLY a compact JSON object matching the \
same schema as the inputs: {\"relevance\": <0..1>, \"novelty\": <0..1>, \"drift\": <0..1>, \
\"slot\": <string>, \"reason\": <short string>}. Weigh each judge's evidence; you may disagree \
with the majority if you have a principled reason, but briefly state it in 'reason'.";

/// A qualify-gate that runs N panel judges concurrently and uses an arbiter to synthesize.
///
/// Cheaper + more robust than a single judge: diverse panel catches hallucinations one model
/// misses; the arbiter produces a principled final verdict rather than a raw majority.
pub struct CouncilJudge {
    panel: Vec<Arc<dyn LlmClient>>,
    arbiter: Arc<dyn LlmClient>,
    min_relevance: f32,
    min_novelty: f32,
    max_drift: f32,
}

impl CouncilJudge {
    /// Create a council with `panel` judges and an `arbiter`.
    ///
    /// `panel` must be non-empty. The `arbiter` need not be in the panel — it can be a
    /// cheaper/faster model that only sees the pre-scored verdicts.
    pub fn new(panel: Vec<Arc<dyn LlmClient>>, arbiter: Arc<dyn LlmClient>) -> Self {
        assert!(
            !panel.is_empty(),
            "CouncilJudge requires at least one panel judge"
        );
        Self {
            panel,
            arbiter,
            min_relevance: 0.5,
            min_novelty: 0.3,
            max_drift: 0.4,
        }
    }

    pub fn with_thresholds(mut self, min_relevance: f32, min_novelty: f32, max_drift: f32) -> Self {
        self.min_relevance = min_relevance;
        self.min_novelty = min_novelty;
        self.max_drift = max_drift;
        self
    }

    fn panel_prompt(item: &RecallItem, ccs: &CommittedContextState) -> String {
        let committed = ccs.render();
        let committed = if committed.is_empty() {
            "(empty)".to_string()
        } else {
            committed
        };
        format!(
            "Committed context so far:\n{committed}\n\nCandidate fact (id={id}, score {score:.3}):\n{content}\n\nScore the candidate.",
            id = item.id,
            score = item.score,
            content = item.content,
        )
    }

    fn decide(&self, verdict: &JudgeVerdict, ccs: &CommittedContextState) -> QualifyDecision {
        let reason = verdict.reason.clone().unwrap_or_default();
        let signals = self.audit_signals(verdict);
        if verdict.relevance < self.min_relevance {
            return QualifyDecision::reject(
                format!("council: low relevance {:.2} ({reason})", verdict.relevance),
                verdict.relevance,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        if verdict.novelty < self.min_novelty {
            return QualifyDecision::reject(
                format!(
                    "council: redundant, novelty {:.2} ({reason})",
                    verdict.novelty
                ),
                verdict.relevance,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        if verdict.drift > self.max_drift {
            return QualifyDecision::reject(
                format!(
                    "council: drift {:.2} exceeds {:.2} ({reason})",
                    verdict.drift, self.max_drift
                ),
                verdict.relevance,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        let slot = verdict
            .slot
            .clone()
            .filter(|s| ccs.schema().contains(s))
            .unwrap_or_else(|| ccs.schema().default_slot().to_string());
        QualifyDecision::admit(slot, verdict.relevance)
            .with_audit(QualifyAudit::from_signals(true, signals))
    }

    fn audit_signals(&self, verdict: &JudgeVerdict) -> Vec<QualifySignal> {
        vec![
            QualifySignal::new(
                "relevance",
                verdict.relevance,
                self.min_relevance,
                verdict.relevance >= self.min_relevance,
            ),
            QualifySignal::new(
                "novelty",
                verdict.novelty,
                self.min_novelty,
                verdict.novelty >= self.min_novelty,
            ),
            QualifySignal::new(
                "drift",
                verdict.drift,
                self.max_drift,
                verdict.drift <= self.max_drift,
            ),
        ]
    }

    fn majority_verdict(verdicts: &[JudgeVerdict]) -> Option<JudgeVerdict> {
        if verdicts.is_empty() {
            return None;
        }
        let n = verdicts.len() as f32;
        let avg_relevance = verdicts.iter().map(|v| v.relevance).sum::<f32>() / n;
        let avg_novelty = verdicts.iter().map(|v| v.novelty).sum::<f32>() / n;
        let avg_drift = verdicts.iter().map(|v| v.drift).sum::<f32>() / n;
        let slot = verdicts
            .iter()
            .filter_map(|v| v.slot.clone())
            .next()
            .unwrap_or_default();
        let reason = format!(
            "majority of {} panel judges (avg relevance={:.2}, novelty={:.2}, drift={:.2})",
            verdicts.len(),
            avg_relevance,
            avg_novelty,
            avg_drift,
        );
        Some(JudgeVerdict {
            relevance: avg_relevance,
            novelty: avg_novelty,
            drift: avg_drift,
            slot: Some(slot),
            reason: Some(reason),
        })
    }
}

impl QualifyGate for CouncilJudge {
    fn qualify<'a>(
        &'a self,
        item: &'a RecallItem,
        ccs: &'a CommittedContextState,
    ) -> BoxFuture<'a, QualifyDecision> {
        async move {
            let prompt = Self::panel_prompt(item, ccs);

            // Run all panel judges concurrently.
            let panel_futures: Vec<_> = self
                .panel
                .iter()
                .map(|client| {
                    let req = LlmRequest::new(prompt.clone())
                        .with_system(PANEL_SYSTEM)
                        .with_temperature(0.0)
                        .with_max_tokens(200);
                    client.complete(req)
                })
                .collect();
            let panel_results = futures_util::future::join_all(panel_futures).await;

            let verdicts: Vec<JudgeVerdict> = panel_results
                .into_iter()
                .filter_map(|r| r.ok())
                .filter_map(|text| parse_verdict_pub(&text))
                .collect();

            // Fail closed if fewer than half the panel responded.
            let quorum = self.panel.len().div_ceil(2);
            if verdicts.len() < quorum {
                return QualifyDecision::reject(
                    format!(
                        "council: only {}/{} panel judges responded (quorum {})",
                        verdicts.len(),
                        self.panel.len(),
                        quorum
                    ),
                    item.score,
                );
            }

            // Ask the arbiter to synthesize.
            let verdicts_json = json!(verdicts).to_string();
            let arbiter_prompt = format!(
                "Panel verdicts for candidate (id={id}):\n{verdicts_json}\n\nSynthesize a final verdict.",
                id = item.id,
            );
            let arbiter_req = LlmRequest::new(arbiter_prompt)
                .with_system(ARBITER_SYSTEM)
                .with_temperature(0.0)
                .with_max_tokens(200);

            let final_verdict = match self.arbiter.complete(arbiter_req).await {
                Ok(text) => parse_verdict_pub(&text).or_else(|| Self::majority_verdict(&verdicts)),
                Err(_) => Self::majority_verdict(&verdicts),
            };

            match final_verdict {
                Some(verdict) => self.decide(&verdict, ccs),
                None => {
                    QualifyDecision::reject("council: no verdict produced".to_string(), item.score)
                }
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CcsSchema, CommittedContextState, StaticLlmClient};

    fn ccs() -> CommittedContextState {
        CommittedContextState::new(CcsSchema::default(), 4096)
    }

    fn high_score_client() -> Arc<dyn LlmClient> {
        Arc::new(StaticLlmClient::new(
            r#"{"relevance":0.9,"novelty":0.8,"drift":0.1,"slot":"decision","reason":"ok"}"#,
        ))
    }

    fn low_score_client() -> Arc<dyn LlmClient> {
        Arc::new(StaticLlmClient::new(
            r#"{"relevance":0.2,"novelty":0.1,"drift":0.7,"slot":"decision","reason":"bad"}"#,
        ))
    }

    #[tokio::test]
    async fn council_admits_when_panel_and_arbiter_agree_high() {
        let panel = vec![high_score_client(), high_score_client()];
        let arbiter = high_score_client();
        let gate = CouncilJudge::new(panel, arbiter);
        let item = RecallItem::new("x", "the team chose Rust", 1.0);
        let decision = gate.qualify(&item, &ccs()).await;
        assert!(decision.admitted, "should admit high-scoring candidate");
    }

    #[tokio::test]
    async fn council_rejects_when_panel_and_arbiter_agree_low() {
        let panel = vec![low_score_client(), low_score_client()];
        let arbiter = low_score_client();
        let gate = CouncilJudge::new(panel, arbiter);
        let item = RecallItem::new("x", "irrelevant noise", 0.1);
        let decision = gate.qualify(&item, &ccs()).await;
        assert!(!decision.admitted, "should reject low-scoring candidate");
    }

    #[tokio::test]
    async fn council_falls_back_to_majority_when_arbiter_returns_garbage() {
        let panel = vec![high_score_client(), high_score_client()];
        let arbiter = Arc::new(StaticLlmClient::new("I cannot do that.")) as Arc<dyn LlmClient>;
        let gate = CouncilJudge::new(panel, arbiter);
        let item = RecallItem::new("x", "the team chose Rust", 1.0);
        let decision = gate.qualify(&item, &ccs()).await;
        // Panel majority says high → should admit despite arbiter failure.
        assert!(decision.admitted, "should admit via majority fallback");
    }

    #[tokio::test]
    async fn council_fails_closed_below_quorum() {
        let failing = Arc::new(StaticLlmClient::new("")) as Arc<dyn LlmClient>;
        let panel = vec![failing.clone(), failing.clone(), failing];
        let arbiter = high_score_client();
        // Empty responses won't parse as valid verdicts → quorum not reached.
        let gate = CouncilJudge::new(panel, arbiter);
        let item = RecallItem::new("x", "test", 1.0);
        let decision = gate.qualify(&item, &ccs()).await;
        assert!(!decision.admitted, "should fail closed below quorum");
    }
}
