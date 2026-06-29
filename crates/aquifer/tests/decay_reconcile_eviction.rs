// SPDX-License-Identifier: Apache-2.0

//! Integration tests for:
//! - Part 1: Recency-aware soft-decay at retrieval (ranking)
//! - Part 2: Write-time reconciliation (ReconcileDecision)
//! - Part 3: Eviction with soft-archive (state=Archived)
//!
//! No live Qdrant or LLM. Uses FilesBackend (the canonical local backend).

use std::sync::Arc;

use aquifer::{
    decay::{retrieval_strength, DecayConfig},
    eviction::{evict, EvictionAction, EvictionPolicy},
    files_parse_record, files_render_record,
    reconcile::{reconcile, ReconcileConfig, ReconcileDecision},
    FilesBackend, MemoryBackend, MemoryId, MemoryQuery, MemoryRecord, MemoryState, MemoryTier,
    StoreMemory,
};
use artesian_test_support::TempDir;
use chrono::{Duration, Utc};
use std::collections::BTreeMap;

// ─── helpers ─────────────────────────────────────────────────────────────────

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
    r.last_access =
        last_access_days_ago.map(|days| Utc::now() - Duration::seconds((days * 86_400.0) as i64));
    r
}

// ─── Part 1: Recency-aware decay ranking ─────────────────────────────────────

/// A stale record with the same base score as a fresh one is downranked by decay.
#[test]
fn decay_downranks_stale_vs_fresh_same_base_score() {
    let config = DecayConfig::default();
    let now = Utc::now();

    let stale = make_record("stale", Some(120.0), 0); // 4× half-life ago, no accesses
    let fresh = make_record("fresh", Some(0.5), 8); // 12 hours ago, 8 accesses

    let base = 0.8f32;
    let stale_effective = base * retrieval_strength(&stale, &config, now);
    let fresh_effective = base * retrieval_strength(&fresh, &config, now);

    assert!(
        fresh_effective > stale_effective,
        "fresh ({fresh_effective:.3}) should outrank stale ({stale_effective:.3})"
    );
}

/// A record with no last_access returns strength 1.0 (neutral, not dampened).
#[test]
fn decay_neutral_for_no_last_access() {
    let record = make_record("new", None, 0); // never accessed
    let strength = retrieval_strength(&record, &DecayConfig::default(), Utc::now());
    // Must be exactly 1.0 for backward-compat
    assert!((strength - 1.0).abs() < 1e-6, "got {strength}");
}

/// A frequently-accessed recent record gets a boost above 1.0.
#[test]
fn decay_boost_for_high_access_count() {
    let mut record = make_record("popular", Some(0.0), 50);
    record.last_access = Some(Utc::now()); // just now
    let config = DecayConfig::default();
    let strength = retrieval_strength(&record, &config, Utc::now());
    assert!(
        strength > 1.3,
        "50 accesses + just now should give strength > 1.3, got {strength}"
    );
}

