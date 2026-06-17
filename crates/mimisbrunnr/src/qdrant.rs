// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::{collections::HashMap, time::Duration};

use futures_util::{future::BoxFuture, FutureExt};
use qdrant_client::{
    qdrant::{
        Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, FieldType, Filter,
        GetPointsBuilder, PointStruct, QueryPointsBuilder, RetrievedPoint, ScoredPoint,
        ScrollPointsBuilder, UpsertPointsBuilder, Value, VectorParamsBuilder,
    },
    Payload, Qdrant,
};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::{
    vector::payload_matches_filter, Distance, Filter as MemoryFilter, FilterCondition, FilterValue,
    MemoryError, MemoryResult, PayloadIndex, RangeFilter, SnapshotReport, VectorCollection,
    VectorCollectionAdmin, VectorMemoryBackend, VectorMemoryConfig, VectorPoint, VectorSearch,
    VectorSearchHit, VectorSearchSource, VectorStore, VectorStoreCapabilities,
};

pub type QdrantBackend = VectorMemoryBackend<QdrantVectorStore>;

#[derive(Debug, Clone)]
pub struct QdrantVectorStoreConfig {
    pub url: String,
    pub rest_url: Option<String>,
    pub api_key: Option<String>,
}

impl QdrantVectorStoreConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            rest_url: None,
            api_key: None,
        }
    }

    pub fn normalized(&self) -> MemoryResult<Self> {
        let endpoints = QdrantEndpoints::from_urls(&self.url, self.rest_url.as_deref())?;
        Ok(Self {
            url: endpoints.grpc_url,
            rest_url: Some(endpoints.rest_url),
            api_key: self.api_key.clone(),
        })
    }

    pub fn endpoints(&self) -> MemoryResult<QdrantEndpoints> {
        QdrantEndpoints::from_urls(&self.url, self.rest_url.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantEndpoints {
    pub grpc_url: String,
    pub rest_url: String,
}

impl QdrantEndpoints {
    pub fn from_urls(qdrant_url: &str, rest_url: Option<&str>) -> MemoryResult<Self> {
        let qdrant_url = normalize_url(qdrant_url)?;
        let rest_url = rest_url.map(normalize_url).transpose()?;
        let qdrant_port = qdrant_url.port();

        let (grpc_url, rest_url) = match (qdrant_port, rest_url) {
            (Some(6334), Some(rest_url)) => (qdrant_url, rest_url),
            (Some(6333), Some(rest_url)) => (derive_port(&qdrant_url, 6334)?, rest_url),
            (Some(6334), None) => (qdrant_url.clone(), derive_port(&qdrant_url, 6333)?),
            (Some(6333), None) => (derive_port(&qdrant_url, 6334)?, qdrant_url.clone()),
            (None, Some(rest_url)) => (derive_port(&qdrant_url, 6334)?, rest_url),
            (None, None) => (
                derive_port(&qdrant_url, 6334)?,
                derive_port(&qdrant_url, 6333)?,
            ),
            (Some(_), Some(rest_url)) => (qdrant_url, rest_url),
            (Some(port), None) => {
                return Err(MemoryError::InvalidFile(format!(
                    "cannot derive Qdrant REST endpoint from custom --qdrant-url port {port}; pass --qdrant-rest-url explicitly"
                )));
            }
        };

        Ok(Self {
            grpc_url: url_to_endpoint(&grpc_url),
            rest_url: url_to_endpoint(&rest_url),
        })
    }
}

pub struct QdrantVectorStore {
    config: QdrantVectorStoreConfig,
    client: Qdrant,
}

impl QdrantVectorStore {
    pub fn connect(config: QdrantVectorStoreConfig) -> MemoryResult<Self> {
        let config = config.normalized()?;
        let mut builder = Qdrant::from_url(&config.url);
        if let Some(api_key) = &config.api_key {
            builder = builder.api_key(api_key.clone());
        }
        let client = builder
            .build()
            .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
        Ok(Self { config, client })
    }

    pub fn config(&self) -> &QdrantVectorStoreConfig {
        &self.config
    }

    pub fn client(&self) -> &Qdrant {
        &self.client
    }

    pub fn memory_backend(
        self,
        collection: impl Into<String>,
    ) -> MemoryResult<VectorMemoryBackend<Self>> {
        VectorMemoryBackend::new(self, VectorMemoryConfig::new(collection))
    }

    pub async fn preflight(config: QdrantVectorStoreConfig) -> MemoryResult<QdrantPreflightReport> {
        preflight_qdrant(config).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantPreflightReport {
    pub grpc_url: String,
    pub rest_url: String,
    pub grpc_version: String,
    pub rest_status: u16,
}

pub async fn preflight_qdrant(
    config: QdrantVectorStoreConfig,
) -> MemoryResult<QdrantPreflightReport> {
    let config = config.normalized()?;
    let mut builder = Qdrant::from_url(&config.url)
        .timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(3));
    if let Some(api_key) = &config.api_key {
        builder = builder.api_key(api_key.clone());
    }
    let client = builder
        .build()
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
    let health = client.health_check().await.map_err(|error| {
        MemoryError::BackendUnavailable(format!(
            "Qdrant gRPC preflight failed for {}; expected the gRPC endpoint (default :6334). \
             Check that the gRPC port is exposed and that --qdrant-url is not pointing at an unrelated service. details: {error}",
            config.url
        ))
    })?;

    let rest_url = rest_url(&config);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
    let mut request = client.get(format!("{rest_url}/healthz"));
    if let Some(api_key) = &config.api_key {
        request = request.header("api-key", api_key);
    }
    let response = request.send().await.map_err(|error| {
        MemoryError::BackendUnavailable(format!(
            "Qdrant REST preflight failed for {rest_url}/healthz; expected the REST endpoint \
             (default :6333). Check the REST port or pass --qdrant-rest-url explicitly. details: {error}"
        ))
    })?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(MemoryError::BackendUnavailable(format!(
            "Qdrant REST preflight failed for {rest_url}/healthz with {status}; set the configured API key env var or remove Qdrant auth for local testing"
        )));
    }
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(MemoryError::BackendUnavailable(format!(
            "Qdrant REST preflight failed for {rest_url}/healthz with {status}: {text}"
        )));
    }

    Ok(QdrantPreflightReport {
        grpc_url: config.url,
        rest_url,
        grpc_version: health.version,
        rest_status: status.as_u16(),
    })
}

impl VectorStore for QdrantVectorStore {
    fn ensure_collection(&self, collection: VectorCollection) -> BoxFuture<'_, MemoryResult<()>> {
        async move {
            let exists = self
                .client
                .collection_exists(&collection.name)
                .await
                .map_err(qdrant_error)?;
            if exists {
                return Ok(());
            }

            self.client
                .create_collection(
                    CreateCollectionBuilder::new(&collection.name).vectors_config(
                        VectorParamsBuilder::new(
                            collection.dimensions as u64,
                            qdrant_distance(collection.distance),
                        ),
                    ),
                )
                .await
                .map_err(qdrant_error)?;
            Ok(())
        }
        .boxed()
    }

    fn ensure_payload_index(
        &self,
        collection: &str,
        index: PayloadIndex,
    ) -> BoxFuture<'_, MemoryResult<()>> {
        let collection = collection.to_string();
        async move {
            let field_type = if index.field == "content" {
                FieldType::Text
            } else {
                FieldType::Keyword
            };
            let result = self
                .client
                .create_field_index(
                    CreateFieldIndexCollectionBuilder::new(collection, index.field, field_type)
                        .wait(true),
                )
                .await;
            match result {
                Ok(_) => Ok(()),
                Err(error) if error.to_string().contains("already exists") => Ok(()),
                Err(error) => Err(qdrant_error(error)),
            }
        }
        .boxed()
    }

    fn upsert(
        &self,
        collection: &str,
        points: Vec<VectorPoint>,
    ) -> BoxFuture<'_, MemoryResult<()>> {
        let collection = collection.to_string();
        async move {
            let points = points
                .into_iter()
                .map(|point| {
                    let payload = Payload::try_from(point.payload)
                        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
                    Ok(PointStruct::new(
                        qdrant_point_id(&point.id),
                        point.vector,
                        payload,
                    ))
                })
                .collect::<MemoryResult<Vec<_>>>()?;

            self.client
                .upsert_points(UpsertPointsBuilder::new(collection, points).wait(true))
                .await
                .map_err(qdrant_error)?;
            Ok(())
        }
        .boxed()
    }

    fn search(
        &self,
        collection: &str,
        search: VectorSearch,
    ) -> BoxFuture<'_, MemoryResult<Vec<VectorSearchHit>>> {
        let collection = collection.to_string();
        async move {
            match search.source {
                VectorSearchSource::Vector | VectorSearchSource::Hybrid
                    if search.vector.is_some() =>
                {
                    let mut builder = QueryPointsBuilder::new(collection)
                        .query(search.vector.expect("vector checked above"))
                        .limit(search.limit as u64)
                        .with_payload(true);
                    if let Some(filter) = qdrant_filter(&search.filter) {
                        builder = builder.filter(filter);
                    }
                    let response = self.client.query(builder).await.map_err(qdrant_error)?;
                    response
                        .result
                        .into_iter()
                        .filter_map(|point| scored_point_to_hit(point, &search.filter).transpose())
                        .collect()
                }
                VectorSearchSource::Vector => Ok(Vec::new()),
                VectorSearchSource::Keyword | VectorSearchSource::Hybrid => {
                    let text = search.text.unwrap_or_default();
                    let response = self
                        .client
                        .scroll(
                            scroll_builder(&collection, &search.filter)
                                .limit((search.limit.max(1) * 10) as u32)
                                .with_payload(true),
                        )
                        .await
                        .map_err(qdrant_error)?;
                    let mut hits = response
                        .result
                        .into_iter()
                        .filter_map(|point| {
                            retrieved_point_to_hit(point, &search.filter, &text).transpose()
                        })
                        .collect::<MemoryResult<Vec<_>>>()?;
                    hits.sort_by(|left, right| {
                        right
                            .score
                            .partial_cmp(&left.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    hits.truncate(search.limit);
                    Ok(hits)
                }
            }
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
            let response = self
                .client
                .get_points(
                    GetPointsBuilder::new(collection, vec![qdrant_point_id(&point_id).into()])
                        .with_payload(true)
                        .with_vectors(true),
                )
                .await
                .map_err(qdrant_error)?;
            response
                .result
                .into_iter()
                .next()
                .map(retrieved_point_to_point)
                .transpose()
        }
        .boxed()
    }

    fn capabilities(&self) -> VectorStoreCapabilities {
        VectorStoreCapabilities {
            supports_server_side_hybrid: false,
            supports_sparse: false,
        }
    }
}

impl VectorCollectionAdmin for QdrantVectorStore {
    fn active_collection(&self, alias: &str) -> BoxFuture<'_, MemoryResult<Option<String>>> {
        let alias = alias.to_string();
        async move {
            let aliases = self.client.list_aliases().await.map_err(qdrant_error)?;
            Ok(aliases
                .aliases
                .into_iter()
                .find(|candidate| candidate.alias_name == alias)
                .map(|candidate| candidate.collection_name))
        }
        .boxed()
    }

    fn swap_alias(
        &self,
        alias: &str,
        old_collection: Option<&str>,
        new_collection: &str,
    ) -> BoxFuture<'_, MemoryResult<()>> {
        let alias = alias.to_string();
        let old_collection = old_collection.map(str::to_string);
        let new_collection = new_collection.to_string();
        async move {
            if old_collection.as_deref() == Some(new_collection.as_str()) {
                return Ok(());
            }
            let mut actions = Vec::new();
            if old_collection.is_some() {
                actions.push(serde_json::json!({
                    "delete_alias": {
                        "alias_name": alias
                    }
                }));
            }
            actions.push(serde_json::json!({
                "create_alias": {
                    "collection_name": new_collection,
                    "alias_name": alias
                }
            }));
            let client = reqwest::Client::new();
            let mut request = client
                .post(format!("{}/collections/aliases", rest_url(&self.config)))
                .json(&serde_json::json!({ "actions": actions }));
            if let Some(api_key) = &self.config.api_key {
                request = request.header("api-key", api_key);
            }
            let response = request
                .send()
                .await
                .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(MemoryError::BackendUnavailable(format!(
                    "Qdrant alias swap failed with {status}: {text}"
                )));
            }
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
        let target_dir = target_dir.to_path_buf();
        async move {
            std::fs::create_dir_all(&target_dir)?;
            let snapshot = self
                .client
                .create_snapshot(collection.clone())
                .await
                .map_err(qdrant_error)?
                .snapshot_description
                .ok_or_else(|| {
                    MemoryError::BackendUnavailable(
                        "Qdrant returned no snapshot description".to_string(),
                    )
                })?;
            let path = target_dir.join(&snapshot.name);
            download_snapshot_file(&self.config, &collection, &snapshot.name, &path).await?;
            Ok(SnapshotReport {
                collection,
                snapshot_name: snapshot.name,
                path,
                size_bytes: u64::try_from(snapshot.size).ok(),
                checksum: snapshot.checksum,
            })
        }
        .boxed()
    }
}

