// SPDX-License-Identifier: Apache-2.0

//! Memory eviction: soft-archive and hard-delete with auditable forgetting log.
//!
//! Eviction is **non-destructive by default**: records matching the policy are soft-archived
//! (`state = Archived`) rather than deleted. A subsequent `--hard` pass permanently removes
//! records that are already `Archived`. Every decision is appended to an eviction log
//! (`~/.artesian/eviction.jsonl`) so forgetting is auditable.
//!
//! ## Policies (applied in combination; any matching record is evicted)
//! - `--ttl-days N`: archive low-utility records whose `last_access` (or `created_at` if never
//!   accessed) is older than N days; high downstream-use records are protected from age-only TTL.
//! - `--lru`: archive records with the lowest utility-weighted retrieval_strength (computed with
//!   the default [`DecayConfig`]).
//! - `--min-score S`: archive records whose stored search score (from prior find) is below S.
//!   Since `MemoryRecord` has no stored score, this is applied to the decay-adjusted composite.
//! - `--max-keep N`: after other policies, if more than N Active records remain, archive the
//!   lowest-strength ones until only N remain.
//!
//! The caller passes all currently-loaded active records; the function returns the decisions
//! without mutating storage — the caller writes back.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    decay::{retrieval_strength, DecayConfig},
    MemoryRecord, MemoryState,
};

/// One line of the eviction audit log (`~/.artesian/eviction.jsonl`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvictionLogEntry {
    pub ts: DateTime<Utc>,
    pub record_id: String,
    pub node_id: String,
    pub action: EvictionAction,
    pub reason: String,
    /// Retrieval strength at eviction time (diagnostic).
    pub retrieval_strength: f32,
}

/// The action taken on a record during an eviction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvictionAction {
    /// Record moved to `Archived` state (soft-delete).
    Archive,
    /// Record permanently deleted (only applies to already-Archived records with `--hard`).
    Delete,
}

/// Policy options for a single eviction pass.
#[derive(Debug, Clone, Default)]
pub struct EvictionPolicy {
    /// Archive records whose last-access (or created_at) is older than this many days.
    pub ttl_days: Option<f32>,
    /// Archive records with the lowest retrieval strength (LRU-style).
    pub lru: bool,
    /// Archive records whose decay-adjusted strength is below this threshold.
    pub min_strength: Option<f32>,
    /// After all other policies, archive lowest-strength records until at most this many remain.
    pub max_keep: Option<usize>,
    /// When `true`, permanently delete records that are already `Archived` (hard delete).
    /// Does not archive additional records — only cleans up prior soft-archives.
    pub hard: bool,
    /// Decay configuration for strength computation. Defaults to [`DecayConfig::default()`].
    pub decay_config: DecayConfig,
}

/// Result of an eviction pass.
#[derive(Debug, Clone, Default)]
pub struct EvictionReport {
    pub archived: usize,
    pub deleted: usize,
    pub log_entries: Vec<EvictionLogEntry>,
}