/// FilesBackend find respects decay: the fresh record should appear before the stale one
/// when both have equal keyword scores.
#[tokio::test]
async fn files_backend_find_orders_by_decay() {
    let dir = TempDir::new("decay-find-order");
    // Disable track_access so results aren't mutated under us.
    let backend = Arc::new(
        FilesBackend::new(dir.path())
            .with_track_access(false)
            // Ensure decay is active with a short half-life so 60-day-old records are clearly dampened.
            .with_decay_config(DecayConfig {
                decay_enabled: true,
                half_life_days: 10.0,
                floor: 0.1,
                access_boost: 1.5,
            }),
    );

    // Store two records with the same text (so same keyword score).
    let stored_stale = backend
        .store(StoreMemory::atom("Rust is the chosen language"))
        .await
        .expect("store stale");
    let stored_fresh = backend
        .store(StoreMemory::atom("Rust is the chosen programming language"))
        .await
        .expect("store fresh");

    // Manually rewrite their OKF files to inject last_access / access_count signals.
    let mem_dir = dir.path().join("memory");
    let mut found_stale = false;
    let mut found_fresh = false;

    // Walk and patch both files.
    for entry in walkdir::walkdir_blocking(&mem_dir) {
        let entry = entry.expect("dir entry");
        if entry.path().extension().is_some_and(|e| e == "md") {
            let text = std::fs::read_to_string(entry.path()).expect("read");
            if let Ok(mut rec) = files_parse_record(&text) {
                if rec.id == stored_stale.id {
                    rec.last_access = Some(Utc::now() - Duration::days(90));
                    rec.access_count = 0;
                    let rendered = files_render_record(&rec).expect("render");
                    std::fs::write(entry.path(), rendered).expect("write");
                    found_stale = true;
                } else if rec.id == stored_fresh.id {
                    rec.last_access = Some(Utc::now() - Duration::hours(1));
                    rec.access_count = 10;
                    let rendered = files_render_record(&rec).expect("render");
                    std::fs::write(entry.path(), rendered).expect("write");
                    found_fresh = true;
                }
            }
        }
    }

    assert!(
        found_stale && found_fresh,
        "both records should have been found and patched"
    );

    // Now query: both records share "Rust" "chosen" "language" → same base keyword score.
    let hits = backend
        .find(MemoryQuery::new("Rust chosen language").with_limit(10))
        .await
        .expect("find");
    assert!(
        hits.len() >= 2,
        "expected at least 2 hits, got {}",
        hits.len()
    );

    // Fresh should appear first.
    let first_id = &hits[0].record.id;
    assert_eq!(
        *first_id, stored_fresh.id,
        "fresh record should be ranked first by decay"
    );
}

// A small synchronous directory walker (no external dep)
mod walkdir {
    use std::path::PathBuf;

    pub struct DirEntry {
        path: PathBuf,
    }
    impl DirEntry {
        pub fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    pub fn walkdir_blocking(
        root: &std::path::Path,
    ) -> impl Iterator<Item = std::io::Result<DirEntry>> {
        let mut stack: Vec<std::path::PathBuf> = Vec::new();
        if root.exists() {
            stack.push(root.to_path_buf());
        }
        let mut files: Vec<PathBuf> = Vec::new();
        while let Some(dir) = stack.pop() {
            if let Ok(read) = std::fs::read_dir(&dir) {
                for entry in read.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        files.push(p);
                    }
                }
            }
        }
        files.into_iter().map(|p| Ok(DirEntry { path: p }))
    }
}

// ─── Part 2: Write-time reconciliation ───────────────────────────────────────

#[test]
fn reconcile_add_when_no_overlap() {
    let record = MemoryRecord {
        content: "The Eiffel tower is in Paris".to_string(),
        ..make_record("a", None, 0)
    };
    let incoming = "Rust is a systems programming language";
    let config = ReconcileConfig {
        reconcile_on_write: true,
        ..ReconcileConfig::default()
    };
    let decision = reconcile(incoming, &[&record], &config);
    assert_eq!(decision, ReconcileDecision::Add);
}

#[test]
fn reconcile_noop_for_identical_content() {
    let content = "The team chose Rust for the core crate";
    let existing = MemoryRecord {
        content: content.to_string(),
        ..make_record("x", None, 0)
    };
    let config = ReconcileConfig {
        reconcile_on_write: true,
        ..ReconcileConfig::default()
    };
    let decision = reconcile(content, &[&existing], &config);
    assert!(
        matches!(decision, ReconcileDecision::Noop { .. }),
        "expected Noop, got {decision:?}"
    );
}

#[test]
fn reconcile_update_for_similar_same_length() {
    let existing = MemoryRecord {
        content: "The team chose Rust for the backend because it is fast".to_string(),
        ..make_record("y", None, 0)
    };
    let incoming = "The team chose Rust for the backend because of performance";
    let config = ReconcileConfig {
        reconcile_on_write: true,
        ..ReconcileConfig::default()
    };
    let decision = reconcile(incoming, &[&existing], &config);
    assert!(
        matches!(decision, ReconcileDecision::Update { .. }),
        "expected Update, got {decision:?}"
    );
}

