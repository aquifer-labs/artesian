// SPDX-License-Identifier: Apache-2.0

use futures_util::{FutureExt, future::BoxFuture};

use aquifer::{MemoryBackend, MemoryQuery};

use crate::HeadgateResult;

/// A retrieval candidate surfaced by the data plane, normalized for the control plane.
///
/// `score` is **backend-relative**: a cosine similarity in `[0, 1]` for vector stores, a raw
/// match count for keyword stores. The qualify-gate's threshold is interpreted on the same
/// scale as the store that produced the item.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallItem {
    pub id: String,
    pub content: String,
    pub score: f32,
    pub source: String,
}

impl RecallItem {
    pub fn new(id: impl Into<String>, content: impl Into<String>, score: f32) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            score,
            source: "unknown".to_string(),
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

/// Data-plane seam: the control plane reads recall candidates from any retrieval store.
///
/// Aquifer's [`MemoryBackend`] is adapted via [`MemoryRecallStore`]; an external store
/// (mem0, Anthropic memory, a bespoke vector index) becomes a `RecallStore` with a thin
/// adapter, so the ACC control plane composes over whatever already holds the knowledge.
pub trait RecallStore: Send + Sync {
    fn recall(&self, query: &str, limit: usize) -> BoxFuture<'_, HeadgateResult<Vec<RecallItem>>>;
}

/// Adapter making any Aquifer [`MemoryBackend`] usable as a [`RecallStore`].
pub struct MemoryRecallStore<B> {
    backend: B,
    source: String,
}

impl<B> MemoryRecallStore<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            source: "aquifer".to_string(),
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

impl<B: MemoryBackend> RecallStore for MemoryRecallStore<B> {
    fn recall(&self, query: &str, limit: usize) -> BoxFuture<'_, HeadgateResult<Vec<RecallItem>>> {
        let memory_query = MemoryQuery::new(query).with_limit(limit);
        async move {
            let hits = self.backend.find(memory_query).await?;
            Ok(hits
                .into_iter()
                .map(|hit| RecallItem {
                    id: hit.record.node_id,
                    content: hit.record.content,
                    score: hit.score,
                    source: self.source.clone(),
                })
                .collect())
        }
        .boxed()
    }
}

/// A fixed list of candidates — useful for testing the control plane without a live backend.
pub struct StaticRecallStore {
    items: Vec<RecallItem>,
}

impl StaticRecallStore {
    pub fn new(items: Vec<RecallItem>) -> Self {
        Self { items }
    }
}

impl RecallStore for StaticRecallStore {
    fn recall(&self, _query: &str, limit: usize) -> BoxFuture<'_, HeadgateResult<Vec<RecallItem>>> {
        let items = self.items.iter().take(limit).cloned().collect();
        async move { Ok(items) }.boxed()
    }
}