fn qdrant_distance(distance: Distance) -> qdrant_client::qdrant::Distance {
    match distance {
        Distance::Cosine => qdrant_client::qdrant::Distance::Cosine,
        Distance::Dot => qdrant_client::qdrant::Distance::Dot,
        Distance::Euclidean => qdrant_client::qdrant::Distance::Euclid,
    }
}

fn scroll_builder(collection: &str, filter: &MemoryFilter) -> ScrollPointsBuilder {
    let mut builder = ScrollPointsBuilder::new(collection);
    if let Some(filter) = qdrant_filter(filter) {
        builder = builder.filter(filter);
    }
    builder
}

fn qdrant_filter(filter: &MemoryFilter) -> Option<Filter> {
    if filter.is_empty() {
        return None;
    }

    Some(Filter {
        must: filter.must.iter().filter_map(qdrant_condition).collect(),
        should: filter.should.iter().filter_map(qdrant_condition).collect(),
        must_not: filter
            .must_not
            .iter()
            .filter_map(qdrant_condition)
            .collect(),
        min_should: None,
    })
}

fn qdrant_condition(condition: &FilterCondition) -> Option<Condition> {
    match condition {
        FilterCondition::Eq { field, value } => {
            value_to_match(value).map(|value| Condition::matches(field.to_string(), value))
        }
        FilterCondition::In { field, values } => {
            let values = values
                .iter()
                .filter_map(|value| match value {
                    FilterValue::String(value) => Some(value.clone()),
                    _ => None,
                })
                .collect::<Vec<String>>();
            if values.is_empty() {
                None
            } else {
                Some(Condition::matches(field.to_string(), values))
            }
        }
        FilterCondition::Range(range) => qdrant_range_condition(range),
        FilterCondition::Exists { field } => Some(Condition::is_empty(field.to_string())),
    }
}

