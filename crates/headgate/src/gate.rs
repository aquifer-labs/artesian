// SPDX-License-Identifier: Apache-2.0

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};

use crate::{CommittedContextState, RecallItem};

/// The qualify-gate's verdict on a single recall candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualifyDecision {
    pub admitted: bool,
    pub reason: String,
    pub slot: Option<String>,
    pub score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<QualifyAudit>,
}

/// One deterministic signal that contributed to a qualify-gate decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualifySignal {
    pub name: String,
    pub value: f32,
    pub threshold: f32,
    pub passed: bool,
    /// Normalized distance from the threshold, clamped to `[0, 1]`.
    pub margin: f32,
}

impl QualifySignal {
    pub fn new(name: impl Into<String>, value: f32, threshold: f32, passed: bool) -> Self {
        Self {
            name: name.into(),
            value,
            threshold,
            passed,
            margin: normalized_margin(value, threshold),
        }
    }
}

/// Bias-audited, chance-corrected metadata for a qualify-gate decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualifyAudit {
    pub signals: Vec<QualifySignal>,
    /// Fraction of signal votes that agree with the final admit/reject verdict.
    pub agreement: f32,
    /// Fleiss/Cohen-style chance-corrected agreement over the binary signal votes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chance_corrected_agreement: Option<f32>,
    /// Derived decision confidence in `[0, 1]`, combining agreement with threshold distance.
    pub confidence: f32,
}

impl QualifyAudit {
    pub fn from_signals(admitted: bool, signals: Vec<QualifySignal>) -> Self {
        let agreement = signal_agreement(admitted, &signals);
        let chance_corrected_agreement = chance_corrected_signal_agreement(&signals);
        let confidence = decision_confidence(admitted, agreement, &signals);
        Self {
            signals,
            agreement,
            chance_corrected_agreement,
            confidence,
        }
    }
}

impl QualifyDecision {
    pub fn admit(slot: impl Into<String>, score: f32) -> Self {
        Self {
            admitted: true,
            reason: "qualified".to_string(),
            slot: Some(slot.into()),
            score,
            audit: None,
        }
    }

    pub fn reject(reason: impl Into<String>, score: f32) -> Self {
        Self {
            admitted: false,
            reason: reason.into(),
            slot: None,
            score,
            audit: None,
        }
    }

    pub fn with_audit(mut self, audit: QualifyAudit) -> Self {
        self.audit = Some(audit);
        self
    }
}

/// The qualify-gate — the ACC trust boundary. Only candidates that qualify (relevant,
/// non-duplicate, non-redundant) are eligible to enter the committed state. The default
/// implementation is deterministic; the feature-gated LLM judge-eval gate
/// ([`crate::JudgeQualifyGate`], scoring drift / hallucination) is a drop-in replacement.
///
/// `qualify` is async so an implementation may consult an external judge (an LLM); the
/// deterministic gate resolves immediately. An implementation that cannot reach its judge
/// should return a conservative reject rather than surface an error.
pub trait QualifyGate: Send + Sync {
    fn qualify<'a>(
        &'a self,
        item: &'a RecallItem,
        ccs: &'a CommittedContextState,
    ) -> BoxFuture<'a, QualifyDecision>;
}

/// Deterministic default gate: relevance threshold + redundancy rejection + slot routing.
#[derive(Debug, Clone)]
pub struct DefaultQualifyGate {
    /// Minimum candidate score to qualify (interpreted on the recall store's score scale).
    pub min_score: f32,
    /// Token-overlap at or above which a candidate is treated as redundant.
    pub redundancy_threshold: f32,
    slot_keywords: Vec<(String, Vec<String>)>,
}

impl DefaultQualifyGate {
    pub fn new(min_score: f32, redundancy_threshold: f32) -> Self {
        Self {
            min_score,
            redundancy_threshold,
            slot_keywords: default_slot_keywords(),
        }
    }

    /// Override the keyword → slot routing table.
    pub fn with_slot_keywords(mut self, slot_keywords: Vec<(String, Vec<String>)>) -> Self {
        self.slot_keywords = slot_keywords;
        self
    }

