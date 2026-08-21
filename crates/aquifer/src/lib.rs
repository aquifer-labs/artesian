// SPDX-License-Identifier: Apache-2.0

//! Aquifer memory API and local backends.

mod anchor;
mod backend;
mod backfill;
mod chunking;
mod compat;
pub mod decay;
pub mod entity;
pub mod episode;
pub mod event;
pub mod eviction;
mod files;
pub mod graph;
mod harness_import;
mod identity;
mod lane_lock;
mod mmr;
#[cfg(feature = "pgvector")]
mod pgvector;
#[cfg(feature = "qdrant")]
mod qdrant;
pub mod reconcile;
mod retrieval;
mod rrf;
mod semantic_cache;
mod session;
#[cfg(feature = "sqlite-vec")]
mod sqlite_vec;
pub mod temporal;
pub mod txn;
mod types;
mod upgrade;
#[cfg(feature = "vector")]
mod vector;
#[cfg(feature = "vector")]
mod vector_memory;
mod working;

pub use anchor::{AnchorAnchorStore, RecoveryContext, SessionAnchor, recover_after_compaction};
pub use backend::{BulkStoreReport, MemoryBackend};
pub use backfill::{
    BackfillFailure, BackfillOptions, BackfillStats, backfill_directory,
    backfill_directory_with_options, backfill_directory_with_project, collect_memory_paths,
    parse_memory_path,
};
pub use chunking::{Chunk, ChunkConfig, chunk_text};
#[allow(deprecated)]
pub use compat::OKF_VERSION;
pub use compat::{COMPAT_POINT_ID, CollectionCompat, HEADWATER_VERSION};
pub use decay::{DecayConfig, retrieval_strength};
pub use entity::{EntityIndex, extract_entities};
pub use episode::EpisodeIndex;
pub use event::{Event, assemble_events};
pub use eviction::{
    EvictionAction, EvictionLogEntry, EvictionPolicy, EvictionReport, append_eviction_log, evict,
};
pub use files::{
    FilesBackend, parse_record as files_parse_record, render_record as files_render_record,
};
pub use graph::{
    DEFAULT_GRAPH_HOPS, GRAPH_EXPANSION_LIMIT, GRAPH_SCAN_LIMIT, MAX_GRAPH_HOPS, Relation,
    expand_hits_with_neighbors,
};
pub use harness_import::{
    HarnessKind, HarnessMemoryCandidate, HarnessParseReport, parse_harness_candidates,
};
pub use identity::stable_memory_id;
pub use lane_lock::{SessionLaneGuard, SessionLaneLock};
pub use mmr::{MMR_DEFAULT_LAMBDA, MMR_MIN_CANDIDATES, mmr_diversify, mmr_diversify_when_large};
#[cfg(feature = "pgvector")]
pub use pgvector::{PgVectorBackend, PgVectorStore};
#[cfg(feature = "qdrant")]
pub use qdrant::{
    QdrantBackend, QdrantEndpoints, QdrantPreflightReport, QdrantVectorStore,
    QdrantVectorStoreConfig, ReplicateReport, preflight_qdrant, replicate_collection,
    replicate_collection_incremental,
};
pub use reconcile::{DEFAULT_RECONCILE_THRESHOLD, ReconcileConfig, ReconcileDecision, reconcile};
#[cfg(feature = "vector")]
pub use retrieval::FastembedReranker;
pub use retrieval::{LocalLexicalReranker, Reranker};
pub use rrf::reciprocal_rank_fusion;
#[cfg(feature = "vector")]
pub use semantic_cache::EmbedderVectorizer;
pub use semantic_cache::{CachingMemoryBackend, QueryVectorizer, SemanticCache, cosine_similarity};
pub use session::{
    DEFAULT_SESSION_COMPONENT, SESSION_RECORD_SOURCE, SESSION_RECORD_TAG, Session, SessionKey,
    SessionListFilter, SessionStore, SessionSummary,
};
#[cfg(feature = "sqlite-vec")]
pub use sqlite_vec::{SqliteVecBackend, SqliteVecVectorStore, SqliteVecVectorStoreConfig};
pub use temporal::{
    apply_knowledge_supersession, apply_recency_decay, entity_timeline, sort_hits_by_event_time,
};
#[allow(deprecated)]
pub use txn::sync_okf_directory;
pub use txn::{
    CommitLog, SyncReport, TransactionalMemory, TxnError, TxnSeq, sync_headwater_directory,
};
pub use types::{
    MemoryError, MemoryId, MemoryQuery, MemoryRecord, MemoryResult, MemoryScope, MemoryState,
    MemoryTier, ProcedureStep, RecallTelemetry, RetractReport, RrfOptions, SHARED_PROJECT,
    SKILL_PROCEDURE_METADATA_KEY, SearchHit, SearchSource, StoreMemory, UNTAGGED_PROJECT_LABEL,
    annotate_session_distances, insert_skill_procedure_metadata, normalize_project,
    skill_procedure_from_metadata,
};
pub use upgrade::{
    HeadwaterExportReport, HeadwaterVerifyReport, MigrationPlan, MigrationReport, RechunkReport,
    SnapshotReport, VectorCollectionAdmin, default_migration_collection, export_headwater_bundle,
    migrate_headwater_bundle, migration_manifest_path, rechunk_oversized_sqlite,
    verify_headwater_bundle,
};
#[allow(deprecated)]
pub use upgrade::{
    OkfExportReport, OkfVerifyReport, export_okf_bundle, migrate_okf_bundle, verify_okf_bundle,
};
#[cfg(feature = "vector")]
pub use vector::{
    Distance, Filter, FilterCondition, FilterValue, PayloadIndex, RangeFilter, VectorCollection,
    VectorPoint, VectorQuantization, VectorSearch, VectorSearchHit, VectorSearchSource,
    VectorStore, VectorStoreCapabilities,
};
#[cfg(feature = "vector")]
pub use vector_memory::{
    FastembedTextEmbedder, PINNED_FASTEMBED_DIMENSIONS, PINNED_FASTEMBED_MODEL, TextEmbedder,
    VectorMemoryBackend, VectorMemoryConfig,
};
pub use working::{
    InMemoryWorkingMemory, WorkingMemory, WorkingMemoryMode, WorkingMemoryView, WorkingTurn,
};

pub mod consolidation;
pub use consolidation::{
    ConsolidationClaim, ConsolidationOptions, ConsolidationReport, GovernanceFields,
    consolidation_pass,
};

pub mod dream;
pub use dream::{
    DreamDecision, DreamError, DreamOptions, DreamQualifyRecord, DreamResult, DreamSnapshotEntry,
    dream, render_diary, write_dream_bundle,
};
