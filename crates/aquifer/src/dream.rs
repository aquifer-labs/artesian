// SPDX-License-Identifier: Apache-2.0

//! Dreams engine — OCF bundle-to-bundle memory consolidation.
//!
//! A **dream** is a read-only, bundle-to-bundle operation: it reads the committed state (a
//! collection of [`MemoryRecord`]s plus access signals), runs promotion ranking and optional LLM
//! consolidation, and writes a *new* OCF bundle to `--out`. The source collection is **never
//! mutated** — input records are immutable. Every promote / merge / drop / supersede / decay
//! decision is logged as a line in `qualify.jsonl`.
//!
//! ## Promotion ranking
//!
//! Records are ranked by a weighted combination of:
//! - **frequency** (`access_count`) — how often the record has been retrieved.
//! - **recency of access** (`last_access`) — more recently accessed records score higher.
//! - **recency of creation** (`created_at`) — newer records preferred over stale ones.
//! - **consolidation** — records surviving a dedup pass score a bonus.
//! - **conceptual richness** — length-normalised as a proxy for information density.
//!
//! Records scoring above a configurable threshold are admitted to the output bundle with
//! decision `"admit"`. Those below are logged as `"reject"`. Records superseded by a merged
//! claim carry `"supersede"`. The no-LLM path emits `"admit"`/`"reject"` only (no merging).
//!
//! ## No-LLM fallback
//!
//! When no LLM consolidation callback is provided, the engine skips the merge pass and
//! prints a one-line note. It still produces a valid, complete OCF bundle — the qualify log
//! records `"admit"` / `"reject"` decisions based on the deterministic scoring alone.
//!
//! ## Usage
//!
//! ```no_run
//! # use aquifer::dream::{dream, DreamOptions};
//! # use aquifer::MemoryRecord;
//! # let records: Vec<MemoryRecord> = vec![];
//! let result = dream(&records, &DreamOptions::default(), None)
//!     .expect("dream should succeed");
//! println!("dream: {} admitted, {} rejected",
//!     result.admitted, result.rejected);
//! ```

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    consolidation::{consolidation_pass, ConsolidationOptions},
    MemoryRecord, MemoryTier,
};

// ── OCF on-disk constants ────────────────────────────────────────────────────

const OCF_VERSION: &str = "0.1";
const MANIFEST_FILE: &str = "manifest.json";
const SCHEMA_FILE: &str = "schema.json";
const SNAPSHOT_FILE: &str = "snapshot.json";
const QUALIFY_FILE: &str = "qualify.jsonl";
const DIARY_FILE: &str = "DREAMS.md";

/// The slot name used for all promoted records in the output snapshot.
const DREAM_SLOT: &str = "dream";

// ── public types ─────────────────────────────────────────────────────────────

/// A qualify-log decision value — the `decision` field in each `qualify.jsonl` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DreamDecision {
    /// Record promoted into the output bundle.
    Admit,
    /// Record excluded (score below threshold or superseded by a merged claim).
    Reject,
    /// Record promoted after being merged with one or more near-duplicates.
    Merge,
    /// Record dropped because it was absorbed into a merged claim.
    Supersede,
    /// Record dropped due to staleness (low access + old creation date).
    Decay,
}

/// One line of `qualify.jsonl` written by the dreams engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamQualifyRecord {
    pub ts: DateTime<Utc>,
    /// The `node_id` of the source record (or merged claim ID).
    pub unit_ref: String,
    /// The promote/reject decision.
    pub decision: DreamDecision,
    /// Composite score used to make the decision.
    pub score: f32,
    /// Human-readable rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One entry in the output snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamSnapshotEntry {
    pub id: String,
    pub slot: String,
    pub content: String,
    pub tokens: usize,
    pub score: f32,
    pub committed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_access: Option<DateTime<Utc>>,
    #[serde(default)]
    pub access_count: u32,
}

