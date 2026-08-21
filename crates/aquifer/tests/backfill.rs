// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use aquifer::{
    FilesBackend, MemoryBackend, MemoryQuery, MemoryResult, SqliteVecVectorStore, TextEmbedder,
    VectorMemoryBackend, VectorMemoryConfig, backfill_directory, backfill_directory_with_project,
};
use artesian_test_support::TempDir;

#[tokio::test]
async fn backfill_is_idempotent_for_files_backend() {
    let tempdir = TempDir::new("backfill-files");
    let source = tempdir.join("source");
    std::fs::create_dir_all(&source).expect("source dir should be created");
    std::fs::write(
        source.join("memory.md"),
        "[2026-01-02] Durable imported memory",
    )
    .expect("source memory should be written");

    let backend = FilesBackend::new(tempdir.join("files"));
    assert_backfill_idempotency(&backend, &source).await;
}

#[tokio::test]
async fn backfill_is_idempotent_for_sqlite_vec_backend() {
    let tempdir = TempDir::new("backfill-sqlite");
    let source = tempdir.join("source");
    std::fs::create_dir_all(&source).expect("source dir should be created");
    std::fs::write(
        source.join("memory.json"),
        r#"{"content":"Durable imported memory","tier":"l1-atom","tags":["import"]}"#,
    )
    .expect("source memory should be written");

    let store = SqliteVecVectorStore::in_memory().expect("sqlite-vec should open");
    let backend = VectorMemoryBackend::with_embedder(
        store,
        VectorMemoryConfig {
            collection: "backfill".to_string(),
            dimensions: TEST_DIMENSIONS,
            ..VectorMemoryConfig::new("backfill")
        },
        Arc::new(TestEmbedder),
    )
    .expect("backend should construct");
    assert_backfill_idempotency(&backend, &source).await;
}

#[tokio::test]
async fn backfill_with_project_stamps_raw_and_ocf_records_for_files_backend() {
    let tempdir = TempDir::new("backfill-project-files");
    let source = tempdir.join("source");
    write_mixed_project_source(&source);

    let backend = FilesBackend::new(tempdir.join("files"));
    assert_project_stamped_backfill(&backend, &source).await;
}

#[tokio::test]
async fn backfill_with_project_stamps_raw_and_ocf_records_for_sqlite_vec_backend() {
    let tempdir = TempDir::new("backfill-project-sqlite");
    let source = tempdir.join("source");
    write_mixed_project_source(&source);

    let store = SqliteVecVectorStore::in_memory().expect("sqlite-vec should open");
    let backend = VectorMemoryBackend::with_embedder(
        store,
        VectorMemoryConfig {
            collection: "backfill-project".to_string(),
            dimensions: TEST_DIMENSIONS,
            ..VectorMemoryConfig::new("backfill-project")
        },
        Arc::new(TestEmbedder),
    )
    .expect("backend should construct");
    assert_project_stamped_backfill(&backend, &source).await;
}

async fn assert_backfill_idempotency(backend: &dyn MemoryBackend, source: &std::path::Path) {
    let first = backfill_directory(backend, source)
        .await
        .expect("first backfill should succeed");
    let second = backfill_directory(backend, source)
        .await
        .expect("second backfill should succeed");
    let hits = backend
        .find(MemoryQuery::new("imported").with_limit(10))
        .await
        .expect("find should succeed");

    assert_eq!(first.scanned, 1);
    assert_eq!(first.imported, 1);
    assert_eq!(first.skipped_duplicates, 0);
    assert!(first.failed.is_empty());
    assert_eq!(second.scanned, 1);
    assert_eq!(second.imported, 0);
    assert_eq!(second.skipped_duplicates, 1);
    assert!(second.failed.is_empty());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.content, "Durable imported memory");
}

