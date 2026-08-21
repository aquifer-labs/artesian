// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use aquifer::{
    Distance, MemoryBackend, MemoryError, MemoryResult, MigrationPlan, SnapshotReport,
    SqliteVecVectorStore, TextEmbedder, VectorCollection, VectorCollectionAdmin,
    VectorMemoryBackend, VectorMemoryConfig, VectorPoint, VectorSearch, VectorSearchHit,
    VectorStore, VectorStoreCapabilities, migrate_headwater_bundle, verify_headwater_bundle,
};
use artesian_test_support::TempDir;
use futures_util::{FutureExt, future::BoxFuture};

#[tokio::test]
async fn old_schema_bundle_still_verifies() {
    let tempdir = TempDir::new("headwater-old-schema");
    let legacy = tempdir.join("legacy.md");
    std::fs::write(
        &legacy,
        r#"+++
id = "legacy-1"
node_id = "node:legacy"
tier = "l1-atom"
tags = ["legacy"]
metadata = {}
created_at = "2026-01-01T00:00:00Z"
+++

[2026-01-01] Legacy TOML memory survives upgrades.
"#,
    )
    .expect("legacy fixture should be written");
    std::fs::write(
        tempdir.join("headwater.md"),
        r#"---
type: decision
title: Unknown keys are tolerated
timestamp: 2026-01-02T00:00:00Z
node_id: node:headwater
tier: l1-atom
future_key: retained-as-metadata
---

headwater readers tolerate unknown scalar keys.
"#,
    )
    .expect("headwater fixture should be written");

    let report = verify_headwater_bundle(tempdir.path()).expect("bundle should verify");

    assert_eq!(report.files, 2);
    assert_eq!(report.records, 2);
}

#[tokio::test]
async fn embedding_model_mismatch_refuses_collection_access() {
    let store = SqliteVecVectorStore::in_memory().expect("sqlite-vec should open");
    let backend = VectorMemoryBackend::with_embedder(
        store.clone(),
        VectorMemoryConfig {
            embedding_model: "test-model-a".to_string(),
            dimensions: TEST_DIMENSIONS,
            distance: Distance::Cosine,
            ..VectorMemoryConfig::new("mismatch")
        },
        Arc::new(TestEmbedder),
    )
    .expect("backend should construct");
    backend
        .store(aquifer::StoreMemory::atom("first model memory"))
        .await
        .expect("initial store should write compat metadata");

    let incompatible = VectorMemoryBackend::with_embedder(
        store,
        VectorMemoryConfig {
            embedding_model: "test-model-b".to_string(),
            dimensions: TEST_DIMENSIONS,
            distance: Distance::Cosine,
            ..VectorMemoryConfig::new("mismatch")
        },
        Arc::new(TestEmbedder),
    )
    .expect("backend should construct");
    let error = incompatible
        .find(aquifer::MemoryQuery::new("first"))
        .await
        .expect_err("mismatched model should be rejected");

    assert!(matches!(error, MemoryError::CompatMismatch { .. }));
}

#[tokio::test]
async fn migration_rebuilds_new_collection_and_alias_swaps_idempotently() {
    let tempdir = TempDir::new("migration");
    std::fs::write(
        tempdir.join("memory.md"),
        r#"---
type: memory
timestamp: 2026-01-03T00:00:00Z
node_id: node:migrate
tier: l1-atom
---

Re-embedded memory is rebuilt from headwater.
"#,
    )
    .expect("migration fixture should be written");
    let store = MockAdminStore::default();
    store.insert_alias("memory", "memory_old");
    let plan = MigrationPlan {
        headwater_root: tempdir.path().to_path_buf(),
        alias: "memory".to_string(),
        new_collection: "memory_new".to_string(),
        retention_days: 30,
        config: VectorMemoryConfig {
            embedding_model: "test-model".to_string(),
            dimensions: TEST_DIMENSIONS,
            distance: Distance::Cosine,
            ..VectorMemoryConfig::new("memory")
        },
    };

    let first = migrate_headwater_bundle(&store, plan.clone(), Arc::new(TestEmbedder))
        .await
        .expect("migration should rebuild collection");
    let second = migrate_headwater_bundle(&store, plan, Arc::new(TestEmbedder))
        .await
        .expect("migration should be idempotent");

    assert_eq!(first.imported, 1);
    assert_eq!(first.old_collection.as_deref(), Some("memory_old"));
    assert!(first.retained_old_collection);
    assert_eq!(store.active_alias("memory"), Some("memory_new".to_string()));
    assert!(store.collection_exists("memory_old"));
    assert!(store.collection_exists("memory_new"));
    assert_eq!(second.imported, 0);
    assert_eq!(second.skipped_duplicates, 1);
}

const TEST_DIMENSIONS: usize = 3;

struct TestEmbedder;

