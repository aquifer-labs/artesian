// SPDX-License-Identifier: Apache-2.0

//! Cost-aware judge tiering.

use std::sync::Arc;

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};

use crate::{CommittedContextState, QualifyAudit, QualifyDecision, QualifyGate, RecallItem};

/// A configured judge tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeTier {
    Cheap,
    Frontier,
}

impl JudgeTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Frontier => "frontier",
        }
    }
}

/// Bindings and thresholds for selecting a cheap or frontier judge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeTierConfig {
    /// Binding name for the cheap judge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap_judge: Option<String>,
    /// Binding name for the frontier judge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier_judge: Option<String>,
    /// Stakes at or above this value use the frontier judge.
    pub frontier_stakes_threshold: f32,
    /// Stakes used when callers invoke the plain `QualifyGate` trait.
    pub default_stakes: f32,
}

impl Default for JudgeTierConfig {
    fn default() -> Self {
        Self {
            cheap_judge: None,
            frontier_judge: None,
            frontier_stakes_threshold: 0.75,
            default_stakes: 0.0,
        }
    }
}

impl JudgeTierConfig {
    pub fn new(
        cheap_judge: impl Into<String>,
        frontier_judge: impl Into<String>,
        frontier_stakes_threshold: f32,
    ) -> Self {
        Self {
            cheap_judge: Some(cheap_judge.into()),
            frontier_judge: Some(frontier_judge.into()),
            frontier_stakes_threshold,
            default_stakes: 0.0,
        }
    }

    pub fn with_default_stakes(mut self, default_stakes: f32) -> Self {
        self.default_stakes = default_stakes;
        self
    }

    pub fn select(&self, stakes: f32) -> JudgeTier {
        if stakes >= self.frontier_stakes_threshold {
            JudgeTier::Frontier
        } else {
            JudgeTier::Cheap
        }
    }
}

/// A qualify-gate that selects cheap or frontier gates by configured stakes.
pub struct TieredQualifyGate {
    cheap: Arc<dyn QualifyGate>,
    frontier: Arc<dyn QualifyGate>,
    config: JudgeTierConfig,
}

impl TieredQualifyGate {
    pub fn new(
        cheap: Arc<dyn QualifyGate>,
        frontier: Arc<dyn QualifyGate>,
        config: JudgeTierConfig,
    ) -> Self {
        Self {
            cheap,
            frontier,
            config,
        }
    }

    pub fn config(&self) -> &JudgeTierConfig {
        &self.config
    }

    pub fn select_tier(&self, stakes: f32) -> JudgeTier {
        self.config.select(stakes)
    }

    pub fn qualify_with_stakes<'a>(
        &'a self,
        item: &'a RecallItem,
        ccs: &'a CommittedContextState,
        stakes: f32,
    ) -> BoxFuture<'a, QualifyDecision> {
        async move {
            let tier = self.select_tier(stakes);
            let gate = match tier {
                JudgeTier::Cheap => &self.cheap,
                JudgeTier::Frontier => &self.frontier,
            };
            let mut decision = gate.qualify(item, ccs).await;
            let audit = decision
                .audit
                .take()
                .unwrap_or_else(|| QualifyAudit::from_signals(decision.admitted, Vec::new()))
                .with_judge_tier(tier.as_str());
            decision.audit = Some(audit);
            decision
        }
        .boxed()
    }
}

impl QualifyGate for TieredQualifyGate {
    fn qualify<'a>(
        &'a self,
        item: &'a RecallItem,
        ccs: &'a CommittedContextState,
    ) -> BoxFuture<'a, QualifyDecision> {
        self.qualify_with_stakes(item, ccs, self.config.default_stakes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{future::BoxFuture, FutureExt};

    use crate::CcsSchema;

    struct StaticGate {
        admitted: bool,
    }

    impl QualifyGate for StaticGate {
        fn qualify<'a>(
            &'a self,
            _item: &'a RecallItem,
            _ccs: &'a CommittedContextState,
        ) -> BoxFuture<'a, QualifyDecision> {
            let decision = if self.admitted {
                QualifyDecision::admit("decision", 1.0)
            } else {
                QualifyDecision::reject("cheap reject", 0.1)
            };
            async move { decision }.boxed()
        }
    }

    fn ccs() -> CommittedContextState {
        CommittedContextState::new(CcsSchema::default(), 4096)
    }

    #[tokio::test]
    async fn tiering_selects_cheap_vs_frontier_by_stakes() {
        let config = JudgeTierConfig::new("cheap-local", "frontier-remote", 0.7);
        let gate = TieredQualifyGate::new(
            Arc::new(StaticGate { admitted: false }),
            Arc::new(StaticGate { admitted: true }),
            config,
        );
        let item = RecallItem::new("a", "candidate", 1.0);

        let cheap = gate.qualify_with_stakes(&item, &ccs(), 0.2).await;
        assert!(!cheap.admitted);
        assert_eq!(
            cheap
                .audit
                .as_ref()
                .and_then(|audit| audit.judge_tier.as_deref()),
            Some("cheap")
        );

        let frontier = gate.qualify_with_stakes(&item, &ccs(), 0.9).await;
        assert!(frontier.admitted);
        assert_eq!(
            frontier
                .audit
                .as_ref()
                .and_then(|audit| audit.judge_tier.as_deref()),
            Some("frontier")
        );
    }
}
