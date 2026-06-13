// SPDX-License-Identifier: Apache-2.0

//! Mímisbrunnr memory API and local backends.

mod backend;
mod files;
#[cfg(feature = "qdrant")]
mod qdrant;
mod rrf;
mod types;

pub use backend::MemoryBackend;
pub use files::FilesBackend;
#[cfg(feature = "qdrant")]
pub use qdrant::{QdrantBackend, QdrantBackendConfig};
pub use rrf::reciprocal_rank_fusion;
pub use types::{
    MemoryError, MemoryId, MemoryQuery, MemoryRecord, MemoryResult, MemoryTier, RrfOptions,
    SearchHit, SearchSource, StoreMemory,
};
