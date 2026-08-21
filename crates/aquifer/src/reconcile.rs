// SPDX-License-Identifier: Apache-2.0

//! Write-time reconciliation: when a new memory is semantically close to an existing one,
//! reconcile rather than blindly appending.
//!
//! ## Decisions
//! - **Add**: no existing record is close enough → store as new (default behaviour).
//! - **Update**: an existing record is similar; augment/merge its content in place.
//! - **Supersede**: an existing record is contradicted; replace it and record a `superseded_by` pointer.
//! - **Noop**: the incoming content is already covered by an existing record; skip.
//!
//! Reconciliation is **opt-in** (`ReconcileConfig::reconcile_on_write = false` by default) so
//! existing store behaviour is unchanged when the caller has not enabled it.
//!
//! The caller is responsible for performing the actual store/update/delete against the backend;
//! this module only analyses the candidates and returns the decision.
//!
//! ## Similarity
//! Uses a simple normalised term-overlap (Jaccard over lowercased tokens) — no LLM or embedding
//! required, consistent with the no-LLM fallback used elsewhere in this crate.

use crate::MemoryRecord;

/// How similar two contents must be (Jaccard token overlap) for reconciliation to apply.
pub const DEFAULT_RECONCILE_THRESHOLD: f32 = 0.55;

/// Configuration for write-time reconciliation.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileConfig {
    /// Enable reconciliation on write. Default `false` (opt-in, backward-compat).
    pub reconcile_on_write: bool,
    /// Jaccard similarity threshold above which reconciliation is considered.
    /// Records below this threshold → `Add`.
    pub similarity_threshold: f32,
    /// If the incoming content is a *subset* of the existing record's tokens (i.e., covered),
    /// return `Noop` rather than `Update`. Threshold for "covered" similarity.
    pub noop_threshold: f32,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self {
            reconcile_on_write: false,
            similarity_threshold: DEFAULT_RECONCILE_THRESHOLD,
            noop_threshold: 0.85,
        }
    }
}

/// The reconciliation decision for a store operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReconcileDecision {
    /// No existing record is close enough — store as new.
    Add,
    /// An existing record is similar; update (merge/augment) it in place.
    /// Contains the ID of the record to update, and the merged content.
    Update {
        existing_id: String,
        merged_content: String,
    },
    /// An existing record is superseded; replace it, recording this incoming content as the
    /// successor. Contains the ID of the superseded record.
    Supersede { superseded_id: String },
    /// The incoming content is already fully covered by an existing record — skip.
    Noop { covered_by_id: String },
}

use serde::{Deserialize, Serialize};

/// Analyse `candidates` (existing records) against `incoming_content` and return the
/// appropriate [`ReconcileDecision`].
///
/// When `config.reconcile_on_write` is `false`, always returns `Add` (no-op fast path).
pub fn reconcile(
    incoming_content: &str,
    candidates: &[&MemoryRecord],
    config: &ReconcileConfig,
) -> ReconcileDecision {
    if !config.reconcile_on_write || candidates.is_empty() {
        return ReconcileDecision::Add;
    }

    let incoming_tokens = tokenize(incoming_content);

    // Find the most-similar existing record.
    let mut best_sim = 0.0f32;
    let mut best_candidate: Option<&MemoryRecord> = None;
    for candidate in candidates {
        let existing_tokens = tokenize(&candidate.content);
        let sim = jaccard(&incoming_tokens, &existing_tokens);
        if sim > best_sim {
            best_sim = sim;
            best_candidate = Some(candidate);
        }
    }

    let Some(best) = best_candidate else {
        return ReconcileDecision::Add;
    };

    if best_sim < config.similarity_threshold {
        // Not close enough — add as new.
        return ReconcileDecision::Add;
    }

    if best_sim >= config.noop_threshold {
        // Effectively the same content — skip.
        return ReconcileDecision::Noop {
            covered_by_id: best.id.to_string(),
        };
    }

    // In the middle band: decide Update vs Supersede.
    // Heuristic: if the incoming content is *longer* than the existing record it likely
    // contains new information → Supersede (the old one is now outdated / subsumed).
    // If shorter or similar length → Update (augment / merge).
    let incoming_chars = incoming_content.chars().count();
    let existing_chars = best.content.chars().count();

    if incoming_chars > existing_chars + 20 {
        // Incoming is substantially longer → likely a more complete / updated version.
        ReconcileDecision::Supersede {
            superseded_id: best.id.to_string(),
        }
    } else {
        // Merge: concatenate with a separator if the contents are meaningfully different.
        let merged = if incoming_content.trim() == best.content.trim() {
            best.content.clone()
        } else {
            format!("{}\n\n{}", best.content.trim(), incoming_content.trim())
        };
        ReconcileDecision::Update {
            existing_id: best.id.to_string(),
            merged_content: merged,
        }
    }
}

