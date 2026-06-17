// SPDX-License-Identifier: Apache-2.0

//! Integration tests for D5: rechunk migration of oversized whole-file records.
//!
//! Scenario: records stored before chunking was introduced exist in the SQLite-vec
//! collection as single large payloads (content > ChunkConfig::max_chars, no
//! `parent_node` metadata). `rechunk_oversized_sqlite` identifies them, re-stores
//! via `MemoryBackend::store()` (which now splits into bounded chunks), and deletes
//! the original oversized record.

use std::sync::Arc;

use aquifer::{
    rechunk_oversized_sqlite, Distance, MemoryBackend, MemoryQuery, MemoryResult,
    SqliteVecVectorStore, SqliteVecVectorStoreConfig, StoreMemory, TextEmbedder, VectorCollection,
    VectorMemoryBackend, VectorMemoryConfig, VectorPoint, VectorStore,
};
use artesian_test_support::TempDir;

const TEST_DIMENSIONS: usize = 8;
const COLLECTION: &str = "rechunk-test";

struct TestEmbedder;

impl TextEmbedder for TestEmbedder {
    fn embed_query(&self, text: &str) -> MemoryResult<Vec<f32>> {
        Ok(test_embedding(text))
    }

    fn embed_passage(&self, text: &str) -> MemoryResult<Vec<f32>> {
        Ok(test_embedding(text))
    }
}

fn test_embedding(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0f32; TEST_DIMENSIONS];
    for token in text.split_whitespace() {
        let index = token
            .bytes()
            .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
            % TEST_DIMENSIONS;
        vector[index] += 1.0;
    }
    let mag = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if mag > 0.0 {
        for v in &mut vector {
            *v /= mag;
        }
    }
    vector
}

fn make_backend(store: SqliteVecVectorStore) -> VectorMemoryBackend<SqliteVecVectorStore> {
    VectorMemoryBackend::with_embedder(
        store,
        VectorMemoryConfig {
            collection: COLLECTION.to_string(),
            dimensions: TEST_DIMENSIONS,
            // These tests assert the raw chunk/storage layer; disable small-to-big
            // retrieval expansion so find() returns individual stored chunks.
            parent_context_auto: false,
            parent_context_chars: 0,
            ..VectorMemoryConfig::new(COLLECTION)
        },
        Arc::new(TestEmbedder),
    )
    .expect("backend should construct")
}

/// Generate a string that is guaranteed to exceed 1 600 characters (ChunkConfig::max_chars).
/// We use repetition so the content is deterministic and contains keyword "oversized-sentinel"
/// that we can search for later.
fn large_content(char_count: usize) -> String {
    let word = "oversized-sentinel-word ";
    word.repeat(char_count.div_ceil(word.len()))
        .chars()
        .take(char_count)
        .collect()
}

/// Insert a record directly into the vector store (simulating a pre-chunking write that
/// bypassed the chunking layer). This is the "before migration" state.
async fn insert_legacy_oversized(
    store: &SqliteVecVectorStore,
    backend: &VectorMemoryBackend<SqliteVecVectorStore>,
    content: &str,
) -> String {
    let id = format!("legacy-{}", content.len());
    let payload_json = serde_json::json!({
        "id": id,
        "node_id": format!("node:{id}"),
        "content": content,
        "tags": [],
        "metadata": {},
        "tier": "l1-atom",
        "created_at": "2024-01-01T00:00:00Z",
    });

    let vector = test_embedding(content);
    let point = VectorPoint {
        id: id.clone(),
        payload: payload_json
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        vector,
    };

    // Ensure the collection table exists before upserting.
    let _ = backend;
    store
        .ensure_collection(VectorCollection {
            name: COLLECTION.to_string(),
            dimensions: TEST_DIMENSIONS,
            distance: Distance::Cosine,
        })
        .await
        .expect("ensure_collection");
    store
        .upsert(COLLECTION, vec![point])
        .await
        .expect("direct upsert of legacy record");
    id
}

#[tokio::test]
async fn store_large_content_produces_bounded_chunks_with_parent_linkage() {
    let tempdir = TempDir::new("rechunk-store");
    let store =
        SqliteVecVectorStore::open(SqliteVecVectorStoreConfig::new(tempdir.join("db.sqlite")))
            .expect("store opens");
    let backend = make_backend(store);

    let content = large_content(4_000);

    let record = backend
        .store(StoreMemory::atom(&content))
        .await
        .expect("store should succeed");

    // The returned record is the first chunk — it must have a parent_node.
    assert!(
        record.metadata.contains_key("parent_node"),
        "first chunk should carry parent_node; got metadata: {:?}",
        record.metadata
    );

    // All chunks should be findable via search.
    let hits = backend
        .find(MemoryQuery::new("oversized-sentinel-word").with_limit(20))
        .await
        .expect("find should succeed");

    assert!(
        hits.len() >= 2,
        "expected at least 2 chunks for a 4 000-char doc; got {}",
        hits.len()
    );

    for hit in &hits {
        assert!(
            hit.record.metadata.contains_key("parent_node"),
            "every chunk should carry parent_node; got {:?}",
            hit.record.metadata
        );
        assert!(
            hit.record.content.len() <= 1_800,
            "chunk content should be bounded; got {} chars",
            hit.record.content.len()
        );
    }

    // Reconstruct the full content from chunks and verify coverage.
    let reconstructed: String = hits.iter().map(|h| h.record.content.as_str()).collect();
    let original_words: Vec<&str> = content.split_whitespace().collect();
    let covered_words: usize = original_words
        .iter()
        .filter(|w| reconstructed.contains(**w))
        .count();
    assert!(
        covered_words >= original_words.len() * 95 / 100,
        "at least 95% of original words should be reachable via chunks; coverage={}/{}",
        covered_words,
        original_words.len()
    );
}