/// Run one eviction pass over `records`, returning the decisions without mutating storage.
///
/// - Active records matching any soft-archive policy → `Archive`.
/// - Already-Archived records when `policy.hard` → `Delete`.
/// - Returns the full set of log entries and counts.
pub fn evict(records: &[MemoryRecord], policy: &EvictionPolicy) -> EvictionReport {
    let now = Utc::now();
    let mut log_entries: Vec<EvictionLogEntry> = Vec::new();
    let mut archived_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // Phase 1: hard-delete already-archived records if requested.
    if policy.hard {
        for record in records {
            if record.state == MemoryState::Archived {
                let strength = retrieval_strength(record, &policy.decay_config, now);
                log_entries.push(EvictionLogEntry {
                    ts: now,
                    record_id: record.id.to_string(),
                    node_id: record.node_id.clone(),
                    action: EvictionAction::Delete,
                    reason: "hard delete of already-archived record".to_string(),
                    retrieval_strength: strength,
                });
            }
        }
    }

    // Phase 2: soft-archive Active records matching any policy.
    let active_records: Vec<&MemoryRecord> = records
        .iter()
        .filter(|r| r.state == MemoryState::Active)
        .collect();

    // Compute strength for all active records once.
    let mut scored: Vec<(&MemoryRecord, f32)> = active_records
        .iter()
        .map(|r| (*r, retrieval_strength(r, &policy.decay_config, now)))
        .collect();

    // TTL policy
    if let Some(ttl_days) = policy.ttl_days {
        for (record, strength) in &scored {
            let age_days = age_in_days(record, now);
            if age_days > ttl_days
                && !utility_protected(record, *strength)
                && archived_ids.insert(record.id.to_string())
            {
                log_entries.push(EvictionLogEntry {
                    ts: now,
                    record_id: record.id.to_string(),
                    node_id: record.node_id.clone(),
                    action: EvictionAction::Archive,
                    reason: format!("TTL: age {age_days:.1}d > ttl {ttl_days}d"),
                    retrieval_strength: *strength,
                });
            }
        }
    }

    // Min-strength policy
    if let Some(min_strength) = policy.min_strength {
        for (record, strength) in &scored {
            if *strength < min_strength && archived_ids.insert(record.id.to_string()) {
                log_entries.push(EvictionLogEntry {
                    ts: now,
                    record_id: record.id.to_string(),
                    node_id: record.node_id.clone(),
                    action: EvictionAction::Archive,
                    reason: format!(
                        "min-strength: strength {strength:.3} < threshold {min_strength:.3}"
                    ),
                    retrieval_strength: *strength,
                });
            }
        }
    }

    // LRU / max-keep: sort by strength ascending so the weakest are evicted first.
    if policy.lru || policy.max_keep.is_some() {
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    }

    // LRU: archive the bottom half (up to 50%) by default when enabled without max_keep.
    // Callers may override via max_keep. If both are set, max_keep wins.
    if policy.lru && policy.max_keep.is_none() {
        let evict_count = (scored.len() / 2).max(1).min(scored.len());
        for (record, strength) in scored.iter().take(evict_count) {
            if archived_ids.insert(record.id.to_string()) {
                log_entries.push(EvictionLogEntry {
                    ts: now,
                    record_id: record.id.to_string(),
                    node_id: record.node_id.clone(),
                    action: EvictionAction::Archive,
                    reason: format!("LRU: lowest retrieval strength {strength:.3}"),
                    retrieval_strength: *strength,
                });
            }
        }
    }

    // Max-keep: if remaining Active records (not yet marked for archive) exceed the cap,
    // archive the weakest until at most max_keep remain.
    if let Some(max_keep) = policy.max_keep {
        let remaining_active = scored
            .iter()
            .filter(|(r, _)| !archived_ids.contains(r.id.as_str()))
            .count();
        if remaining_active > max_keep {
            let to_evict = remaining_active - max_keep;
            let mut evicted = 0usize;
            for (record, strength) in scored.iter() {
                if evicted >= to_evict {
                    break;
                }
                if archived_ids.contains(record.id.as_str()) {
                    continue;
                }
                archived_ids.insert(record.id.to_string());
                log_entries.push(EvictionLogEntry {
                    ts: now,
                    record_id: record.id.to_string(),
                    node_id: record.node_id.clone(),
                    action: EvictionAction::Archive,
                    reason: format!(
                        "max-keep: {remaining_active} active > {max_keep} cap, strength={strength:.3}"
                    ),
                    retrieval_strength: *strength,
                });
                evicted += 1;
            }
        }
    }

    let archived = log_entries
        .iter()
        .filter(|e| e.action == EvictionAction::Archive)
        .count();
    let deleted = log_entries
        .iter()
        .filter(|e| e.action == EvictionAction::Delete)
        .count();

    EvictionReport {
        archived,
        deleted,
        log_entries,
    }
}

/// Age of a record in days: time since `last_access` if set, else time since `created_at`.
fn age_in_days(record: &MemoryRecord, now: DateTime<Utc>) -> f32 {
    let ref_time = record.last_access.unwrap_or(record.created_at);
    now.signed_duration_since(ref_time).num_seconds().max(0) as f32 / 86_400.0
}

fn utility_protected(record: &MemoryRecord, strength: f32) -> bool {
    record.useful_count > 0 && strength >= 1.0
}

