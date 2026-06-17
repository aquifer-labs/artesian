// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ClaimRequest, NewTask, Task, TaskError, TaskKind, TaskResult, TaskStatus, TaskStore,
    TransitionTask,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskImportOutcome {
    Imported(Task),
    SkippedDuplicate(Task),
}

impl TaskImportOutcome {
    pub fn task(&self) -> &Task {
        match self {
            Self::Imported(task) | Self::SkippedDuplicate(task) => task,
        }
    }

    pub const fn imported(&self) -> bool {
        matches!(self, Self::Imported(_))
    }
}

#[derive(Debug, Clone)]
pub struct FilesTaskStore {
    root: PathBuf,
    mutation_lock: Arc<Mutex<()>>,
}

impl FilesTaskStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mutation_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn tasks_dir(&self) -> PathBuf {
        self.root.join("tasks")
    }

    fn status_dir(&self, status: TaskStatus) -> PathBuf {
        self.tasks_dir().join(status.directory())
    }

    fn task_path(&self, status: TaskStatus, id: &str) -> PathBuf {
        self.status_dir(status).join(format!("{id}.md"))
    }

    fn ensure_dirs(&self) -> TaskResult<()> {
        for status in [
            TaskStatus::Todo,
            TaskStatus::Doing,
            TaskStatus::Done,
            TaskStatus::Blocked,
        ] {
            std::fs::create_dir_all(self.status_dir(status))?;
        }
        Ok(())
    }

    fn read_task_at(path: &Path) -> TaskResult<Task> {
        parse_task(&std::fs::read_to_string(path)?)
    }

    fn write_task_at(path: &Path, task: &Task) -> TaskResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, render_task(task)?)?;
        Ok(())
    }

    fn find_path(&self, id: &str) -> TaskResult<Option<(TaskStatus, PathBuf)>> {
        for status in [
            TaskStatus::Todo,
            TaskStatus::Doing,
            TaskStatus::Done,
            TaskStatus::Blocked,
        ] {
            let path = self.task_path(status, id);
            if path.exists() {
                return Ok(Some((status, path)));
            }
        }
        Ok(None)
    }

    pub fn is_task_like_path(path: &Path) -> bool {
        path.components().any(|component| {
            let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
            matches!(
                name.as_str(),
                "task" | "tasks" | "todo" | "doing" | "done" | "blocked" | "status" | "statuses"
            )
        }) || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name == "status.md" || name.ends_with(".task.md")
            })
    }

    pub fn parse_task_like_markdown(path: &Path, text: &str) -> TaskResult<Task> {
        if let Ok(task) = parse_task(text) {
            return Ok(task);
        }
        inferred_task_from_markdown(path, text)
    }

    pub async fn import_task(&self, task: Task) -> TaskResult<TaskImportOutcome> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|error| TaskError::InvalidFile(error.to_string()))?;
        self.ensure_dirs()?;
        if let Some((_, path)) = self.find_path(&task.id)? {
            return Self::read_task_at(&path).map(TaskImportOutcome::SkippedDuplicate);
        }
        let path = self.task_path(task.status, &task.id);
        Self::write_task_at(&path, &task)?;
        Ok(TaskImportOutcome::Imported(task))
    }
}

impl TaskStore for FilesTaskStore {
    fn create(&self, task: NewTask) -> BoxFuture<'_, TaskResult<Task>> {
        async move {
            let _guard = self
                .mutation_lock
                .lock()
                .map_err(|error| TaskError::InvalidFile(error.to_string()))?;
            self.ensure_dirs()?;
            let now = Utc::now();
            let task = Task {
                id: task
                    .id
                    .unwrap_or_else(|| generated_task_id(&task.title, now)),
                title: task.title,
                description: task.description,
                role: task.role,
                status: TaskStatus::Todo,
                kind: task.kind,
                blockers: task.blockers,
                children: task.children,
                claimed_by: None,
                verifier_names: task.verifier_names,
                created_at: now,
                updated_at: now,
            };
            let path = self.task_path(TaskStatus::Todo, &task.id);
            if path.exists() {
                return Err(TaskError::InvalidFile(format!(
                    "task already exists: {}",
                    task.id
                )));
            }
            Self::write_task_at(&path, &task)?;
            Ok(task)
        }
        .boxed()
    }

    fn claim(&self, request: ClaimRequest) -> BoxFuture<'_, TaskResult<Option<Task>>> {
        async move {
            let _guard = self
                .mutation_lock
                .lock()
                .map_err(|error| TaskError::InvalidFile(error.to_string()))?;
            self.ensure_dirs()?;
            let tasks = self.list_blocking()?;
            let candidate = tasks
                .iter()
                .filter(|task| {
                    request
                        .task_id
                        .as_ref()
                        .is_none_or(|id| task.id.as_str() == id)
                })
                .find(|task| task.is_dispatch_eligible(&tasks))
                .cloned();
            let Some(mut task) = candidate else {
                return Ok(None);
            };
            let old_path = self.task_path(TaskStatus::Todo, &task.id);
            let new_path = self.task_path(TaskStatus::Doing, &task.id);
            task.status = TaskStatus::Doing;
            task.claimed_by = Some(request.claimant);
            task.updated_at = Utc::now();
            match std::fs::rename(&old_path, &new_path) {
                Ok(()) => {
                    Self::write_task_at(&new_path, &task)?;
                    Ok(Some(task))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(TaskError::Io(error)),
            }
        }
        .boxed()
    }

    fn transition(&self, transition: TransitionTask) -> BoxFuture<'_, TaskResult<Task>> {
        async move {
            let _guard = self
                .mutation_lock
                .lock()
                .map_err(|error| TaskError::InvalidFile(error.to_string()))?;
            self.ensure_dirs()?;
            let Some((old_status, old_path)) = self.find_path(&transition.id)? else {
                return Err(TaskError::NotFound(transition.id));
            };
            let mut task = Self::read_task_at(&old_path)?;
            task.status = transition.status;
            task.updated_at = Utc::now();
            let new_path = self.task_path(task.status, &task.id);
            if old_status != task.status {
                std::fs::rename(&old_path, &new_path)?;
            }
            Self::write_task_at(&new_path, &task)?;
            Ok(task)
        }
        .boxed()
    }

    fn get(&self, id: &str) -> BoxFuture<'_, TaskResult<Option<Task>>> {
        let id = id.to_string();
        async move {
            let Some((_, path)) = self.find_path(&id)? else {
                return Ok(None);
            };
            Self::read_task_at(&path).map(Some)
        }
        .boxed()
    }

    fn list(&self) -> BoxFuture<'_, TaskResult<Vec<Task>>> {
        async move { self.list_blocking() }.boxed()
    }

    fn find(&self, query: &str) -> BoxFuture<'_, TaskResult<Vec<Task>>> {
        let query = query.to_ascii_lowercase();
        async move {
            let mut tasks = self
                .list_blocking()?
                .into_iter()
                .filter(|task| {
                    format!("{} {} {}", task.id, task.title, task.description)
                        .to_ascii_lowercase()
                        .contains(&query)
                })
                .collect::<Vec<_>>();
            tasks.sort_by_key(|task| task.updated_at);
            tasks.reverse();
            Ok(tasks)
        }
        .boxed()
    }
}