/// Result of a dream run.
#[derive(Debug, Clone)]
pub struct DreamResult {
    /// Number of records admitted into the output bundle.
    pub admitted: usize,
    /// Number of records rejected (score below threshold or superseded).
    pub rejected: usize,
    /// Whether the LLM consolidation pass ran (vs deterministic-only fallback).
    pub llm_ran: bool,
    /// All qualify-log entries (decisions).
    pub qualify: Vec<DreamQualifyRecord>,
    /// The admitted snapshot entries.
    pub entries: Vec<DreamSnapshotEntry>,
}

/// Options for the dreams engine.
#[derive(Debug, Clone)]
pub struct DreamOptions {
    /// Jaccard similarity threshold for the dedup/consolidation grouping pass.
    pub similarity_threshold: f32,
    /// Records scoring at or above this value are admitted (0.0–1.0).
    pub admit_threshold: f32,
    /// Weight for `access_count` in the composite score (0.0 = disabled).
    pub weight_access_count: f32,
    /// Weight for `last_access` recency in the composite score.
    pub weight_recency: f32,
    /// Weight for creation-date freshness.
    pub weight_freshness: f32,
    /// Bonus applied to records that survive consolidation dedup.
    pub weight_consolidation: f32,
    /// Weight for content length (information richness proxy, log-scaled).
    pub weight_richness: f32,
    /// Write a human-readable narrative (`DREAMS.md`) alongside the OCF files.
    pub diary: bool,
    /// Name to use for the dream collection source in qualify records.
    pub source_label: String,
}

impl Default for DreamOptions {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.6,
            admit_threshold: 0.3,
            weight_access_count: 0.35,
            weight_recency: 0.25,
            weight_freshness: 0.15,
            weight_consolidation: 0.15,
            weight_richness: 0.10,
            diary: false,
            source_label: "artesian-dream".to_string(),
        }
    }
}

// ── OCF on-disk structs (private, minimal) ───────────────────────────────────

#[derive(Serialize)]
struct OcfManifest<'a> {
    ocf_version: &'a str,
    created: DateTime<Utc>,
    unit_source: &'a str,
}

#[derive(Serialize)]
struct OcfSlot<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

#[derive(Serialize)]
struct OcfSchema<'a> {
    ocf_version: &'a str,
    slots: Vec<OcfSlot<'a>>,
    budget_tokens: usize,
    eviction: &'a str,
}

#[derive(Serialize)]
struct OcfSnapshot<'a> {
    budget_tokens: usize,
    token_count: usize,
    saturation: f32,
    entries: &'a [DreamSnapshotEntry],
}

// ── composite scorer ─────────────────────────────────────────────────────────

/// Compute a composite [0, 1] promotion score for a `MemoryRecord`.
/// All sub-scores are normalised independently; the composite is a weighted sum
/// clamped to [0, 1].
fn composite_score(record: &MemoryRecord, opts: &DreamOptions, now: DateTime<Utc>) -> f32 {
    // access_count: log-scaled, soft-capped at 50 accesses → 1.0
    let access_score = if opts.weight_access_count > 0.0 {
        let count = record.access_count as f32;
        (count / 50.0).min(1.0).sqrt()
    } else {
        0.0
    };

    // last_access recency: exponential decay over 30 days
    let recency_score = if opts.weight_recency > 0.0 {
        match record.last_access {
            Some(last) => {
                let age_days =
                    now.signed_duration_since(last).num_seconds().max(0) as f32 / 86_400.0;
                (-age_days / 30.0_f32).exp()
            }
            None => 0.0, // never accessed — no recency signal
        }
    } else {
        0.0
    };

    // created_at freshness: exponential decay over 90 days
    let freshness_score = if opts.weight_freshness > 0.0 {
        let age_days = now
            .signed_duration_since(record.created_at)
            .num_seconds()
            .max(0) as f32
            / 86_400.0;
        (-age_days / 90.0_f32).exp()
    } else {
        0.0
    };

    // richness: log-scaled content length, soft-capped at 1 000 chars → 1.0
    let richness_score = if opts.weight_richness > 0.0 {
        let chars = record.content.len() as f32;
        (chars / 1_000.0).min(1.0).sqrt()
    } else {
        0.0
    };

    let raw = opts.weight_access_count * access_score
        + opts.weight_recency * recency_score
        + opts.weight_freshness * freshness_score
        + opts.weight_richness * richness_score;

    raw.clamp(0.0, 1.0)
}