#[tokio::test]
async fn rechunk_oversized_sqlite_migrates_legacy_whole_file_records() {
    let tempdir = TempDir::new("rechunk-migrate");
    let store =
        SqliteVecVectorStore::open(SqliteVecVectorStoreConfig::new(tempdir.join("db.sqlite")))
            .expect("store opens");
    let backend = make_backend(store.clone());

    // Simulate pre-chunking state: insert a 50 000-char record directly.
    let large = large_content(50_000);
    insert_legacy_oversized(&store, &backend, &large).await;

    // Verify pre-migration: exactly one record, no parent_node.
    let pre = store
        .scan_all_records(COLLECTION)
        .expect("scan should succeed");
    assert_eq!(
        pre.len(),
        1,
        "should have exactly 1 legacy record before migration"
    );
    assert!(
        pre[0]
            .get("metadata")
            .and_then(|m| m.get("parent_node"))
            .is_none(),
        "legacy record should not have parent_node before migration"
    );

    // Run migration.
    let report = rechunk_oversized_sqlite(&store, &backend, COLLECTION)
        .await
        .expect("rechunk should succeed");

    assert_eq!(report.scanned, 1);
    assert_eq!(report.oversized, 1);
    assert_eq!(report.rechunked, 1);

    // Post-migration: multiple chunk records, each bounded, each with parent_node.
    // Exclude the internal compat metadata record (kind = "artesian.compat").
    let post = store
        .scan_all_records(COLLECTION)
        .expect("scan after rechunk");

    let chunk_records: Vec<_> = post
        .iter()
        .filter(|p| {
            p.get("kind")
                .and_then(|v| v.as_str())
                .is_none_or(|k| k != "artesian.compat")
        })
        .collect();

    assert!(
        chunk_records.len() >= 10,
        "50 KB doc should produce at least 10 chunks; got {}",
        chunk_records.len()
    );

    for payload in &chunk_records {
        let has_parent = payload
            .get("metadata")
            .and_then(|m| m.get("parent_node"))
            .is_some();
        let content_len = payload
            .get("content")
            .and_then(|v| v.as_str())
            .map_or(0, str::len);

        assert!(
            has_parent,
            "every chunk must carry parent_node; got: {payload:?}"
        );
        assert!(
            content_len <= 1_800,
            "each chunk must be bounded (≤ 1 800 chars); got {content_len}"
        );
    }

    // Full content is reachable: search for the sentinel keyword.
    let hits = backend
        .find(MemoryQuery::new("oversized-sentinel-word").with_limit(50))
        .await
        .expect("find after rechunk");
    assert!(
        !hits.is_empty(),
        "chunks should be searchable after migration"
    );
}

/// ~1 200 characters of mostly-Cyrillic text: its UTF-8 byte length (> 1 600)
/// exceeds `max_chars`, while its character count (≤ 1 600) fits a single chunk.
/// This is exactly the record a byte-based oversized gate would wrongly
/// rechunk-and-delete on a second migration run.
fn multibyte_single_chunk() -> String {
    let word = "память-токен-страж ";
    word.repeat(1_200_usize.div_ceil(word.chars().count()))
        .chars()
        .take(1_200)
        .collect()
}

#[tokio::test]
async fn rechunk_oversized_sqlite_is_idempotent_for_multibyte_single_chunk_records() {
    let tempdir = TempDir::new("rechunk-multibyte");
    let store =
        SqliteVecVectorStore::open(SqliteVecVectorStoreConfig::new(tempdir.join("db.sqlite")))
            .expect("store opens");
    let backend = make_backend(store.clone());

    let content = multibyte_single_chunk();
    // The dangerous shape: bytes over the limit, characters under it.
    assert!(
        content.len() > 1_600,
        "byte length must exceed max_chars to exercise the regression"
    );
    assert!(
        content.chars().count() <= 1_600,
        "character count must fit a single chunk"
    );

    let legacy_id = insert_legacy_oversized(&store, &backend, &content).await;

    // Two consecutive migration runs must both be no-ops for this record and must
    // never delete it (the byte-vs-char data-loss regression).
    for run in 1..=2 {
        let report = rechunk_oversized_sqlite(&store, &backend, COLLECTION)
            .await
            .expect("rechunk should succeed");
        assert_eq!(
            report.oversized, 0,
            "run {run}: a single-chunk record must not be flagged oversized"
        );
        assert_eq!(
            report.rechunked, 0,
            "run {run}: nothing should be rechunked"
        );

        let surviving = store
            .scan_all_records(COLLECTION)
            .expect("scan after rechunk")
            .into_iter()
            .filter(|p| p.get("id").and_then(|v| v.as_str()) == Some(legacy_id.as_str()))
            .count();
        assert_eq!(
            surviving, 1,
            "run {run}: legacy record must survive migration, not be deleted"
        );
    }
}