fn value_to_match(value: &FilterValue) -> Option<qdrant_client::qdrant::r#match::MatchValue> {
    match value {
        FilterValue::String(value) => Some(qdrant_client::qdrant::r#match::MatchValue::Keyword(
            value.clone(),
        )),
        FilterValue::Bool(value) => {
            Some(qdrant_client::qdrant::r#match::MatchValue::Boolean(*value))
        }
        FilterValue::Number(_) => None,
    }
}

fn qdrant_range_condition(range: &RangeFilter) -> Option<Condition> {
    use qdrant_client::qdrant::{condition::ConditionOneOf, FieldCondition, Range};

    Some(Condition {
        condition_one_of: Some(ConditionOneOf::Field(FieldCondition {
            key: range.field.clone(),
            range: Some(Range {
                lt: range.lt,
                gt: range.gt,
                gte: range.gte,
                lte: range.lte,
            }),
            ..Default::default()
        })),
    })
}

fn scored_point_to_hit(
    point: ScoredPoint,
    filter: &MemoryFilter,
) -> MemoryResult<Option<VectorSearchHit>> {
    let vector_point = scored_point_to_point(point)?;
    if !payload_matches_filter(&vector_point.payload, filter) {
        return Ok(None);
    }
    let score = vector_point
        .payload
        .get("_score")
        .and_then(JsonValue::as_f64)
        .unwrap_or(0.0) as f32;
    Ok(Some(VectorSearchHit {
        point: strip_score(vector_point),
        score,
    }))
}

