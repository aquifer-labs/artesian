// SPDX-License-Identifier: Apache-2.0

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tiktoken_rs::{CoreBPE, cl100k_base};

fn tokenizer() -> Option<&'static CoreBPE> {
    static TOKENIZER: OnceLock<Option<CoreBPE>> = OnceLock::new();
    TOKENIZER.get_or_init(|| cl100k_base().ok()).as_ref()
}

/// Count tokens with the `cl100k_base` tokenizer (the same one the benchmark suite uses, so
/// footprint numbers are comparable), falling back to a chars/4 approximation if the
/// tokenizer cannot be loaded.
pub fn count_tokens(text: &str) -> usize {
    match tokenizer() {
        Some(bpe) => bpe.encode_with_special_tokens(text).len(),
        None => text.chars().count().div_ceil(4),
    }
}

/// Per-cycle control metrics emitted by the [`crate::Headgate`] commit-loop.
///
/// `footprint_tokens` is the committed-state token count — the **footprint** moat metric.
/// Drift and hallucination require an LLM judge and are added by a judge-eval gate in a
/// later slice; this struct is the stable surface those metrics will extend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaugeMetrics {
    pub candidates: usize,
    pub admitted: usize,
    pub rejected_relevance: usize,
    pub rejected_redundant: usize,
    pub rejected_saturated: usize,
    pub compressed: usize,
    pub evicted: usize,
    pub footprint_tokens: usize,
    pub budget_tokens: usize,
}

impl GaugeMetrics {
    /// Fraction of recall candidates admitted into the committed state.
    pub fn admit_rate(&self) -> f32 {
        if self.candidates == 0 {
            0.0
        } else {
            self.admitted as f32 / self.candidates as f32
        }
    }

    /// Committed footprint as a fraction of the budget (1.0 = saturated).
    pub fn saturation(&self) -> f32 {
        if self.budget_tokens == 0 {
            0.0
        } else {
            self.footprint_tokens as f32 / self.budget_tokens as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_count_is_positive_for_nonempty() {
        assert!(count_tokens("the agent decided to use Rust") > 0);
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn admit_rate_and_saturation() {
        let metrics = GaugeMetrics {
            candidates: 10,
            admitted: 4,
            footprint_tokens: 512,
            budget_tokens: 2048,
            ..GaugeMetrics::default()
        };
        assert!((metrics.admit_rate() - 0.4).abs() < 1e-6);
        assert!((metrics.saturation() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn empty_metrics_do_not_divide_by_zero() {
        let metrics = GaugeMetrics::default();
        assert_eq!(metrics.admit_rate(), 0.0);
        assert_eq!(metrics.saturation(), 0.0);
    }
}
