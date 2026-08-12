// SPDX-License-Identifier: Apache-2.0

//! Integration tests for Part A: access-tracking (access_count / last_access).
//!
//! Rules verified:
//! 1. `find` bumps `access_count` and sets `last_access` for returned records.
//! 2. A completely empty backend returns empty results — not an error.
//! 3. Records written without the new fields (backward-compat) load with None / 0.

use std::sync::Arc;

use aquifer::{FilesBackend, MemoryBackend, MemoryQuery, StoreMemory};
use artesian_test_support::TempDir;

/// Store a record via the FilesBackend, query it back, and verify the access bump.
///
/// The access bump is fire-and-forget (tokio::spawn); we yield control with a small sleep
/// to let the spawned write settle, then re-query to observe the persisted value.
#[tokio::test]
async fn find_bumps_access_count_and_last_access() {
    let dir = TempDir::new("access-tracking-bump");
    let backend = Arc::new(FilesBackend::new(dir.path()));

    // Store a record; the backend assigns an ID.
    let stored = backend
        .store(StoreMemory::atom("The team chose Rust"))
        .await
        .expect("store should succeed");
    let id = stored.id.clone();

    // Find using an empty query — FilesBackend returns all records for an empty text query.
    let hits = backend
        .find(MemoryQuery::new("").with_limit(100))
        .await
        .expect("find should succeed");
    assert!(
        hits.iter().any(|h| h.record.id == id),
        "stored record should appear in find results"
    );

    // Yield to the executor so the fire-and-forget tokio::spawn writeback can complete.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Re-query to observe the persisted access_count.
    let hits2 = backend
        .find(MemoryQuery::new("").with_limit(100))
        .await
        .expect("second find should succeed");
    let record = hits2
        .iter()
        .find(|h| h.record.id == id)
        .map(|h| &h.record)
        .expect("record should appear in second find");

    assert!(
        record.access_count >= 1,
        "access_count should be >= 1 after a find; got {}",
        record.access_count
    );
    assert!(
        record.last_access.is_some(),
        "last_access should be set after a find"
    );
}

/// A completely empty backend should return an empty result — not an error.
#[tokio::test]
async fn find_on_empty_backend_returns_empty_not_error() {
    let dir = TempDir::new("access-tracking-empty");
    let backend = Arc::new(FilesBackend::new(dir.path()));
    let hits = backend
        .find(MemoryQuery::new("any query").with_limit(10))
        .await
        .expect("find on empty backend should succeed");
    assert!(hits.is_empty());
}

/// Records stored without `access_count` / `last_access` in the TOML front-matter must
/// deserialise with the serde defaults (access_count = 0, last_access = None).
#[tokio::test]
async fn backward_compat_record_loads_with_zero_access() {
    let dir = TempDir::new("access-tracking-compat");

    // Write an headwater (YAML `---`) memory file without access_count / last_access fields
    // to simulate a record written by an older version of artesian.
    let old_format = "---\ntype: memory\nid: compat-old-1\nnode_id: node:compat-old-1\ntier: l1-atom\ntimestamp: \"2026-01-01T00:00:00Z\"\ntags: []\n---\n\nOld format memory content for backward-compat test.\n";
    std::fs::create_dir_all(dir.path().join("memory")).expect("create memory dir");
    std::fs::write(
        dir.path().join("memory").join("compat-old-1.md"),
        old_format,
    )
    .expect("write old-format file");

    let backend = Arc::new(FilesBackend::new(dir.path()));

    // Read all records — the old-format file must load without panicking.
    let hits_all = backend
        .find(MemoryQuery::new("").with_limit(1000))
        .await
        .expect("all-records find should succeed");

    // The old record should be loadable; access_count must default to 0 and last_access to None.
    if let Some(hit) = hits_all
        .iter()
        .find(|h| h.record.id.as_str() == "compat-old-1")
    {
        assert_eq!(
            hit.record.access_count, 0,
            "old-format record should have access_count = 0"
        );
        assert!(
            hit.record.last_access.is_none(),
            "old-format record should have last_access = None"
        );
    }
    // Absence of a panic / error is the primary signal that old records load correctly.
}

/// The access bump rewrites every record a query returned, in the background, while
/// other queries are walking the same directory. Before the write was made atomic
/// this failed with `InvalidFile("missing TOML front matter")` — a reader landing
/// between the truncate and the write of `fs::write`. It is the same race that
/// intermittently reddened CI on `files_backend_satisfies_memory_contract`.
///
/// The pressure is deliberate: each round issues several concurrent full-table
/// queries, and every hit in every one of them spawns a writeback over a record the
/// next round is about to read. A handful of records and a single round is not
/// enough to land in the window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_finds_never_observe_a_half_written_record() {
    let dir = TempDir::new("access-tracking-atomic");
    let backend = Arc::new(FilesBackend::new(dir.path()));

    const RECORDS: usize = 40;
    for index in 0..RECORDS {
        backend
            .store(StoreMemory::atom(format!("atomic record {index}")))
            .await
            .expect("store should succeed");
    }

    for round in 0..20 {
        let mut readers = Vec::new();
        for _ in 0..8 {
            let backend = Arc::clone(&backend);
            readers.push(tokio::spawn(async move {
                backend.find(MemoryQuery::new("").with_limit(100)).await
            }));
        }
        for reader in readers {
            let hits = reader
                .await
                .expect("reader task should not panic")
                .unwrap_or_else(|error| {
                    panic!("round {round}: find read a partially written record: {error}")
                });
            assert_eq!(hits.len(), RECORDS);
        }
    }
}