fn retrieved_point_to_hit(
    point: RetrievedPoint,
    filter: &MemoryFilter,
    text: &str,
) -> MemoryResult<Option<VectorSearchHit>> {
    let vector_point = retrieved_point_to_point(point)?;
    if !payload_matches_filter(&vector_point.payload, filter) {
        return Ok(None);
    }
    let content = vector_point
        .payload
        .get("content")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    // Filter-only retrieval (empty query text — e.g. small-to-big sibling lookups or
    // node_id drill-down) keeps every point the filter already selected; we only drop on
    // a genuine keyword miss. Without this, an empty query scored 0 and dropped all
    // filter-matched rows, silently breaking sibling/parent retrieval on Qdrant.
    let score = if text.trim().is_empty() {
        1.0
    } else {
        keyword_score(content, text)
    };
    if score == 0.0 {
        return Ok(None);
    }
    Ok(Some(VectorSearchHit {
        point: vector_point,
        score,
    }))
}

fn scored_point_to_point(point: ScoredPoint) -> MemoryResult<VectorPoint> {
    let mut payload = json_from_payload(point.payload)?;
    if let JsonValue::Object(map) = &mut payload {
        map.insert("_score".to_string(), JsonValue::from(point.score));
    }
    Ok(VectorPoint {
        id: payload_id(&payload),
        vector: Vec::new(),
        payload,
    })
}

fn retrieved_point_to_point(point: RetrievedPoint) -> MemoryResult<VectorPoint> {
    let payload = json_from_payload(point.payload)?;
    Ok(VectorPoint {
        id: payload_id(&payload),
        vector: Vec::new(),
        payload,
    })
}

