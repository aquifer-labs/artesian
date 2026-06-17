// SPDX-License-Identifier: Apache-2.0

use std::{path::PathBuf, sync::Arc};

use futures_util::{future::BoxFuture, FutureExt};
use tokio::process::Command;

use crate::{Task, TaskError, TaskResult, TaskStatus, TaskStore, TransitionTask};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierOutcome {
    pub name: String,
    pub passed: bool,
    pub output: String,
}

pub trait Verifier: Send + Sync {
    fn name(&self) -> &str;

    fn verify(&self, task: &Task) -> BoxFuture<'_, TaskResult<VerifierOutcome>>;
}

#[derive(Debug, Clone)]
pub struct CommandVerifier {
    name: String,
    command: String,
    args: Vec<String>,
    working_dir: Option<PathBuf>,
}

impl CommandVerifier {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            working_dir: None,
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn with_working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }
}

impl Verifier for CommandVerifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn verify(&self, _task: &Task) -> BoxFuture<'_, TaskResult<VerifierOutcome>> {
        async move {
            let mut command = Command::new(&self.command);
            command.args(&self.args);
            if let Some(working_dir) = &self.working_dir {
                command.current_dir(working_dir);
            }
            let output = command.output().await?;
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok(VerifierOutcome {
                name: self.name.clone(),
                passed: output.status.success(),
                output: text,
            })
        }
        .boxed()
    }
}

#[derive(Clone, Default)]
pub struct VerifierGate {
    verifiers: Vec<Arc<dyn Verifier>>,
}

impl VerifierGate {
    pub fn new(verifiers: Vec<Arc<dyn Verifier>>) -> Self {
        Self { verifiers }
    }

    pub fn is_empty(&self) -> bool {
        self.verifiers.is_empty()
    }

    pub async fn verify(&self, task: &Task) -> TaskResult<Vec<VerifierOutcome>> {
        let mut outcomes = Vec::new();
        for verifier in &self.verifiers {
            let outcome = verifier.verify(task).await?;
            if !outcome.passed {
                return Err(TaskError::Verifier(format!(
                    "{} failed: {}",
                    outcome.name, outcome.output
                )));
            }
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    pub async fn mark_done<S: TaskStore>(&self, store: &S, id: &str) -> TaskResult<Task> {
        let task = store
            .get(id)
            .await?
            .ok_or_else(|| TaskError::NotFound(id.to_string()))?;
        self.verify(&task).await?;
        store
            .transition(TransitionTask {
                id: id.to_string(),
                status: TaskStatus::Done,
            })
            .await
    }
}

pub trait ExternalConnector: Send + Sync {
    fn name(&self) -> &str;

    fn mirror_task(&self, task: &Task) -> BoxFuture<'_, TaskResult<()>>;
}

#[derive(Debug, Clone)]
pub struct McpExternalConnector {
    name: String,
    enabled: bool,
}

impl McpExternalConnector {
    pub fn new(name: impl Into<String>, enabled: bool) -> Self {
        Self {
            name: name.into(),
            enabled,
        }
    }
}

impl ExternalConnector for McpExternalConnector {
    fn name(&self) -> &str {
        &self.name
    }

    fn mirror_task(&self, _task: &Task) -> BoxFuture<'_, TaskResult<()>> {
        async move {
            if self.enabled {
                return Ok(());
            }
            Ok(())
        }
        .boxed()
    }
}