/// Tokenise a string into a sorted, deduplicated `Vec<String>` of lowercase words.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect();
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

/// Jaccard similarity between two sorted token vecs.
fn jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut i = 0;
    let mut j = 0;
    let mut inter = 0usize;
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    let union = a.len() + b.len() - inter;
    if union == 0 {
        1.0
    } else {
        inter as f32 / union as f32
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{MemoryId, MemoryRecord, MemoryTier};

    fn make_candidate(id: &str, content: &str) -> MemoryRecord {
        MemoryRecord::new(
            MemoryId::new(id),
            format!("node:{id}"),
            content,
            Vec::new(),
            BTreeMap::new(),
            MemoryTier::L1Atom,
        )
    }

    fn cfg_on() -> ReconcileConfig {
        ReconcileConfig {
            reconcile_on_write: true,
            ..ReconcileConfig::default()
        }
    }

    /// Config with a lower threshold, for tests where the content similarity is moderate.
    fn cfg_on_low_threshold() -> ReconcileConfig {
        ReconcileConfig {
            reconcile_on_write: true,
            similarity_threshold: 0.3,
            noop_threshold: 0.85,
        }
    }

    #[test]
    fn add_when_disabled() {
        let record = make_candidate("a", "The team chose Rust for the backend");
        let decision = reconcile(
            "The team chose Rust for the backend",
            &[&record],
            &ReconcileConfig::default(), // reconcile_on_write = false
        );
        assert_eq!(decision, ReconcileDecision::Add);
    }

    #[test]
    fn add_when_no_candidates() {
        let decision = reconcile("new content", &[], &cfg_on());
        assert_eq!(decision, ReconcileDecision::Add);
    }

    #[test]
    fn add_when_dissimilar() {
        let record = make_candidate("a", "Python is a dynamically typed language");
        let decision = reconcile("The Eiffel tower is in Paris", &[&record], &cfg_on());
        assert_eq!(decision, ReconcileDecision::Add);
    }

    #[test]
    fn noop_when_nearly_identical() {
        let content = "The team chose Rust for the backend crate";
        let record = make_candidate("a", content);
        // Same content → should be Noop
        let decision = reconcile(content, &[&record], &cfg_on());
        assert!(
            matches!(decision, ReconcileDecision::Noop { .. }),
            "expected Noop, got {decision:?}"
        );
    }

    #[test]
    fn update_when_similar_same_length() {
        // Use a lower threshold because shared-token Jaccard for similar-length sentences
        // with partial synonym overlap is typically in the 0.3–0.5 range.
        let existing = "The team chose Rust for the core crate because of performance";
        let incoming = "The team chose Rust for the core because it is fast and safe";
        let record = make_candidate("a", existing);
        let decision = reconcile(incoming, &[&record], &cfg_on_low_threshold());
        assert!(
            matches!(decision, ReconcileDecision::Update { .. }),
            "expected Update, got {decision:?}"
        );
    }

    #[test]
    fn supersede_when_incoming_longer() {
        // Existing and incoming share the same core tokens AND incoming is much longer → Supersede.
        // Use low threshold so the shared tokens register above the similarity floor.
        let existing = "The team chose Rust for performance and safety requirements";
        let incoming = "The team chose Rust for performance and safety requirements and additionally \
                        for memory safety zero-cost abstractions type system and lack of garbage \
                        collector which was critical for audio processing latency requirements";
        let record = make_candidate("a", existing);
        let decision = reconcile(incoming, &[&record], &cfg_on_low_threshold());
        assert!(
            matches!(decision, ReconcileDecision::Supersede { .. }),
            "expected Supersede, got {decision:?}"
        );
    }

    #[test]
    fn jaccard_symmetric() {
        let a = tokenize("rust is great");
        let b = tokenize("great is rust");
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_disjoint_is_zero() {
        let a = tokenize("apple banana cherry");
        let b = tokenize("dog elephant fox");
        assert!(jaccard(&a, &b) < 1e-6);
    }
}