/// Append eviction log entries to the default audit log path
/// (`~/.artesian/eviction.jsonl`), creating the directory if needed.
///
/// On failure, the function returns the error rather than silently swallowing it,
/// so the caller can decide whether to treat it as fatal.
pub fn append_eviction_log(entries: &[EvictionLogEntry]) -> std::io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(&home).join(".artesian");
    std::fs::create_dir_all(&dir)?;
    let log_path = dir.join("eviction.jsonl");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    for entry in entries {
        let line =
            serde_json::to_string(entry).map_err(|e| std::io::Error::other(e.to_string()))?;
        writeln!(file, "{line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{Duration, Utc};

    use super::*;
    use crate::{MemoryId, MemoryTier};

    fn make_record(id: &str, last_access_days_ago: Option<f32>, count: u32) -> MemoryRecord {
        let mut r = MemoryRecord::new(
            MemoryId::new(id),
            format!("node:{id}"),
            format!("content for {id}"),
            Vec::new(),
            BTreeMap::new(),
            MemoryTier::L1Atom,
        );
        r.access_count = count;
        r.last_access = last_access_days_ago
            .map(|days| Utc::now() - Duration::seconds((days * 86_400.0) as i64));
        r
    }

    fn make_useful_record(
        id: &str,
        last_access_days_ago: Option<f32>,
        useful: u32,
    ) -> MemoryRecord {
        let mut record = make_record(id, last_access_days_ago, 0);
        record.useful_count = useful;
        record
    }

    fn make_archived(id: &str) -> MemoryRecord {
        let mut r = make_record(id, Some(200.0), 0);
        r.state = MemoryState::Archived;
        r
    }

    #[test]
    fn ttl_archives_old_records() {
        let old = make_record("old", Some(100.0), 0);
        let fresh = make_record("fresh", Some(1.0), 5);
        let policy = EvictionPolicy {
            ttl_days: Some(30.0),
            ..EvictionPolicy::default()
        };
        let report = evict(&[old, fresh], &policy);
        assert_eq!(report.archived, 1);
        assert!(report.log_entries.iter().any(|e| e.record_id == "old"));
        assert!(!report.log_entries.iter().any(|e| e.record_id == "fresh"));
    }

    #[test]
    fn lru_archives_weakest() {
        let weak = make_record("weak", Some(120.0), 0);
        let strong = make_record("strong", Some(1.0), 20);
        let policy = EvictionPolicy {
            lru: true,
            ..EvictionPolicy::default()
        };
        let report = evict(&[weak.clone(), strong], &policy);
        // LRU should archive at least the weakest record
        assert!(report.archived >= 1);
        assert!(report.log_entries.iter().any(|e| e.record_id == "weak"));
    }

    #[test]
    fn hard_deletes_archived_records() {
        let archived = make_archived("old-archived");
        let active = make_record("active", Some(1.0), 5);
        let policy = EvictionPolicy {
            hard: true,
            ..EvictionPolicy::default()
        };
        let report = evict(&[archived, active], &policy);
        assert_eq!(report.deleted, 1);
        assert_eq!(report.archived, 0); // hard does not re-archive
        assert!(report
            .log_entries
            .iter()
            .any(|e| e.record_id == "old-archived" && e.action == EvictionAction::Delete));
    }

    #[test]
    fn max_keep_archives_excess() {
        let records: Vec<MemoryRecord> = (0..5)
            .map(|i| make_record(&format!("r{i}"), Some((i as f32 + 1.0) * 20.0), 0))
            .collect();
        let policy = EvictionPolicy {
            max_keep: Some(2),
            ..EvictionPolicy::default()
        };
        let report = evict(&records, &policy);
        assert_eq!(report.archived, 3, "should archive 3 to keep only 2");
    }

    #[test]
    fn utility_weighted_eviction_keeps_old_useful_record() {
        let old_useful = make_useful_record("old-useful", Some(180.0), 10);
        let fresh_unused = make_record("fresh-unused", Some(1.0), 0);
        let policy = EvictionPolicy {
            max_keep: Some(1),
            ..EvictionPolicy::default()
        };

        let report = evict(&[old_useful, fresh_unused], &policy);

        assert_eq!(report.archived, 1);
        assert!(report
            .log_entries
            .iter()
            .any(|entry| entry.record_id == "fresh-unused"));
        assert!(!report
            .log_entries
            .iter()
            .any(|entry| entry.record_id == "old-useful"));
    }

    #[test]
    fn ttl_does_not_prune_old_useful_record_by_age_alone() {
        let old_useful = make_useful_record("old-useful", Some(180.0), 10);
        let old_unused = make_record("old-unused", Some(180.0), 0);
        let policy = EvictionPolicy {
            ttl_days: Some(30.0),
            ..EvictionPolicy::default()
        };

        let report = evict(&[old_useful, old_unused], &policy);

        assert_eq!(report.archived, 1);
        assert!(report
            .log_entries
            .iter()
            .any(|entry| entry.record_id == "old-unused"));
        assert!(!report
            .log_entries
            .iter()
            .any(|entry| entry.record_id == "old-useful"));
    }

    #[test]
    fn archived_records_excluded_from_soft_archive() {
        // Already-archived records should not be double-counted by TTL.
        let archived = make_archived("old-already");
        let policy = EvictionPolicy {
            ttl_days: Some(1.0), // very short TTL
            ..EvictionPolicy::default()
        };
        let report = evict(&[archived], &policy);
        // The already-archived record is not an Active record → not touched by TTL.
        assert_eq!(report.archived, 0);
    }
}