// ── dream entry-point ────────────────────────────────────────────────────────

/// Optional LLM merge callback: receives consolidated content, returns rewritten content.
pub type LlmMergeFn = dyn Fn(&str) -> Option<String>;

/// Run a dream over `records`.
///
/// `llm_merge` is an optional callback: given a consolidated content string, it may rewrite
/// it (e.g. via an LLM summariser). When `None`, the merge pass is skipped and a note is
/// printed to stderr.
pub fn dream(
    records: &[MemoryRecord],
    opts: &DreamOptions,
    llm_merge: Option<&LlmMergeFn>,
) -> Result<DreamResult, DreamError> {
    if records.is_empty() {
        return Ok(DreamResult {
            admitted: 0,
            rejected: 0,
            llm_ran: false,
            qualify: Vec::new(),
            entries: Vec::new(),
        });
    }

    let now = Utc::now();
    let llm_ran;

    // Step 1: deterministic dedup/consolidation pass.
    let consolidation = consolidation_pass(
        records,
        &ConsolidationOptions {
            similarity_threshold: opts.similarity_threshold,
            source_label: opts.source_label.clone(),
            ..Default::default()
        },
    );

    // Build a quick lookup: source node_id → group index (for supersede decisions).
    // Each claim carries `source_ids` (the node_ids of the records it absorbed).
    let mut superseded: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Claims that absorbed > 1 source have the extras as superseded.
    for claim in &consolidation.claims {
        if claim.source_ids.len() > 1 {
            // The canonical source is the one that became the claim; the rest are superseded.
            // We mark all but the first as superseded (arbitrary but consistent).
            for sid in claim.source_ids.iter().skip(1) {
                superseded.insert(sid.clone());
            }
        }
    }

    // Step 2: score each record.
    let mut scored: Vec<(&MemoryRecord, f32)> = records
        .iter()
        .map(|r| {
            let mut score = composite_score(r, opts, now);
            // Consolidation bonus: record survived dedup → it's the canonical representative.
            let survived = consolidation
                .claims
                .iter()
                .any(|claim| claim.source_ids.contains(&r.node_id));
            if survived {
                score = (score + opts.weight_consolidation).min(1.0);
            }
            (r, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Step 3: optional LLM merge pass on admitted claims.
    // We run it on the consolidated claim content, not the raw records.
    let mut merged_contents: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Some(merge_fn) = llm_merge {
        llm_ran = true;
        for claim in &consolidation.claims {
            if let Some(rewritten) = merge_fn(&claim.content) {
                for sid in &claim.source_ids {
                    merged_contents.insert(sid.clone(), rewritten.clone());
                }
            }
        }
    } else {
        eprintln!(
            "note: artesian dream — no LLM configured; skipping LLM synthesis pass. \
             Deterministic promotion only."
        );
        llm_ran = false;
    }

    // Step 4: build qualify log + snapshot entries.
    let mut qualify: Vec<DreamQualifyRecord> = Vec::new();
    let mut entries: Vec<DreamSnapshotEntry> = Vec::new();
    let mut admitted = 0usize;
    let mut rejected = 0usize;

    for (record, score) in &scored {
        let unit_ref = record.node_id.clone();
        let ts = now;

        if superseded.contains(&record.node_id) {
            qualify.push(DreamQualifyRecord {
                ts,
                unit_ref,
                decision: DreamDecision::Supersede,
                score: *score,
                reason: Some("absorbed into a merged claim by consolidation pass".to_string()),
            });
            rejected += 1;
            continue;
        }

        // Decay: very low score AND tier is L0Raw (raw, unprocessed).
        if *score < opts.admit_threshold * 0.5 && record.tier == MemoryTier::L0Raw {
            qualify.push(DreamQualifyRecord {
                ts,
                unit_ref,
                decision: DreamDecision::Decay,
                score: *score,
                reason: Some("score below decay floor for raw-tier records".to_string()),
            });
            rejected += 1;
            continue;
        }

        if *score >= opts.admit_threshold {
            let content = merged_contents
                .get(&record.node_id)
                .cloned()
                .unwrap_or_else(|| record.content.clone());
            let tokens = content.len() / 4 + 1;
            let decision = if merged_contents.contains_key(&record.node_id) {
                DreamDecision::Merge
            } else {
                DreamDecision::Admit
            };
            qualify.push(DreamQualifyRecord {
                ts,
                unit_ref: unit_ref.clone(),
                decision,
                score: *score,
                reason: None,
            });
            entries.push(DreamSnapshotEntry {
                id: record.id.to_string(),
                slot: DREAM_SLOT.to_string(),
                content,
                tokens,
                score: *score,
                committed_at: now,
                unit_ref: Some(unit_ref),
                last_access: record.last_access,
                access_count: record.access_count,
            });
            admitted += 1;
        } else {
            qualify.push(DreamQualifyRecord {
                ts,
                unit_ref,
                decision: DreamDecision::Reject,
                score: *score,
                reason: Some(format!(
                    "score {:.3} below admit threshold {:.3}",
                    score, opts.admit_threshold
                )),
            });
            rejected += 1;
        }
    }

    Ok(DreamResult {
        admitted,
        rejected,
        llm_ran,
        qualify,
        entries,
    })
}

/// Write a dream result to an OCF bundle directory.
///
/// The directory is created if it does not exist. The source collection is identified by
/// `collection_name` (informational only — no mutation of source records).
///
/// If `diary = true` (per `opts`), also writes `DREAMS.md` — a human-readable narrative
/// summarising the dream. The diary does NOT feed promotion decisions.
pub fn write_dream_bundle(
    result: &DreamResult,
    opts: &DreamOptions,
    out_dir: &Path,
    collection_name: &str,
) -> Result<(), DreamError> {
    std::fs::create_dir_all(out_dir)?;

    let budget_tokens: usize = result.entries.iter().map(|e| e.tokens).sum();
    let saturation = if budget_tokens == 0 {
        0.0
    } else {
        result.entries.len() as f32 / budget_tokens.max(1) as f32
    };

    // manifest.json
    let manifest = OcfManifest {
        ocf_version: OCF_VERSION,
        created: Utc::now(),
        unit_source: collection_name,
    };
    std::fs::write(
        out_dir.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    // schema.json
    let schema = OcfSchema {
        ocf_version: OCF_VERSION,
        slots: vec![OcfSlot {
            name: DREAM_SLOT,
            description: Some("promoted memory record from dream pass"),
        }],
        budget_tokens: budget_tokens.max(1),
        eviction: "score-lru",
    };
    std::fs::write(
        out_dir.join(SCHEMA_FILE),
        serde_json::to_string_pretty(&schema)?,
    )?;

    // snapshot.json
    let snapshot = OcfSnapshot {
        budget_tokens: budget_tokens.max(1),
        token_count: budget_tokens,
        saturation,
        entries: &result.entries,
    };
    std::fs::write(
        out_dir.join(SNAPSHOT_FILE),
        serde_json::to_string_pretty(&snapshot)?,
    )?;

    // qualify.jsonl
    let mut jsonl = String::new();
    for record in &result.qualify {
        jsonl.push_str(&serde_json::to_string(record)?);
        jsonl.push('\n');
    }
    std::fs::write(out_dir.join(QUALIFY_FILE), jsonl)?;

    // DREAMS.md — optional human-readable diary.
    if opts.diary {
        let diary = render_diary(result, collection_name);
        std::fs::write(out_dir.join(DIARY_FILE), diary)?;
    }

    Ok(())
}

/// Render a human-readable narrative of the dream (for `--diary`).
/// The diary is informational only — it plays no role in promotion decisions.
pub fn render_diary(result: &DreamResult, collection_name: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Dream Report — {}",
        Utc::now().format("%Y-%m-%d %H:%M UTC")
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "**Source collection:** `{collection_name}`");
    let _ = writeln!(
        out,
        "**LLM synthesis:** {}",
        if result.llm_ran {
            "yes"
        } else {
            "no (deterministic only)"
        }
    );
    let _ = writeln!(
        out,
        "**Admitted:** {} · **Rejected:** {}",
        result.admitted, result.rejected
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "## Promoted entries");
    for entry in &result.entries {
        let snippet = entry
            .content
            .chars()
            .take(120)
            .collect::<String>()
            .replace('\n', " ");
        let ellipsis = if entry.content.len() > 120 { "…" } else { "" };
        let _ = writeln!(
            out,
            "- **{}** (score {:.2}, {} accesses): {snippet}{}",
            entry.id,
            entry.score,
            entry.access_count,
            ellipsis,
        );
    }
    if result.admitted == 0 {
        let _ = writeln!(out, "*(no entries promoted)*");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Decision log ({} entries)", result.qualify.len());
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for q in &result.qualify {
        *counts
            .entry(format!("{:?}", q.decision).to_lowercase())
            .or_default() += 1;
    }
    for (decision, count) in &counts {
        let _ = writeln!(out, "- `{decision}`: {count}");
    }
    out
}

/// Errors from the dreams engine.
#[derive(Debug, thiserror::Error)]
pub enum DreamError {
    #[error("I/O error writing bundle: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialisation error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;

    use super::*;
    use crate::{MemoryId, MemoryRecord, MemoryTier};

    fn make_record(id: &str, content: &str) -> MemoryRecord {
        MemoryRecord::new(
            MemoryId::new(id),
            format!("node:{id}"),
            content,
            vec!["test".to_string()],
            BTreeMap::new(),
            MemoryTier::L1Atom,
        )
    }

    fn make_accessed_record(id: &str, content: &str, count: u32) -> MemoryRecord {
        let mut r = make_record(id, content);
        r.access_count = count;
        r.last_access = Some(Utc::now());
        r
    }

    /// A dream over an empty slice must succeed and produce an empty result.
    #[test]
    fn dream_empty_input_returns_empty_result() {
        let result = dream(&[], &DreamOptions::default(), None).unwrap();
        assert_eq!(result.admitted, 0);
        assert_eq!(result.rejected, 0);
        assert!(result.qualify.is_empty());
    }

    /// Frequently accessed records score above zero and can be admitted.
    #[test]
    fn dream_high_access_record_is_admitted() {
        let record = make_accessed_record("popular", "The team chose Rust", 30);
        let opts = DreamOptions {
            admit_threshold: 0.1,
            ..Default::default()
        };
        let result = dream(&[record], &opts, None).unwrap();
        assert_eq!(result.admitted, 1);
        assert_eq!(result.rejected, 0);
        assert!(!result.llm_ran);
    }

    /// Records with no access signals score low and may be rejected.
    #[test]
    fn dream_zero_access_record_may_be_rejected() {
        let record = make_record("cold", "Python is a language");
        let opts = DreamOptions {
            admit_threshold: 0.5,  // high threshold so zero-access records fail
            weight_freshness: 0.0, // disable freshness so only access signals matter
            weight_richness: 0.0,
            weight_consolidation: 0.0,
            ..Default::default()
        };
        let result = dream(&[record], &opts, None).unwrap();
        // Zero access count + no last_access → access_score = 0, recency_score = 0
        // → score = 0.0 < 0.5 → rejected.
        assert_eq!(result.rejected, 1);
        assert_eq!(result.admitted, 0);
    }

    /// Source records are never mutated by a dream run.
    #[test]
    fn dream_does_not_mutate_source_records() {
        let original = make_accessed_record("r1", "Rust was chosen", 5);
        let original_access_count = original.access_count;
        let original_content = original.content.clone();
        let _ = dream(std::slice::from_ref(&original), &DreamOptions::default(), None).unwrap();
        // The record passed in is unchanged (we verify the value we still hold).
        assert_eq!(original.access_count, original_access_count);
        assert_eq!(original.content, original_content);
    }

    /// A dream must produce qualify lines covering every input record.
    #[test]
    fn dream_qualify_covers_all_records() {
        let records: Vec<MemoryRecord> = (0..5)
            .map(|i| make_accessed_record(&format!("r{i}"), &format!("fact {i}"), i as u32 * 3))
            .collect();
        let opts = DreamOptions {
            admit_threshold: 0.1,
            ..Default::default()
        };
        let result = dream(&records, &opts, None).unwrap();
        assert_eq!(
            result.qualify.len(),
            records.len(),
            "qualify log must cover every source record"
        );
        assert_eq!(result.admitted + result.rejected, records.len());
    }

    /// The no-LLM fallback must produce a bundle (not fail) and set llm_ran = false.
    #[test]
    fn dream_no_llm_fallback_succeeds() {
        let records = vec![
            make_accessed_record("a", "the team chose Rust", 10),
            make_accessed_record("b", "Python scripting layer", 2),
        ];
        let result = dream(&records, &DreamOptions::default(), None).unwrap();
        assert!(!result.llm_ran);
        // Should still produce some output.
        assert_eq!(result.admitted + result.rejected, 2);
    }

    /// `write_dream_bundle` must create all four OCF files.
    #[test]
    fn dream_bundle_writes_four_ocf_files() {
        use artesian_test_support::TempDir;
        let dir = TempDir::new("dream-bundle");
        let records = vec![make_accessed_record(
            "rec1",
            "chose Rust for performance",
            8,
        )];
        let opts = DreamOptions {
            admit_threshold: 0.1,
            diary: true,
            ..Default::default()
        };
        let result = dream(&records, &opts, None).unwrap();
        write_dream_bundle(&result, &opts, dir.path(), "test-collection").unwrap();

        assert!(dir.join("manifest.json").exists(), "manifest.json missing");
        assert!(dir.join("schema.json").exists(), "schema.json missing");
        assert!(dir.join("snapshot.json").exists(), "snapshot.json missing");
        assert!(dir.join("qualify.jsonl").exists(), "qualify.jsonl missing");
        assert!(
            dir.join("DREAMS.md").exists(),
            "DREAMS.md missing (--diary)"
        );

        // qualify.jsonl must be non-empty and contain the `decision` field.
        let qualify_text = std::fs::read_to_string(dir.join("qualify.jsonl")).unwrap();
        assert!(
            qualify_text.contains("\"decision\""),
            "qualify.jsonl must contain 'decision' field"
        );
    }

    /// The `--diary` flag must write a DREAMS.md that does NOT contain raw qualify-line JSON.
    /// Its content is purely human-readable narrative.
    #[test]
    fn diary_is_human_readable_not_json() {
        let records = vec![make_accessed_record(
            "x",
            "The Rust compiler guarantees safety",
            5,
        )];
        let opts = DreamOptions {
            diary: true,
            admit_threshold: 0.1,
            ..Default::default()
        };
        let result = dream(&records, &opts, None).unwrap();
        let diary = render_diary(&result, "my-collection");
        assert!(diary.contains("Dream Report"), "diary should have a title");
        assert!(
            !diary.contains("{\"ts\""),
            "diary must not contain raw JSON qualify lines"
        );
    }

    /// Near-duplicate records should produce supersede decisions in the qualify log.
    #[test]
    fn dream_supersedes_near_duplicates() {
        let records = vec![
            make_accessed_record("dup-a", "the team chose Rust for the core crate", 5),
            make_accessed_record("dup-b", "the team chose Rust for the core crates", 3),
        ];
        let opts = DreamOptions {
            similarity_threshold: 0.6,
            admit_threshold: 0.1,
            ..Default::default()
        };
        let result = dream(&records, &opts, None).unwrap();
        let has_supersede = result
            .qualify
            .iter()
            .any(|q| q.decision == DreamDecision::Supersede);
        assert!(
            has_supersede,
            "near-duplicate records should produce a supersede decision"
        );
    }
}