impl TextEmbedder for TestEmbedder {
    fn embed_query(&self, text: &str) -> MemoryResult<Vec<f32>> {
        Ok(test_vector(text))
    }

    fn embed_passage(&self, text: &str) -> MemoryResult<Vec<f32>> {
        Ok(test_vector(text))
    }
}

fn test_vector(text: &str) -> Vec<f32> {
    let length = text.len() as f32;
    vec![length, length.rem_euclid(7.0), 1.0]
}

#[derive(Default)]
struct MockAdminStore {
    state: Arc<Mutex<MockState>>,
}

#[derive(Default)]
struct MockState {
    collections: BTreeMap<String, BTreeMap<String, VectorPoint>>,
    aliases: BTreeMap<String, String>,
}

impl MockAdminStore {
    fn insert_alias(&self, alias: &str, collection: &str) {
        let mut state = self.state.lock().expect("state lock should not poison");
        state.collections.entry(collection.to_string()).or_default();
        state
            .aliases
            .insert(alias.to_string(), collection.to_string());
    }

    fn active_alias(&self, alias: &str) -> Option<String> {
        self.state
            .lock()
            .expect("state lock should not poison")
            .aliases
            .get(alias)
            .cloned()
    }

    fn collection_exists(&self, collection: &str) -> bool {
        self.state
            .lock()
            .expect("state lock should not poison")
            .collections
            .contains_key(collection)
    }
}

impl VectorStore for MockAdminStore {
    fn ensure_collection(&self, collection: VectorCollection) -> BoxFuture<'_, MemoryResult<()>> {
        async move {
            self.state
                .lock()
                .expect("state lock should not poison")
                .collections
                .entry(collection.name)
                .or_default();
            Ok(())
        }
        .boxed()
    }

    fn ensure_payload_index(
        &self,
        _collection: &str,
        _index: aquifer::PayloadIndex,
    ) -> BoxFuture<'_, MemoryResult<()>> {
        async { Ok(()) }.boxed()
    }

    fn upsert(
        &self,
        collection: &str,
        points: Vec<VectorPoint>,
    ) -> BoxFuture<'_, MemoryResult<()>> {
        let collection = collection.to_string();
        async move {
            let mut state = self.state.lock().expect("state lock should not poison");
            let collection = state.collections.entry(collection).or_default();
            for point in points {
                collection.insert(point.id.clone(), point);
            }
            Ok(())
        }
        .boxed()
    }

    fn search(
        &self,
        collection: &str,
        _search: VectorSearch,
    ) -> BoxFuture<'_, MemoryResult<Vec<VectorSearchHit>>> {
        let collection = collection.to_string();
        async move {
            let state = self.state.lock().expect("state lock should not poison");
            Ok(state
                .collections
                .get(&collection)
                .into_iter()
                .flat_map(BTreeMap::values)
                .filter(|point| point.id != aquifer::COMPAT_POINT_ID)
                .cloned()
                .map(|point| VectorSearchHit { point, score: 1.0 })
                .collect())
        }
        .boxed()
    }

    fn get(
        &self,
        collection: &str,
        point_id: &str,
    ) -> BoxFuture<'_, MemoryResult<Option<VectorPoint>>> {
        let collection = collection.to_string();
        let point_id = point_id.to_string();
        async move {
            Ok(self
                .state
                .lock()
                .expect("state lock should not poison")
                .collections
                .get(&collection)
                .and_then(|points| points.get(&point_id))
                .cloned())
        }
        .boxed()
    }

    fn capabilities(&self) -> VectorStoreCapabilities {
        VectorStoreCapabilities::default()
    }
}

impl VectorCollectionAdmin for MockAdminStore {
    fn active_collection(&self, alias: &str) -> BoxFuture<'_, MemoryResult<Option<String>>> {
        let alias = alias.to_string();
        async move { Ok(self.active_alias(&alias)) }.boxed()
    }

    fn swap_alias(
        &self,
        alias: &str,
        _old_collection: Option<&str>,
        new_collection: &str,
    ) -> BoxFuture<'_, MemoryResult<()>> {
        let alias = alias.to_string();
        let new_collection = new_collection.to_string();
        async move {
            self.state
                .lock()
                .expect("state lock should not poison")
                .aliases
                .insert(alias, new_collection);
            Ok(())
        }
        .boxed()
    }

    fn snapshot_collection(
        &self,
        collection: &str,
        target_dir: &Path,
    ) -> BoxFuture<'_, MemoryResult<SnapshotReport>> {
        let collection = collection.to_string();
        let path = target_dir.join(format!("{collection}.snapshot"));
        async move {
            Ok(SnapshotReport {
                collection,
                snapshot_name: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("snapshot")
                    .to_string(),
                path,
                size_bytes: None,
                checksum: None,
            })
        }
        .boxed()
    }

    fn delete_collection(&self, _collection: &str) -> BoxFuture<'_, MemoryResult<()>> {
        async { Ok(()) }.boxed()
    }
}
