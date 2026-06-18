// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::metrics::count_tokens;
use crate::{
    CcsSchema, CommittedContextState, CommittedEntry, Compressor, DefaultQualifyGate,
    ExtractiveCompressor, GaugeMetrics, HeadgateResult, NoopCompressor, QualifyGate, RecallStore,
};

/// Tunables for the ACC commit-loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadgateConfig {
    /// Token budget for the committed state (the saturation bound).
    pub budget_tokens: usize,
    /// How many recall candidates to pull per cycle.
    pub recall_limit: usize,
    /// Minimum candidate score to qualify (recall-store-relative scale).
    pub min_score: f32,
    /// Token-overlap at or above which a candidate is rejected as redundant.
    pub redundancy_threshold: f32,
    /// Compress an admitted candidate to fit remaining headroom instead of rejecting it.
    pub compress_on_saturation: bool,
}

impl Default for HeadgateConfig {
    fn default() -> Self {
        Self {
            budget_tokens: 2048,
            recall_limit: 16,
            min_score: 0.2,
            redundancy_threshold: 0.8,
            compress_on_saturation: true,
        }
    }
}

impl From<&artesian_core::AccConfig> for HeadgateConfig {
    fn from(config: &artesian_core::AccConfig) -> Self {
        Self {
            budget_tokens: config.budget_tokens,
            recall_limit: config.recall_limit,
            min_score: config.min_score,
            redundancy_threshold: config.redundancy_threshold,
            compress_on_saturation: config.compress_on_saturation,
        }
    }
}

/// The ACC commit-loop controller.
///
/// Each [`cycle`](Headgate::cycle) pulls recall candidates from the data plane, runs each
/// through the qualify-gate, and admits qualifying knowledge into the bounded committed
/// state — evicting lower-value entries or compressing under saturation — then reports
/// per-cycle [`GaugeMetrics`]. The committed state is the authoritative context the agent
/// reads via [`render`](Headgate::render).
pub struct Headgate {
    recall: Arc<dyn RecallStore>,
    gate: Arc<dyn QualifyGate>,
    compressor: Arc<dyn Compressor>,
    ccs: CommittedContextState,
    config: HeadgateConfig,
}

impl Headgate {
    /// Build a controller with the deterministic default gate and (per config) the
    /// extractive or no-op compressor, over a default schema.
    pub fn new(recall: Arc<dyn RecallStore>, config: HeadgateConfig) -> Self {
        let ccs = CommittedContextState::new(CcsSchema::default(), config.budget_tokens);
        let gate: Arc<dyn QualifyGate> = Arc::new(DefaultQualifyGate::new(
            config.min_score,
            config.redundancy_threshold,
        ));
        let compressor: Arc<dyn Compressor> = if config.compress_on_saturation {
            Arc::new(ExtractiveCompressor)
        } else {
            Arc::new(NoopCompressor)
        };
        Self {
            recall,
            gate,
            compressor,
            ccs,
            config,
        }
    }

    /// Replace the committed-state schema (resets the committed state).
    pub fn with_schema(mut self, schema: CcsSchema) -> Self {
        self.ccs = CommittedContextState::new(schema, self.config.budget_tokens);
        self
    }

    /// Replace the qualify-gate (e.g. an LLM judge-eval gate).
    pub fn with_gate(mut self, gate: Arc<dyn QualifyGate>) -> Self {
        self.gate = gate;
        self
    }

    /// Replace the compressor (e.g. an LLM-backed summarizer).
    pub fn with_compressor(mut self, compressor: Arc<dyn Compressor>) -> Self {
        self.compressor = compressor;
        self
    }

    pub fn ccs(&self) -> &CommittedContextState {
        &self.ccs
    }

    pub fn config(&self) -> &HeadgateConfig {
        &self.config
    }

    /// The committed context the agent reads, rendered as slot-grouped markdown.
    pub fn render(&self) -> String {
        self.ccs.render()
    }

    /// Run one ACC cycle for `query`: recall candidates, qualify each, and admit qualifying
    /// knowledge into the committed state with saturation handling.
    pub async fn cycle(&mut self, query: &str) -> HeadgateResult<GaugeMetrics> {
        let candidates = self.recall.recall(query, self.config.recall_limit).await?;
        let mut metrics = GaugeMetrics {
            candidates: candidates.len(),
            budget_tokens: self.config.budget_tokens,
            ..GaugeMetrics::default()
        };

        for item in candidates {
            let decision = self.gate.qualify(&item, &self.ccs).await;
            if !decision.admitted {
                if decision.reason.starts_with("redundant")
                    || decision.reason == "already committed"
                {
                    metrics.rejected_redundant += 1;
                } else {
                    metrics.rejected_relevance += 1;
                }
                continue;
            }

            let slot = decision
                .slot
                .unwrap_or_else(|| self.ccs.schema().default_slot().to_string());
            let mut content = item.content;
            let mut tokens = count_tokens(&content);

            if tokens > self.ccs.headroom() {
                // 1) Evict committed entries scoring strictly below this candidate.
                while tokens > self.ccs.headroom() && !self.ccs.is_empty() {
                    match self.ccs.lowest_score() {
                        Some(lowest) if lowest < decision.score => {
                            if self.ccs.evict_lowest().is_some() {
                                metrics.evicted += 1;
                            } else {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                // 2) Compress to remaining headroom, when enabled and there is headroom.
                if tokens > self.ccs.headroom()
                    && self.config.compress_on_saturation
                    && self.ccs.headroom() > 0
                {
                    let headroom = self.ccs.headroom();
                    let compressed = self.compressor.compress(&content, headroom).await?;
                    if compressed != content {
                        metrics.compressed += 1;
                        content = compressed;
                        tokens = count_tokens(&content);
                    }
                }
                // 3) Still over budget → cannot admit without overflowing the bound.
                if tokens > self.ccs.headroom() {
                    metrics.rejected_saturated += 1;
                    continue;
                }
            }

            self.ccs
                .admit(CommittedEntry::new(item.id, slot, content, decision.score));
            metrics.admitted += 1;
        }

        metrics.footprint_tokens = self.ccs.token_count();
        Ok(metrics)
    }
}