fn json_from_payload(payload: HashMap<String, Value>) -> MemoryResult<JsonValue> {
    serde_json::to_value(Payload::from(payload))
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))
}

fn payload_id(payload: &JsonValue) -> String {
    payload
        .get("id")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string()
}

fn strip_score(mut point: VectorPoint) -> VectorPoint {
    if let JsonValue::Object(map) = &mut point.payload {
        map.remove("_score");
    }
    point
}

fn keyword_score(content: &str, query: &str) -> f32 {
    if query.trim().is_empty() {
        return 1.0;
    }
    let content = content.to_ascii_lowercase();
    query
        .split_whitespace()
        .filter(|term| content.contains(&term.to_ascii_lowercase()))
        .count() as f32
}

fn qdrant_point_id(memory_id: &str) -> String {
    let hex = if memory_id.len() >= 32
        && memory_id
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        memory_id.to_string()
    } else {
        format!("{:x}", Sha256::digest(memory_id.as_bytes()))
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

fn qdrant_error(error: qdrant_client::QdrantError) -> MemoryError {
    MemoryError::BackendUnavailable(error.to_string())
}

async fn download_snapshot_file(
    config: &QdrantVectorStoreConfig,
    collection: &str,
    snapshot_name: &str,
    path: &Path,
) -> MemoryResult<()> {
    let url = format!(
        "{}/collections/{}/snapshots/{}",
        rest_url(config),
        collection,
        snapshot_name
    );
    let client = reqwest::Client::new();
    let mut request = client.get(url);
    if let Some(api_key) = &config.api_key {
        request = request.header("api-key", api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(MemoryError::BackendUnavailable(format!(
            "Qdrant snapshot download failed with {status}: {text}"
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
    std::fs::write(path, bytes)?;
    Ok(())
}

fn rest_url(config: &QdrantVectorStoreConfig) -> String {
    if let Some(rest_url) = &config.rest_url {
        return rest_url.trim_end_matches('/').to_string();
    }
    if let Some(prefix) = config.url.strip_suffix(":6334") {
        return format!("{prefix}:6333");
    }
    config.url.trim_end_matches('/').to_string()
}

fn normalize_url(input: &str) -> MemoryResult<reqwest::Url> {
    let input = input.trim().trim_end_matches('/');
    reqwest::Url::parse(input).map_err(|error| {
        MemoryError::InvalidFile(format!(
            "invalid Qdrant URL `{input}`: {error}; expected http://HOST:6333 for REST or http://HOST:6334 for gRPC"
        ))
    })
}

fn derive_port(url: &reqwest::Url, port: u16) -> MemoryResult<reqwest::Url> {
    let mut derived = url.clone();
    derived.set_port(Some(port)).map_err(|()| {
        MemoryError::InvalidFile(format!(
            "cannot derive Qdrant endpoint from {}; pass both --qdrant-url and --qdrant-rest-url explicitly",
            url_to_endpoint(url)
        ))
    })?;
    Ok(derived)
}

fn url_to_endpoint(url: &reqwest::Url) -> String {
    url.to_string().trim_end_matches('/').to_string()
}

impl std::fmt::Display for QdrantEndpoints {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "grpc={} rest={}", self.grpc_url, self.rest_url)
    }
}

#[cfg(test)]
mod tests {
    use super::{QdrantEndpoints, QdrantVectorStoreConfig};

    #[test]
    fn derives_grpc_from_rest_qdrant_url() {
        let endpoints = QdrantVectorStoreConfig::new("http://qdrant.local:6333")
            .endpoints()
            .expect("endpoints should derive");
        assert_eq!(
            endpoints,
            QdrantEndpoints {
                grpc_url: "http://qdrant.local:6334".to_string(),
                rest_url: "http://qdrant.local:6333".to_string(),
            }
        );
    }

    #[test]
    fn derives_rest_from_grpc_qdrant_url() {
        let endpoints = QdrantVectorStoreConfig::new("http://qdrant.local:6334")
            .endpoints()
            .expect("endpoints should derive");
        assert_eq!(endpoints.grpc_url, "http://qdrant.local:6334");
        assert_eq!(endpoints.rest_url, "http://qdrant.local:6333");
    }

    #[test]
    fn custom_qdrant_port_requires_explicit_rest_url() {
        let error = QdrantVectorStoreConfig::new("http://qdrant.local:7000")
            .endpoints()
            .expect_err("custom port without REST URL should fail");
        assert!(error
            .to_string()
            .contains("pass --qdrant-rest-url explicitly"));
    }
}