#[test]
fn reconcile_supersede_when_incoming_longer_and_overlapping() {
    // Existing: short record. Incoming: longer but shares enough tokens to be "similar"
    // above the threshold, and is substantially longer → Supersede.
    let existing = MemoryRecord {
        content: "The team chose Rust for performance and safety reasons".to_string(),
        ..make_record("z", None, 0)
    };
    // Incoming shares most tokens of existing AND adds much more — so Jaccard is above threshold.
    let incoming = "The team chose Rust for performance and safety reasons and additionally \
                    for memory safety zero-cost abstractions and the comprehensive type system \
                    and lack of garbage collector which was critical for the use case and \
                    performance requirements of the project";
    let config = ReconcileConfig {
        reconcile_on_write: true,
        similarity_threshold: 0.3, // lower threshold to catch this overlap pattern
        ..ReconcileConfig::default()
    };
    let decision = reconcile(incoming, &[&existing], &config);
    assert!(
        matches!(decision, ReconcileDecision::Supersede { .. }),
        "expected Supersede for much-longer incoming with overlap, got {decision:?}"
    );
}

#[test]
fn reconcile_disabled_always_add() {
    let existing = MemoryRecord {
        content: "The team chose Rust".to_string(),
        ..make_record("z2", None, 0)
    };
    // Even with identical content, disabled reconciliation → Add.
    let decision = reconcile(
        "The team chose Rust",
        &[&existing],
        &ReconcileConfig::default(),
    );
    assert_eq!(decision, ReconcileDecision::Add);
}

// ─── Part 3: Eviction with soft-archive ──────────────────────────────────────

/// state=Archived records are excluded from default find.
#[tokio::test]
async fn archived_records_excluded_from_default_find() {
    let dir = TempDir::new("evict-archived-find");
    let backend = Arc::new(FilesBackend::new(dir.path()).with_track_access(false));

    // Store one normal record.
    let stored = backend
        .store(StoreMemory::atom("Active memory content"))
        .await
        .expect("store active");

    // Manually write an archived record directly to disk.
    let archived_content = "Archived memory content";
    let mut archived = MemoryRecord::new(
        MemoryId::new("archived-001"),
        "node:archived-001",
        archived_content,
        Vec::new(),
        BTreeMap::new(),
        MemoryTier::L1Atom,
    );
    archived.state = MemoryState::Archived;
    let mem_dir = dir.path().join("memory");
    std::fs::create_dir_all(&mem_dir).expect("create dir");
    let rendered = files_render_record(&archived).expect("render archived");
    std::fs::write(mem_dir.join("archived-001.md"), rendered).expect("write archived");

    // Default find should NOT return the archived record.
    let hits = backend
        .find(MemoryQuery::new("").with_limit(100))
        .await
        .expect("find default");
    let ids: Vec<_> = hits.iter().map(|h| h.record.id.as_str()).collect();
    assert!(
        !ids.contains(&"archived-001"),
        "archived record should not appear in default find, got: {ids:?}"
    );
    assert!(
        ids.contains(&stored.id.as_str()),
        "active record should appear in default find"
    );
}

/// With include_archived = true, archived records are returned.
#[tokio::test]
async fn include_archived_flag_surfaces_archived_records() {
    let dir = TempDir::new("evict-include-archived");
    let backend = Arc::new(FilesBackend::new(dir.path()).with_track_access(false));

    let mut archived = MemoryRecord::new(
        MemoryId::new("arch-002"),
        "node:arch-002",
        "This is archived",
        Vec::new(),
        BTreeMap::new(),
        MemoryTier::L1Atom,
    );
    archived.state = MemoryState::Archived;
    let mem_dir = dir.path().join("memory");
    std::fs::create_dir_all(&mem_dir).expect("create dir");
    std::fs::write(
        mem_dir.join("arch-002.md"),
        files_render_record(&archived).expect("render"),
    )
    .expect("write");

    let mut query = MemoryQuery::new("").with_limit(100);
    query.include_archived = true;
    let hits = backend
        .find(query)
        .await
        .expect("find with include_archived");
    let ids: Vec<_> = hits.iter().map(|h| h.record.id.as_str()).collect();
    assert!(
        ids.contains(&"arch-002"),
        "archived record should appear with include_archived=true, got: {ids:?}"
    );
}

