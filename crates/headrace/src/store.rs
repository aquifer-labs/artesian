// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use artesian_core::Role;

pub type TaskResult<T> = Result<T, TaskError>;

#[derive(Debug, Error)]
pub enum TaskError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to encode task: {0}")]
    Encode(#[from] serde_yaml::Error),
    #[error("failed to decode task payload: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task file is invalid: {0}")]
    InvalidFile(String),
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("task is blocked by unfinished tasks: {0:?}")]
    Blocked(Vec<String>),
    #[error("verifier failed: {0}")]
    Verifier(String),
    #[error("memory backend failed: {0}")]
    Memory(#[from] aquifer::MemoryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    Todo,
    Doing,
    Done,
    Blocked,
}

impl TaskStatus {
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Blocked)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    Compound,
    Primitive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub role: Role,
    pub status: TaskStatus,
    pub kind: TaskKind,
    pub blockers: Vec<String>,
    pub children: Vec<String>,
    pub claimed_by: Option<String>,
    pub verifier_names: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    pub fn is_dispatch_eligible(&self, tasks: &[Task]) -> bool {
        self.status == TaskStatus::Todo
            && self.blockers.iter().all(|blocker| {
                tasks
                    .iter()
                    .find(|candidate| candidate.id == *blocker)
                    .is_some_and(|candidate| candidate.status.is_terminal())
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTask {
    pub id: Option<String>,
    pub title: String,
    pub description: String,
    pub role: Role,
    pub kind: TaskKind,
    pub blockers: Vec<String>,
    pub children: Vec<String>,
    pub verifier_names: Vec<String>,
}

impl NewTask {
    pub fn primitive(title: impl Into<String>) -> Self {
        Self {
            id: None,
            title: title.into(),
            description: String::new(),
            role: Role::Worker,
            kind: TaskKind::Primitive,
            blockers: Vec::new(),
            children: Vec::new(),
            verifier_names: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub task_id: Option<String>,
    pub claimant: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionTask {
    pub id: String,
    pub status: TaskStatus,
}

pub trait TaskStore: Send + Sync {
    fn create(&self, task: NewTask) -> BoxFuture<'_, TaskResult<Task>>;

    fn claim(&self, request: ClaimRequest) -> BoxFuture<'_, TaskResult<Option<Task>>>;

    fn transition(&self, transition: TransitionTask) -> BoxFuture<'_, TaskResult<Task>>;

    fn get(&self, id: &str) -> BoxFuture<'_, TaskResult<Option<Task>>>;

    fn list(&self) -> BoxFuture<'_, TaskResult<Vec<Task>>>;

    fn find(&self, query: &str) -> BoxFuture<'_, TaskResult<Vec<Task>>>;
}
