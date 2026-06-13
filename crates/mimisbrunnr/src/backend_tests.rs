// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex};

use futures_util::{future::BoxFuture, FutureExt};

use super::*;
use crate::{MemoryId, MemoryTier, SearchSource};

#[derive(Debug, Default)]
struct MockMemoryBackend {
    records: Arc<Mutex<Vec<MemoryRecord>>>,
}

impl MemoryBackend for MockMemoryBackend {
    fn find(&self, query: MemoryQuery) -> BoxFuture<'_, MemoryResult<Vec<SearchHit>>> {
        let records = Arc::clone(&self.records);
        async move {
            let needle = query.text.to_ascii_lowercase();
            let hits = records
                .lock()
                .expect("records lock should not be poisoned")
                .iter()
                .filter(|record| record.content.to_ascii_lowercase().contains(&needle))
                .cloned()
                .map(|record| SearchHit {
                    record,
                    score: 1.0,
                    source: SearchSource::Keyword,
                })
                .collect();
            Ok(hits)
        }
        .boxed()
    }

    fn store(&self, memory: StoreMemory) -> BoxFuture<'_, MemoryResult<MemoryRecord>> {
        let records = Arc::clone(&self.records);
        async move {
            let id = MemoryId::new(format!("memory-{}", records.lock().unwrap().len() + 1));
            let record = MemoryRecord::new(
                id,
                memory
                    .node_id
                    .unwrap_or_else(|| "node:contract".to_string()),
                memory.content,
                memory.tags,
                memory.metadata,
                memory.tier,
            );
            records.lock().unwrap().push(record.clone());
            Ok(record)
        }
        .boxed()
    }

    fn get_node(&self, node_id: &str) -> BoxFuture<'_, MemoryResult<Option<MemoryRecord>>> {
        let records = Arc::clone(&self.records);
        let node_id = node_id.to_string();
        async move {
            Ok(records
                .lock()
                .expect("records lock should not be poisoned")
                .iter()
                .find(|record| record.node_id == node_id)
                .cloned())
        }
        .boxed()
    }
}

#[tokio::test]
async fn memory_backend_contract_supports_store_find_and_node_drill_down() {
    let backend = MockMemoryBackend::default();
    let stored = backend
        .store(StoreMemory {
            content: "Brunnr stores durable context".to_string(),
            tags: vec!["contract".to_string()],
            metadata: Default::default(),
            tier: MemoryTier::L1Atom,
            node_id: Some("node:test".to_string()),
        })
        .await
        .expect("store should succeed");

    let found = backend
        .find(MemoryQuery::new("durable"))
        .await
        .expect("find should succeed");
    let drill_down = backend
        .get_node("node:test")
        .await
        .expect("get_node should succeed");

    assert_eq!(found, vec![SearchHit::keyword(stored.clone(), 1.0)]);
    assert_eq!(drill_down, Some(stored));
}

#[tokio::test]
async fn memory_backend_contract_supports_hybrid_rrf() {
    let backend = MockMemoryBackend::default();
    backend
        .store(StoreMemory {
            content: "hybrid retrieval".to_string(),
            tags: Vec::new(),
            metadata: Default::default(),
            tier: MemoryTier::L1Atom,
            node_id: Some("node:rrf".to_string()),
        })
        .await
        .expect("store should succeed");

    let hits = backend
        .hybrid_rrf(
            MemoryQuery::new("hybrid"),
            MemoryQuery::new("retrieval"),
            RrfOptions::default(),
        )
        .await
        .expect("hybrid search should succeed");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.node_id, "node:rrf");
}
