// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use futures_util::{future::BoxFuture, FutureExt};
use qdrant_client::{
    qdrant::{
        Condition, CreateCollectionBuilder, Distance, Filter, PointStruct, QueryPointsBuilder,
        RetrievedPoint, ScoredPoint, ScrollPointsBuilder, UpsertPointsBuilder, Value,
        VectorParamsBuilder,
    },
    Payload, Qdrant,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    reciprocal_rank_fusion, MemoryBackend, MemoryError, MemoryId, MemoryQuery, MemoryRecord,
    MemoryResult, RrfOptions, SearchHit, SearchSource, StoreMemory,
};

pub const PINNED_FASTEMBED_MODEL: &str = "intfloat/multilingual-e5-small";
pub const PINNED_FASTEMBED_DIMENSIONS: usize = 384;

#[derive(Debug, Clone)]
pub struct QdrantBackendConfig {
    pub url: String,
    pub api_key: Option<String>,
    pub collection: String,
    pub embedding_model: String,
    pub dimensions: usize,
}

impl QdrantBackendConfig {
    pub fn new(url: impl Into<String>, collection: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            api_key: None,
            collection: collection.into(),
            embedding_model: PINNED_FASTEMBED_MODEL.to_string(),
            dimensions: PINNED_FASTEMBED_DIMENSIONS,
        }
    }
}

pub struct QdrantBackend {
    config: QdrantBackendConfig,
    client: Qdrant,
    embedder: Arc<Mutex<TextEmbedding>>,
}

impl QdrantBackend {
    pub fn connect(config: QdrantBackendConfig) -> MemoryResult<Self> {
        let mut builder = Qdrant::from_url(&config.url);
        if let Some(api_key) = &config.api_key {
            builder = builder.api_key(api_key.clone());
        }
        let client = builder
            .build()
            .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
        let embedder = TextEmbedding::try_new(
            TextInitOptions::new(EmbeddingModel::MultilingualE5Small)
                .with_show_download_progress(false),
        )
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;

        Ok(Self {
            config,
            client,
            embedder: Arc::new(Mutex::new(embedder)),
        })
    }

    pub fn config(&self) -> &QdrantBackendConfig {
        &self.config
    }

    pub fn client(&self) -> &Qdrant {
        &self.client
    }

    pub async fn ensure_collection(&self) -> MemoryResult<()> {
        let exists = self
            .client
            .collection_exists(&self.config.collection)
            .await
            .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
        if exists {
            return Ok(());
        }

        self.client
            .create_collection(
                CreateCollectionBuilder::new(&self.config.collection).vectors_config(
                    VectorParamsBuilder::new(self.config.dimensions as u64, Distance::Cosine),
                ),
            )
            .await
            .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
        Ok(())
    }

    fn embed(&self, prefix: &str, text: &str) -> MemoryResult<Vec<f32>> {
        let input = format!("{prefix}: {text}");
        let mut embedder = self
            .embedder
            .lock()
            .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
        let mut embeddings = embedder
            .embed([input], None)
            .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
        embeddings.pop().ok_or_else(|| {
            MemoryError::BackendUnavailable("fastembed returned no embeddings".to_string())
        })
    }
}

