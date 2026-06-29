// SPDX-License-Identifier: Apache-2.0

//! Position-swap debiasing for order-sensitive pairwise judges.

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};

use crate::{CommittedContextState, RecallItem};

/// Default maximum tolerated AB/BA disagreement rate.
pub const DEFAULT_POSITION_BIAS_THRESHOLD: f32 = 0.10;

/// A judge's pairwise preference in the order it saw the candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairwiseChoice {
    First,
    Second,
    Tie,
}

impl PairwiseChoice {
    fn selected_id<'a>(&self, first: &'a RecallItem, second: &'a RecallItem) -> Option<&'a str> {
        match self {
            Self::First => Some(first.id.as_str()),
            Self::Second => Some(second.id.as_str()),
            Self::Tie => None,
        }
    }
}

/// One pairwise judge verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairwiseDecision {
    pub choice: PairwiseChoice,
    pub score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PairwiseDecision {
    pub fn new(choice: PairwiseChoice, score: f32) -> Self {
        Self {
            choice,
            score,
            reason: None,
        }
    }
}

/// A judge that compares two candidates and may be sensitive to input order.
pub trait PairwiseJudge: Send + Sync {
    fn compare<'a>(
        &'a self,
        first: &'a RecallItem,
        second: &'a RecallItem,
        ccs: &'a CommittedContextState,
    ) -> BoxFuture<'a, PairwiseDecision>;
}

/// Position-swap audit result for one AB/BA comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionSwapAudit {
    pub ab_choice: PairwiseChoice,
    pub ba_choice: PairwiseChoice,
    pub position_bias: f32,
    pub threshold: f32,
    pub flagged: bool,
}

/// Run a pairwise judge in AB and BA order and report order-induced disagreement.
pub async fn position_swap_debias<J>(
    judge: &J,
    a: &RecallItem,
    b: &RecallItem,
    ccs: &CommittedContextState,
    threshold: f32,
) -> PositionSwapAudit
where
    J: PairwiseJudge + ?Sized,
{
    let ab = judge.compare(a, b, ccs).await;
    let ba = judge.compare(b, a, ccs).await;
    let ab_winner = ab.choice.selected_id(a, b);
    let ba_winner = ba.choice.selected_id(b, a);
    let position_bias = f32::from(ab_winner != ba_winner);
    PositionSwapAudit {
        ab_choice: ab.choice,
        ba_choice: ba.choice,
        position_bias,
        threshold,
        flagged: position_bias > threshold,
    }
}

#[derive(Debug, Clone)]
pub struct StaticPairwiseJudge {
    ab_choice: PairwiseChoice,
    ba_choice: PairwiseChoice,
}

impl StaticPairwiseJudge {
    pub fn new(ab_choice: PairwiseChoice, ba_choice: PairwiseChoice) -> Self {
        Self {
            ab_choice,
            ba_choice,
        }
    }
}

impl PairwiseJudge for StaticPairwiseJudge {
    fn compare<'a>(
        &'a self,
        first: &'a RecallItem,
        second: &'a RecallItem,
        _ccs: &'a CommittedContextState,
    ) -> BoxFuture<'a, PairwiseDecision> {
        let choice = if first.id <= second.id {
            self.ab_choice
        } else {
            self.ba_choice
        };
        async move { PairwiseDecision::new(choice, 1.0) }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CcsSchema;

    fn ccs() -> CommittedContextState {
        CommittedContextState::new(CcsSchema::default(), 4096)
    }

    #[tokio::test]
    async fn flags_position_biased_pairwise_judge() {
        let judge = StaticPairwiseJudge::new(PairwiseChoice::First, PairwiseChoice::First);
        let a = RecallItem::new("a", "candidate a", 1.0);
        let b = RecallItem::new("b", "candidate b", 1.0);

        let audit =
            position_swap_debias(&judge, &a, &b, &ccs(), DEFAULT_POSITION_BIAS_THRESHOLD).await;

        assert_eq!(audit.position_bias, 1.0);
        assert!(audit.flagged);
    }

    #[tokio::test]
    async fn passes_unbiased_pairwise_judge() {
        let judge = StaticPairwiseJudge::new(PairwiseChoice::First, PairwiseChoice::Second);
        let a = RecallItem::new("a", "candidate a", 1.0);
        let b = RecallItem::new("b", "candidate b", 1.0);

        let audit =
            position_swap_debias(&judge, &a, &b, &ccs(), DEFAULT_POSITION_BIAS_THRESHOLD).await;

        assert_eq!(audit.position_bias, 0.0);
        assert!(!audit.flagged);
    }
}