async fn assert_project_stamped_backfill(backend: &dyn MemoryBackend, source: &std::path::Path) {
    let stats = backfill_directory_with_project(backend, source, "foo")
        .await
        .expect("backfill should succeed");
    let hits = backend
        .find(
            MemoryQuery::new("project stamp sentinel")
                .with_project("foo")
                .with_limit(10),
        )
        .await
        .expect("find should succeed");

    assert_eq!(stats.scanned, 3);
    assert_eq!(stats.imported, 3);
    assert!(stats.failed.is_empty());
    assert_eq!(hits.len(), 3, "{hits:#?}");
    assert!(
        hits.iter()
            .all(|hit| hit.record.project.as_deref() == Some("foo")),
        "every imported record should be stamped with foo: {hits:#?}"
    );
}

fn write_mixed_project_source(source: &std::path::Path) {
    std::fs::create_dir_all(source).expect("source dir should be created");
    std::fs::write(
        source.join("raw.md"),
        "# Raw\n\nraw project stamp sentinel from markdown",
    )
    .expect("raw memory should be written");
    std::fs::write(
        source.join("structured.md"),
        r#"---
type: memory
timestamp: "2026-01-03T00:00:00Z"
node_id: node:structured-project-stamp
tier: l2-scenario
tags:
  - homelab
  - imported
  - memory
---

structured project stamp sentinel from an OCF record
"#,
    )
    .expect("structured memory should be written");
    std::fs::write(
        source.join("structured-stale.md"),
        r#"---
type: memory
timestamp: "2026-01-04T00:00:00Z"
node_id: node:structured-stale-project-stamp
tier: l2-scenario
tags:
  - homelab
  - imported
  - memory
project: stale-project
---

structured project stamp sentinel from a stale-project OCF record
"#,
    )
    .expect("stale-project structured memory should be written");
}

#[tokio::test]
async fn backfill_reports_bad_file_without_aborting_import() {
    let tempdir = TempDir::new("backfill-bad-file");
    let source = tempdir.join("source");
    std::fs::create_dir_all(&source).expect("source dir should be created");
    std::fs::write(source.join("good.md"), "[2026-01-02] Good memory")
        .expect("good memory should be written");
    std::fs::write(source.join("bad.json"), "{not-json").expect("bad memory should be written");

    let backend = FilesBackend::new(tempdir.join("files"));
    let stats = backfill_directory(&backend, &source)
        .await
        .expect("backfill should not abort");
    let hits = backend
        .find(MemoryQuery::new("Good").with_limit(10))
        .await
        .expect("find should succeed");

    assert_eq!(stats.scanned, 2);
    assert_eq!(stats.imported, 1);
    assert_eq!(stats.failed.len(), 1);
    assert!(stats.failed[0].file.ends_with("bad.json"));
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn backfill_chunks_markdown_by_heading() {
    let tempdir = TempDir::new("backfill-chunks");
    let source = tempdir.join("source");
    std::fs::create_dir_all(&source).expect("source dir should be created");
    std::fs::write(
        source.join("sections.md"),
        "# Alpha\nfirst imported fact\n\n# Beta\nsecond imported fact",
    )
    .expect("section memory should be written");

    let backend = FilesBackend::new(tempdir.join("files"));
    let stats = backfill_directory(&backend, &source)
        .await
        .expect("backfill should succeed");
    let hits = backend
        .find(MemoryQuery::new("imported fact").with_limit(10))
        .await
        .expect("find should succeed");

    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.imported, 2);
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter()
            .any(|hit| hit.record.metadata.get("heading") == Some(&"Alpha".to_string()))
    );
    assert!(
        hits.iter()
            .any(|hit| hit.record.metadata.get("heading") == Some(&"Beta".to_string()))
    );
}

const TEST_DIMENSIONS: usize = 8;

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
    let mut vector = vec![0.0; TEST_DIMENSIONS];
    for token in text.split_whitespace() {
        let index = token.bytes().fold(0usize, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(byte as usize)
        }) % TEST_DIMENSIONS;
        vector[index] += 1.0;
    }
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        for value in &mut vector {
            *value /= magnitude;
        }
    }
    vector
}