impl FilesTaskStore {
    fn list_blocking(&self) -> TaskResult<Vec<Task>> {
        self.ensure_dirs()?;
        let mut tasks = Vec::new();
        for status in [
            TaskStatus::Todo,
            TaskStatus::Doing,
            TaskStatus::Done,
            TaskStatus::Blocked,
        ] {
            let dir = self.status_dir(status);
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.extension().is_some_and(|extension| extension == "md") {
                    tasks.push(Self::read_task_at(&path)?);
                }
            }
        }
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(tasks)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskHeader {
    #[serde(rename = "type")]
    kind_name: String,
    id: String,
    title: String,
    role: artesian_core::Role,
    status: TaskStatus,
    kind: TaskKind,
    blockers: Vec<String>,
    children: Vec<String>,
    claimed_by: Option<String>,
    verifier_names: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(flatten)]
    _unknown: BTreeMap<String, serde_yaml::Value>,
}

fn render_task(task: &Task) -> TaskResult<String> {
    let header = TaskHeader {
        kind_name: "task".to_string(),
        id: task.id.clone(),
        title: task.title.clone(),
        role: task.role,
        status: task.status,
        kind: task.kind,
        blockers: task.blockers.clone(),
        children: task.children.clone(),
        claimed_by: task.claimed_by.clone(),
        verifier_names: task.verifier_names.clone(),
        created_at: task.created_at,
        updated_at: task.updated_at,
        _unknown: BTreeMap::new(),
    };
    Ok(format!(
        "---\n{}---\n\n{}\n",
        serde_yaml::to_string(&header)?,
        task.description
    ))
}

fn parse_task(text: &str) -> TaskResult<Task> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| TaskError::InvalidFile("missing task front matter".to_string()))?;
    let (header, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| TaskError::InvalidFile("unterminated task front matter".to_string()))?;
    let header: TaskHeader = serde_yaml::from_str(header)?;
    if !header.kind_name.eq_ignore_ascii_case("task") {
        return Err(TaskError::InvalidFile(format!(
            "unsupported OKF type: {}",
            header.kind_name
        )));
    }
    Ok(Task {
        id: header.id,
        title: header.title,
        description: body.trim().to_string(),
        role: header.role,
        status: header.status,
        kind: header.kind,
        blockers: header.blockers,
        children: header.children,
        claimed_by: header.claimed_by,
        verifier_names: header.verifier_names,
        created_at: header.created_at,
        updated_at: header.updated_at,
    })
}

fn inferred_task_from_markdown(path: &Path, text: &str) -> TaskResult<Task> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(TaskError::InvalidFile(
            "empty task-like markdown".to_string(),
        ));
    }
    let now = Utc::now();
    let title = first_heading(trimmed)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(humanize_stem)
        })
        .unwrap_or_else(|| "Imported task".to_string());
    let status = status_from_path(path).unwrap_or(TaskStatus::Todo);
    Ok(Task {
        id: stable_imported_task_id(trimmed),
        title,
        description: trimmed.to_string(),
        role: artesian_core::Role::Worker,
        status,
        kind: TaskKind::Primitive,
        blockers: Vec::new(),
        children: Vec::new(),
        claimed_by: None,
        verifier_names: Vec::new(),
        created_at: now,
        updated_at: now,
    })
}

fn first_heading(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
    })
}

fn humanize_stem(stem: &str) -> String {
    stem.replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_from_path(path: &Path) -> Option<TaskStatus> {
    path.components().find_map(|component| {
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        match name.as_str() {
            "todo" => Some(TaskStatus::Todo),
            "doing" => Some(TaskStatus::Doing),
            "done" => Some(TaskStatus::Done),
            "blocked" => Some(TaskStatus::Blocked),
            _ => None,
        }
    })
}

fn stable_imported_task_id(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("task-{}", &digest[..12])
}

fn generated_task_id(title: &str, now: DateTime<Utc>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(title.as_bytes());
    hasher.update(now.to_rfc3339().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("task-{}", &digest[..12])
}