    fn route_slot(&self, item: &RecallItem, ccs: &CommittedContextState) -> String {
        let lower = item.content.to_lowercase();
        for (slot, keywords) in &self.slot_keywords {
            if ccs.schema().contains(slot) && keywords.iter().any(|keyword| lower.contains(keyword))
            {
                return slot.clone();
            }
        }
        ccs.schema().default_slot().to_string()
    }
}

impl Default for DefaultQualifyGate {
    fn default() -> Self {
        Self::new(0.2, 0.8)
    }
}

impl DefaultQualifyGate {
    fn decide(&self, item: &RecallItem, ccs: &CommittedContextState) -> QualifyDecision {
        let already_committed = ccs.contains(&item.id);
        let overlap = ccs.max_overlap(&item.content);
        let signals = self.audit_signals(item, already_committed, overlap);

        if item.score < self.min_score {
            return QualifyDecision::reject(
                format!(
                    "below relevance threshold ({:.3} < {:.3})",
                    item.score, self.min_score
                ),
                item.score,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        if already_committed {
            return QualifyDecision::reject("already committed", item.score)
                .with_audit(QualifyAudit::from_signals(false, signals));
        }
        if overlap >= self.redundancy_threshold {
            return QualifyDecision::reject(
                format!(
                    "redundant (overlap {overlap:.3} >= {:.3})",
                    self.redundancy_threshold
                ),
                item.score,
            )
            .with_audit(QualifyAudit::from_signals(false, signals));
        }
        QualifyDecision::admit(self.route_slot(item, ccs), item.score)
            .with_audit(QualifyAudit::from_signals(true, signals))
    }

    fn audit_signals(
        &self,
        item: &RecallItem,
        already_committed: bool,
        overlap: f32,
    ) -> Vec<QualifySignal> {
        let novelty = (1.0 - overlap).clamp(0.0, 1.0);
        let novelty_threshold = (1.0 - self.redundancy_threshold).clamp(0.0, 1.0);
        vec![
            QualifySignal::new(
                "relevance",
                item.score,
                self.min_score,
                item.score >= self.min_score,
            ),
            QualifySignal::new(
                "uniqueness",
                if already_committed { 0.0 } else { 1.0 },
                0.5,
                !already_committed,
            ),
            QualifySignal::new(
                "novelty",
                novelty,
                novelty_threshold,
                overlap < self.redundancy_threshold,
            ),
        ]
    }
}

impl QualifyGate for DefaultQualifyGate {
    fn qualify<'a>(
        &'a self,
        item: &'a RecallItem,
        ccs: &'a CommittedContextState,
    ) -> BoxFuture<'a, QualifyDecision> {
        let decision = self.decide(item, ccs);
        async move { decision }.boxed()
    }
}

fn default_slot_keywords() -> Vec<(String, Vec<String>)> {
    vec![
        (
            "decision".to_string(),
            ["decid", "chose", "chosen", "will use", "agreed", "selected"]
                .iter()
                .map(|keyword| keyword.to_string())
                .collect(),
        ),
        (
            "constraint".to_string(),
            ["must", "never", "always", "require", "cannot", "do not"]
                .iter()
                .map(|keyword| keyword.to_string())
                .collect(),
        ),
        (
            "task-state".to_string(),
            ["todo", "in progress", "blocked", "next step", "remaining"]
                .iter()
                .map(|keyword| keyword.to_string())
                .collect(),
        ),
    ]
}

fn normalized_margin(value: f32, threshold: f32) -> f32 {
    let scale = value.abs().max(threshold.abs()).max(1.0);
    ((value - threshold).abs() / scale).clamp(0.0, 1.0)
}

fn signal_agreement(admitted: bool, signals: &[QualifySignal]) -> f32 {
    if signals.is_empty() {
        return 1.0;
    }
    let agreeing = signals
        .iter()
        .filter(|signal| signal.passed == admitted)
        .count();
    agreeing as f32 / signals.len() as f32
}

fn chance_corrected_signal_agreement(signals: &[QualifySignal]) -> Option<f32> {
    let n = signals.len();
    if n < 2 {
        return None;
    }
    let yes = signals.iter().filter(|signal| signal.passed).count() as f32;
    let total = n as f32;
    let no = total - yes;
    let observed = (yes.mul_add(yes - 1.0, no * (no - 1.0))) / (total * (total - 1.0));
    let yes_rate = yes / total;
    let no_rate = no / total;
    let expected = yes_rate.mul_add(yes_rate, no_rate * no_rate);
    let denominator = 1.0 - expected;
    if denominator.abs() <= f32::EPSILON {
        Some(1.0)
    } else {
        Some(((observed - expected) / denominator).clamp(-1.0, 1.0))
    }
}

