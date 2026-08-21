// SPDX-License-Identifier: Apache-2.0

//! Optional Sandbox sandbox runtime seam.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use futures_util::{FutureExt, future::BoxFuture};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxProfile {
    pub enabled: bool,
    pub image: Option<String>,
    pub allow_network: bool,
    pub mounted_paths: Vec<String>,
}

pub type WorkspaceResult<T> = Result<T, WorkspaceError>;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLease {
    pub worker_id: String,
    pub repo_root: PathBuf,
    pub path: PathBuf,
}

impl WorkspaceLease {
    pub fn cleanup(&self) -> WorkspaceResult<()> {
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path)?;
        }
        Ok(())
    }
}

pub trait WorkspaceProvider: Send + Sync {
    fn lease(
        &self,
        repo_root: &Path,
        worker_id: &str,
    ) -> BoxFuture<'_, WorkspaceResult<WorkspaceLease>>;
}

#[derive(Debug, Clone)]
pub struct ScratchWorkspaceProvider {
    root: PathBuf,
}

impl ScratchWorkspaceProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl WorkspaceProvider for ScratchWorkspaceProvider {
    fn lease(
        &self,
        repo_root: &Path,
        worker_id: &str,
    ) -> BoxFuture<'_, WorkspaceResult<WorkspaceLease>> {
        let repo_root = repo_root.to_path_buf();
        let worker_id = worker_id.to_string();
        async move {
            std::fs::create_dir_all(&self.root)?;
            let path = self.root.join(format!(
                "{}-{}",
                sanitize_worker_id(&worker_id),
                unique_suffix()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(WorkspaceLease {
                worker_id,
                repo_root,
                path,
            })
        }
        .boxed()
    }
}

fn sanitize_worker_id(worker_id: &str) -> String {
    let mut output = String::new();
    for character in worker_id.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else {
            output.push('-');
        }
    }
    output.trim_matches('-').to_string()
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}
