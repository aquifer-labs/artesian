// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use mimisbrunnr::{FilesBackend, MemoryBackend, MemoryQuery, MemoryTier, StoreMemory};
use rmcp::{
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct MemoryServer {
    backend: FilesBackend,
    tool_router: ToolRouter<Self>,
}

impl MemoryServer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            backend: FilesBackend::new(root),
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindRequest {
    pub query: String,
    pub limit: Option<usize>,
    pub node_id: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FindResponse {
    pub hits: Vec<FindHit>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FindHit {
    pub id: String,
    pub node_id: String,
    pub content: String,
    pub score: f32,
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StoreRequest {
    pub content: String,
    pub tags: Option<Vec<String>>,
    pub node_id: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StoreResponse {
    pub id: String,
    pub node_id: String,
}

#[tool_router]
impl MemoryServer {
    #[tool(
        name = "memory.find",
        description = "Find durable memories by keyword query."
    )]
    pub async fn memory_find(
        &self,
        Parameters(request): Parameters<FindRequest>,
    ) -> Result<Json<FindResponse>, ErrorData> {
        let mut query = MemoryQuery::new(request.query);
        query.limit = request.limit.unwrap_or(10);
        query.node_id = request.node_id;
        let hits = self
            .backend
            .find(query)
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?
            .into_iter()
            .map(|hit| FindHit {
                id: hit.record.id.to_string(),
                node_id: hit.record.node_id,
                content: hit.record.content,
                score: hit.score,
                tags: hit.record.tags,
            })
            .collect();
        Ok(Json(FindResponse { hits }))
    }

    #[tool(
        name = "memory.store",
        description = "Store a durable memory in the files backend."
    )]
    pub async fn memory_store(
        &self,
        Parameters(request): Parameters<StoreRequest>,
    ) -> Result<Json<StoreResponse>, ErrorData> {
        let record = self
            .backend
            .store(StoreMemory {
                content: request.content,
                tags: request.tags.unwrap_or_default(),
                metadata: Default::default(),
                tier: MemoryTier::L1Atom,
                node_id: request.node_id,
            })
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        Ok(Json(StoreResponse {
            id: record.id.to_string(),
            node_id: record.node_id,
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Brunnr memory server exposing memory.find and memory.store.")
    }
}

pub async fn run_stdio(root: impl Into<PathBuf>) -> anyhow::Result<()> {
    let server = MemoryServer::new(root);
    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}