impl MemoryBackend for QdrantBackend {
    fn find(&self, query: MemoryQuery) -> BoxFuture<'_, MemoryResult<Vec<SearchHit>>> {
        async move {
            let vector = self.embed("query", &query.text)?;
            let response = self
                .client
                .query(
                    QueryPointsBuilder::new(&self.config.collection)
                        .query(vector)
                        .limit(query.limit as u64)
                        .with_payload(true),
                )
                .await
                .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;

            response
                .result
                .into_iter()
                .map(scored_point_to_hit)
                .collect()
        }
        .boxed()
    }

    fn store(&self, memory: StoreMemory) -> BoxFuture<'_, MemoryResult<MemoryRecord>> {
        async move {
            self.ensure_collection().await?;
            let id = stable_id(&memory);
            let node_id = memory.node_id.unwrap_or_else(|| format!("node:{id}"));
            let record = MemoryRecord {
                id,
                node_id,
                content: memory.content,
                tags: memory.tags,
                metadata: memory.metadata,
                tier: memory.tier,
                created_at: Utc::now(),
            };
            let vector = self.embed("passage", &record.content)?;
            let payload = Payload::try_from(serde_json::to_value(QdrantPayload::from(&record))?)
                .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
            let point = PointStruct::new(point_id(record.id.as_str()), vector, payload);

            self.client
                .upsert_points(
                    UpsertPointsBuilder::new(&self.config.collection, vec![point]).wait(true),
                )
                .await
                .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
            Ok(record)
        }
        .boxed()
    }

    fn hybrid_rrf(
        &self,
        keyword_query: MemoryQuery,
        vector_query: MemoryQuery,
        options: RrfOptions,
    ) -> BoxFuture<'_, MemoryResult<Vec<SearchHit>>> {
        async move {
            let keyword_hits = self.find(keyword_query).await.unwrap_or_default();
            let vector_hits = self.find(vector_query).await.unwrap_or_default();
            Ok(reciprocal_rank_fusion(
                &[keyword_hits, vector_hits],
                options,
            ))
        }
        .boxed()
    }

    fn get_node(&self, node_id: &str) -> BoxFuture<'_, MemoryResult<Option<MemoryRecord>>> {
        let node_id = node_id.to_string();
        async move {
            let response = self
                .client
                .scroll(
                    ScrollPointsBuilder::new(&self.config.collection)
                        .filter(Filter::must([Condition::matches("node_id", node_id)]))
                        .limit(1)
                        .with_payload(true),
                )
                .await
                .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
            response
                .result
                .into_iter()
                .next()
                .map(retrieved_point_to_record)
                .transpose()
        }
        .boxed()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QdrantPayload {
    id: MemoryId,
    node_id: String,
    content: String,
    tags: Vec<String>,
    metadata: BTreeMap<String, String>,
    tier: crate::MemoryTier,
    created_at: DateTime<Utc>,
}

impl From<&MemoryRecord> for QdrantPayload {
    fn from(record: &MemoryRecord) -> Self {
        Self {
            id: record.id.clone(),
            node_id: record.node_id.clone(),
            content: record.content.clone(),
            tags: record.tags.clone(),
            metadata: record.metadata.clone(),
            tier: record.tier,
            created_at: record.created_at,
        }
    }
}

impl From<QdrantPayload> for MemoryRecord {
    fn from(payload: QdrantPayload) -> Self {
        Self {
            id: payload.id,
            node_id: payload.node_id,
            content: payload.content,
            tags: payload.tags,
            metadata: payload.metadata,
            tier: payload.tier,
            created_at: payload.created_at,
        }
    }
}

fn scored_point_to_hit(point: ScoredPoint) -> MemoryResult<SearchHit> {
    let payload = payload_from_map(point.payload)?;
    Ok(SearchHit {
        record: MemoryRecord::from(payload),
        score: point.score,
        source: SearchSource::Vector,
    })
}

fn retrieved_point_to_record(point: RetrievedPoint) -> MemoryResult<MemoryRecord> {
    payload_from_map(point.payload).map(MemoryRecord::from)
}

fn payload_from_map(payload: HashMap<String, Value>) -> MemoryResult<QdrantPayload> {
    Payload::from(payload)
        .deserialize()
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))
}

fn stable_id(memory: &StoreMemory) -> MemoryId {
    let mut hasher = Sha256::new();
    hasher.update(memory.content.as_bytes());
    hasher.update(format!("{:?}", memory.tier).as_bytes());
    for tag in &memory.tags {
        hasher.update(tag.as_bytes());
    }
    for (key, value) in &memory.metadata {
        hasher.update(key.as_bytes());
        hasher.update(value.as_bytes());
    }
    if let Some(node_id) = &memory.node_id {
        hasher.update(node_id.as_bytes());
    }
    MemoryId::new(format!("{:x}", hasher.finalize()))
}

fn point_id(memory_id: &str) -> String {
    let hex = if memory_id.len() >= 32 {
        memory_id.to_string()
    } else {
        format!("{memory_id:0<32}")
    };
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
#[path = "qdrant_tests.rs"]
mod tests;
