// SPDX-License-Identifier: Apache-2.0

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};

use crate::{CommittedContextState, JudgeTokenCost, RecallItem};

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
    /// Cohen's kappa for exactly two binary signal/judge votes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cohen_kappa: Option<f32>,
    /// Krippendorff's alpha over binary signal/judge votes when more than two votes exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub krippendorff_alpha: Option<f32>,
    /// Name of the chance-corrected statistic used for `chance_corrected_agreement`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chance_corrected_method: Option<String>,
    /// Position-swap disagreement rate for order-sensitive pairwise judges, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_bias: Option<f32>,
    /// Threshold used to flag position bias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_bias_threshold: Option<f32>,
    /// Whether `position_bias` exceeded `position_bias_threshold`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_bias_flagged: Option<bool>,
    /// Judge tier used for this decision, when tiering was configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_tier: Option<String>,
    /// Estimated token costs for judge calls that contributed to this decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge_costs: Vec<JudgeTokenCost>,
    /// Derived decision confidence in `[0, 1]`, combining agreement with threshold distance.
    pub confidence: f32,
}

impl QualifyAudit {
    pub fn from_signals(admitted: bool, signals: Vec<QualifySignal>) -> Self {
        let agreement = signal_agreement(admitted, &signals);
        let stats = chance_corrected_signal_agreement(&signals);
        let confidence = decision_confidence(admitted, agreement, &signals);
        Self {
            signals,
            agreement,
            chance_corrected_agreement: stats.value,
            cohen_kappa: stats.cohen_kappa,
            krippendorff_alpha: stats.krippendorff_alpha,
            chance_corrected_method: stats.method,
            position_bias: None,
            position_bias_threshold: None,
            position_bias_flagged: None,
            judge_tier: None,
            judge_costs: Vec::new(),
            confidence,
        }
    }

    pub fn with_position_bias(mut self, bias: f32, threshold: f32) -> Self {
        self.position_bias = Some(bias);
        self.position_bias_threshold = Some(threshold);
        self.position_bias_flagged = Some(bias > threshold);
        self
    }

    pub fn with_judge_tier(mut self, tier: impl Into<String>) -> Self {
        self.judge_tier = Some(tier.into());
        self
    }

    pub fn with_judge_cost(mut self, cost: JudgeTokenCost) -> Self {
        self.judge_costs.push(cost);
        self
    }

    pub fn with_judge_costs(mut self, costs: Vec<JudgeTokenCost>) -> Self {
        self.judge_costs.extend(costs);
        self
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

#[derive(Debug, Clone, Default, PartialEq)]
struct ChanceCorrectedStats {
    value: Option<f32>,
    cohen_kappa: Option<f32>,
    krippendorff_alpha: Option<f32>,
    method: Option<String>,
}

fn chance_corrected_signal_agreement(signals: &[QualifySignal]) -> ChanceCorrectedStats {
    chance_corrected_binary_votes(signals.iter().map(|signal| signal.passed))
}

fn chance_corrected_binary_votes(votes: impl IntoIterator<Item = bool>) -> ChanceCorrectedStats {
    let votes: Vec<bool> = votes.into_iter().collect();
    let n = votes.len();
    if n < 2 {
        return ChanceCorrectedStats::default();
    }
    let statistic = binary_pairwise_kappa_alpha(&votes);
    if n == 2 {
        ChanceCorrectedStats {
            value: Some(statistic),
            cohen_kappa: Some(statistic),
            krippendorff_alpha: None,
            method: Some("cohen_kappa".to_string()),
        }
    } else {
        ChanceCorrectedStats {
            value: Some(statistic),
            cohen_kappa: None,
            krippendorff_alpha: Some(statistic),
            method: Some("krippendorff_alpha".to_string()),
        }
    }
}

fn binary_pairwise_kappa_alpha(votes: &[bool]) -> f32 {
    let n = votes.len();
    let yes = votes.iter().filter(|vote| **vote).count() as f32;
    let total = n as f32;
    let no = total - yes;
    let observed = (yes.mul_add(yes - 1.0, no * (no - 1.0))) / (total * (total - 1.0));
    let yes_rate = yes / total;
    let no_rate = no / total;
    let expected = yes_rate.mul_add(yes_rate, no_rate * no_rate);
    let denominator = 1.0 - expected;
    if denominator.abs() <= f32::EPSILON {
        1.0
    } else {
        ((observed - expected) / denominator).clamp(-1.0, 1.0)
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
        assert_eq!(audit.chance_corrected_agreement, audit.krippendorff_alpha);
        assert_eq!(
            audit.chance_corrected_method.as_deref(),
            Some("krippendorff_alpha")
        );
        assert!((0.0..=1.0).contains(&audit.confidence));
    }

    #[test]
    fn cohen_kappa_for_two_votes_matches_hand_computed_values() {
        let disagree = chance_corrected_binary_votes([true, false]);
        assert_eq!(disagree.cohen_kappa, Some(-1.0));
        assert_eq!(disagree.krippendorff_alpha, None);
        assert_eq!(disagree.method.as_deref(), Some("cohen_kappa"));

        let agree = chance_corrected_binary_votes([true, true]);
        assert_eq!(agree.cohen_kappa, Some(1.0));
        assert_eq!(agree.value, Some(1.0));
    }

    #[test]
    fn krippendorff_alpha_for_three_votes_matches_hand_computed_value() {
        let stats = chance_corrected_binary_votes([true, true, false]);
        assert_eq!(stats.cohen_kappa, None);
        assert_eq!(stats.method.as_deref(), Some("krippendorff_alpha"));
        let alpha = stats.krippendorff_alpha.expect("alpha for >2 votes");
        assert!(
            (alpha - -0.5).abs() < 1e-6,
            "2 yes / 1 no: observed agreement=1/3, expected=5/9, alpha=-0.5; got {alpha}"
        );
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
