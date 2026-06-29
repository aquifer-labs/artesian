// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "llm")]

//! LLM judge-eval qualify-gate.
//!
//! [`JudgeQualifyGate`] is the ACC trust boundary backed by an LLM judge. For each recall
//! candidate it asks the judge to score three dimensions against the current committed state —
//! **relevance**, **novelty** (anti-redundancy), and **drift** (contradiction / hallucination
//! risk) — then applies deterministic thresholds. The gate, not the model, decides admission,
//! so the thresholds are auditable; if the judge is unreachable or returns garbage the gate
//! fails closed (rejects).

use std::sync::Arc;

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};

use crate::{
    count_tokens, CommittedContextState, JudgeTokenCost, LlmClient, LlmRequest, QualifyAudit,
    QualifyDecision, QualifyGate, QualifySignal, RecallItem,
};

const JUDGE_SYSTEM: &str = "You are a memory-control judge for an AI agent. You score whether a \
candidate fact should enter the agent's bounded committed context. Reply with ONLY a compact JSON \
object, no prose, no code fences. Schema: {\"relevance\": <0..1>, \"novelty\": <0..1>, \"drift\": \
<0..1>, \"slot\": <string>, \"reason\": <short string>}. relevance = how useful the candidate is to \
the agent's work; novelty = how much new information it adds versus what is already committed \
(1 = wholly new, 0 = duplicate); drift = risk it contradicts the committed state or is \
unsupported/hallucinated (0 = fully consistent, 1 = contradictory).";

/// The judge's parsed verdict for one candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeVerdict {
    pub relevance: f32,
    pub novelty: f32,
    pub drift: f32,
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Qualify-gate that consults an LLM judge and applies deterministic thresholds.
pub struct JudgeQualifyGate {
    client: Arc<dyn LlmClient>,
    min_relevance: f32,
    min_novelty: f32,
    max_drift: f32,
}

