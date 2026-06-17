// SPDX-License-Identifier: Apache-2.0

//! Þingr task tracking primitives and file-backed task store.

mod files;
mod store;
mod vector;
mod verifier;

pub use files::{FilesTaskStore, TaskImportOutcome};
pub use store::{
    ClaimRequest, NewTask, Task, TaskError, TaskKind, TaskResult, TaskStatus, TaskStore,
    TransitionTask,
};
pub use vector::VectorTaskStore;
pub use verifier::{
    CommandVerifier, ExternalConnector, McpExternalConnector, Verifier, VerifierGate,
    VerifierOutcome,
};