/// LRU eviction soft-archives the weakest record.
#[test]
fn eviction_lru_soft_archives_weakest() {
    let weak = make_record("weak", Some(120.0), 0);
    let strong = make_record("strong", Some(0.5), 20);
    let policy = EvictionPolicy {
        lru: true,
        ..EvictionPolicy::default()
    };
    let report = evict(&[weak, strong], &policy);
    assert!(
        report.archived >= 1,
        "LRU should archive at least one record"
    );
    assert!(
        report.log_entries.iter().any(|e| e.record_id == "weak"),
        "weakest record should be archived"
    );
}

/// Hard eviction permanently deletes already-archived records.
#[test]
fn eviction_hard_deletes_previously_archived() {
    let mut archived = make_record("archived-old", Some(200.0), 0);
    archived.state = MemoryState::Archived;
    let active = make_record("active-now", Some(1.0), 5);

    let policy = EvictionPolicy {
        hard: true,
        ..EvictionPolicy::default()
    };
    let report = evict(&[archived, active], &policy);
    assert_eq!(report.deleted, 1, "one hard delete expected");
    assert_eq!(
        report.archived, 0,
        "hard does not archive additional records"
    );
    assert!(report
        .log_entries
        .iter()
        .any(|e| e.record_id == "archived-old" && e.action == EvictionAction::Delete));
}

/// Eviction with max_keep=1 archives all but the strongest record.
#[test]
fn eviction_max_keep_archives_excess() {
    let records: Vec<MemoryRecord> = vec![
        make_record("r1", Some(100.0), 0), // weakest
        make_record("r2", Some(50.0), 2),  // medium
        make_record("r3", Some(0.5), 15),  // strongest
    ];
    let policy = EvictionPolicy {
        max_keep: Some(1),
        ..EvictionPolicy::default()
    };
    let report = evict(&records, &policy);
    assert_eq!(report.archived, 2, "should archive 2 to keep only 1");
    assert!(
        report
            .log_entries
            .iter()
            .any(|e| e.record_id == "r3" && e.action == EvictionAction::Archive)
            .not(),
        "strongest record (r3) should NOT be archived"
    );
}

/// Backward-compat: a record without state/last_access fields loads as Active / None.
#[test]
fn backward_compat_record_loads_as_active() {
    let legacy_yaml = "---\ntype: memory\nid: legacy-bc-1\nnode_id: node:legacy-bc-1\n\
                       tier: l1-atom\ntimestamp: \"2025-01-01T00:00:00Z\"\ntags: []\n---\n\n\
                       Legacy content without state or access fields.\n";
    let record = files_parse_record(legacy_yaml).expect("should parse legacy record");
    assert_eq!(
        record.state,
        MemoryState::Active,
        "legacy record should default to Active state"
    );
    assert_eq!(record.last_access, None);
    assert_eq!(record.access_count, 0);
    assert_eq!(record.author_id, None);
    assert_eq!(record.useful_count, 0);
}

/// Round-trip: render an Archived record to OKF and parse it back; state is preserved.
#[test]
fn archived_state_survives_render_parse_roundtrip() {
    let mut record = MemoryRecord::new(
        MemoryId::new("rt-001"),
        "node:rt-001",
        "round-trip content",
        Vec::new(),
        BTreeMap::new(),
        MemoryTier::L1Atom,
    );
    record.state = MemoryState::Archived;

    let rendered = files_render_record(&record).expect("render");
    let parsed = files_parse_record(&rendered).expect("parse");
    assert_eq!(
        parsed.state,
        MemoryState::Archived,
        "state should round-trip"
    );
}

// Bring in `.not()` for assertions (no external dep)
trait BoolExt {
    fn not(self) -> bool;
}
impl BoolExt for bool {
    fn not(self) -> bool {
        !self
    }
}