impl JudgeQualifyGate {
    pub fn new(client: Arc<dyn LlmClient>) -> Self {
        Self {
            client,
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

    fn prompt(item: &RecallItem, ccs: &CommittedContextState) -> String {
        let committed = ccs.render();
        let committed = if committed.is_empty() {
            "(empty)".to_string()
        } else {
            committed
        };
        format!(
            "Committed context so far:\n{committed}\n\nCandidate fact (id={id}, retrieval score \
{score:.3}):\n{content}\n\nScore the candidate.",
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
                format!("judge: low relevance {:.2} ({reason})", verdict.relevance),
                verdict.relevance,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        if verdict.novelty < self.min_novelty {
            return QualifyDecision::reject(
                format!(
                    "judge: redundant, novelty {:.2} ({reason})",
                    verdict.novelty
                ),
                verdict.relevance,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        if verdict.drift > self.max_drift {
            return QualifyDecision::reject(
                format!(
                    "judge: drift {:.2} exceeds {:.2} ({reason})",
                    verdict.drift, self.max_drift
                ),
                verdict.relevance,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        let slot = verdict
            .slot
            .clone()
            .filter(|slot| ccs.schema().contains(slot))
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
}

impl QualifyGate for JudgeQualifyGate {
    fn qualify<'a>(
        &'a self,
        item: &'a RecallItem,
        ccs: &'a CommittedContextState,
    ) -> BoxFuture<'a, QualifyDecision> {
        async move {
            let prompt = Self::prompt(item, ccs);
            let prompt_tokens = count_tokens(JUDGE_SYSTEM).saturating_add(count_tokens(&prompt));
            let judge_label = self.client.label().to_string();
            let request = LlmRequest::new(prompt)
                .with_system(JUDGE_SYSTEM)
                .with_temperature(0.0)
                .with_max_tokens(200);
            let raw = match self.client.complete(request).await {
                Ok(text) => text,
                Err(error) => {
                    // Fail closed: an unreachable judge must not silently admit.
                    return QualifyDecision::reject(
                        format!("judge unavailable: {error}"),
                        item.score,
                    )
                    .with_audit(
                        QualifyAudit::from_signals(false, Vec::new())
                            .with_judge_cost(JudgeTokenCost::new(judge_label, prompt_tokens, 0)),
                    );
                }
            };
            let completion_tokens = count_tokens(&raw);
            let cost = JudgeTokenCost::new(judge_label, prompt_tokens, completion_tokens);
            match parse_verdict(&raw) {
                Some(verdict) => {
                    let mut decision = self.decide(&verdict, ccs);
                    if let Some(audit) = decision.audit.take() {
                        decision.audit = Some(audit.with_judge_cost(cost));
                    }
                    decision
                }
                None => QualifyDecision::reject(
                    "judge returned unparseable verdict".to_string(),
                    item.score,
                )
                .with_audit(QualifyAudit::from_signals(false, Vec::new()).with_judge_cost(cost)),
            }
        }
        .boxed()
    }
}

/// Extract a [`JudgeVerdict`] from a model reply, tolerating surrounding prose or code fences
/// by parsing the first balanced `{...}` object.
///
/// Re-exported as `parse_verdict_pub` for use by [`crate::council`].
pub(crate) fn parse_verdict_pub(raw: &str) -> Option<JudgeVerdict> {
    parse_verdict(raw)
}

fn parse_verdict(raw: &str) -> Option<JudgeVerdict> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut end = None;
    for (offset, character) in raw[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + offset + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let json = &raw[start..end?];
    serde_json::from_str(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CcsSchema, CommittedEntry, StaticLlmClient};

    fn ccs() -> CommittedContextState {
        CommittedContextState::new(CcsSchema::default(), 4096)
    }

    #[test]
    fn parses_verdict_with_surrounding_prose() {
        let raw = "Sure! Here is the score:\n```json\n{\"relevance\":0.9,\"novelty\":0.8,\
                   \"drift\":0.1,\"slot\":\"decision\",\"reason\":\"clear choice\"}\n```";
        let verdict = parse_verdict(raw).expect("parse");
        assert!((verdict.relevance - 0.9).abs() < 1e-6);
        assert_eq!(verdict.slot.as_deref(), Some("decision"));
    }

    #[tokio::test]
    async fn admits_high_relevance_low_drift() {
        let client = Arc::new(StaticLlmClient::new(
            "{\"relevance\":0.9,\"novelty\":0.8,\"drift\":0.1,\"slot\":\"decision\",\"reason\":\"ok\"}",
        ));
        let gate = JudgeQualifyGate::new(client);
        let item = RecallItem::new("a", "the team chose Rust", 1.0);
        let decision = gate.qualify(&item, &ccs()).await;
        assert!(decision.admitted);
        assert_eq!(decision.slot.as_deref(), Some("decision"));
    }

    #[tokio::test]
    async fn rejects_high_drift() {
        let client = Arc::new(StaticLlmClient::new(
            "{\"relevance\":0.9,\"novelty\":0.9,\"drift\":0.8,\"reason\":\"contradicts committed\"}",
        ));
        let gate = JudgeQualifyGate::new(client);
        let item = RecallItem::new("a", "the team chose Go", 1.0);
        let decision = gate.qualify(&item, &ccs()).await;
        assert!(!decision.admitted);
        assert!(decision.reason.contains("drift"));
    }

    #[tokio::test]
    async fn rejects_redundant_low_novelty() {
        let mut state = ccs();
        state.admit(CommittedEntry::new("x", "fact", "already known", 1.0));
        let client = Arc::new(StaticLlmClient::new(
            "{\"relevance\":0.9,\"novelty\":0.1,\"drift\":0.1,\"reason\":\"duplicate\"}",
        ));
        let gate = JudgeQualifyGate::new(client);
        let item = RecallItem::new("a", "already known", 1.0);
        let decision = gate.qualify(&item, &state).await;
        assert!(!decision.admitted);
        assert!(decision.reason.contains("redundant"));
    }

    #[tokio::test]
    async fn fails_closed_on_unparseable() {
        let client = Arc::new(StaticLlmClient::new("I cannot do that."));
        let gate = JudgeQualifyGate::new(client);
        let item = RecallItem::new("a", "whatever", 1.0);
        let decision = gate.qualify(&item, &ccs()).await;
        assert!(!decision.admitted);
    }
}