fn decision_confidence(admitted: bool, agreement: f32, signals: &[QualifySignal]) -> f32 {
    let decisive_margin = if signals.is_empty() {
        1.0
    } else if admitted {
        signals
            .iter()
            .map(|signal| signal.margin)
            .fold(1.0_f32, f32::min)
    } else {
        signals
            .iter()
            .filter(|signal| !signal.passed)
            .map(|signal| signal.margin)
            .fold(None, |acc: Option<f32>, margin| {
                Some(acc.map_or(margin, |current| current.max(margin)))
            })
            .unwrap_or(0.0)
    };
    ((agreement.clamp(0.0, 1.0) + decisive_margin.clamp(0.0, 1.0)) / 2.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CcsSchema;

    fn empty_ccs() -> CommittedContextState {
        CommittedContextState::new(CcsSchema::default(), 4096)
    }

    #[test]
    fn rejects_below_relevance() {
        let gate = DefaultQualifyGate::new(0.5, 0.8);
        let item = RecallItem::new("a", "some weakly relevant note", 0.1);
        let decision = gate.decide(&item, &empty_ccs());
        assert!(!decision.admitted);
        assert!(decision.reason.contains("below relevance"));
    }

    #[test]
    fn routes_decision_keyword_to_decision_slot() {
        let gate = DefaultQualifyGate::default();
        let item = RecallItem::new("a", "we chose Rust for the core crates", 1.0);
        let decision = gate.decide(&item, &empty_ccs());
        assert!(decision.admitted);
        assert_eq!(decision.slot.as_deref(), Some("decision"));
    }

    #[test]
    fn routes_unmatched_to_default_slot() {
        let gate = DefaultQualifyGate::default();
        let item = RecallItem::new("a", "the cluster has three nodes", 1.0);
        let decision = gate.decide(&item, &empty_ccs());
        assert_eq!(decision.slot.as_deref(), Some("decision")); // default_slot = first
    }

    #[test]
    fn rejects_redundant_against_committed() {
        let gate = DefaultQualifyGate::new(0.2, 0.6);
        let mut ccs = empty_ccs();
        ccs.admit(crate::CommittedEntry::new(
            "a",
            "fact",
            "the deployment runs nightly on the kubernetes cluster",
            1.0,
        ));
        let item = RecallItem::new(
            "b",
            "the deployment runs nightly on the kubernetes cluster",
            1.0,
        );
        let decision = gate.decide(&item, &ccs);
        assert!(!decision.admitted);
        assert!(decision.reason.contains("redundant"));
    }

    #[test]
    fn multi_signal_decision_reports_audited_agreement_and_confidence() {
        let gate = DefaultQualifyGate::new(0.5, 0.6);
        let mut ccs = empty_ccs();
        ccs.admit(crate::CommittedEntry::new(
            "existing",
            "fact",
            "deployment runs nightly on kubernetes",
            1.0,
        ));
        let item = RecallItem::new("candidate", "deployment runs nightly on kubernetes", 0.9);
        let decision = gate.decide(&item, &ccs);
        assert!(!decision.admitted);
        let audit = decision.audit.expect("audit should be attached");
        assert_eq!(audit.signals.len(), 3);
        assert!((0.0..=1.0).contains(&audit.agreement));
        assert!(audit.chance_corrected_agreement.is_some());
        assert!((0.0..=1.0).contains(&audit.confidence));
    }

    #[test]
    fn audited_decision_preserves_existing_admit_reject_outcome() {
        let gate = DefaultQualifyGate::new(0.5, 0.6);
        let ccs = empty_ccs();
        let weak = RecallItem::new("weak", "some weakly relevant note", 0.1);
        let strong = RecallItem::new("strong", "the team chose Rust", 0.9);

        assert_eq!(
            gate.decide(&weak, &ccs).admitted,
            weak.score >= gate.min_score
        );
        assert!(gate.decide(&strong, &ccs).admitted);
    }
}
