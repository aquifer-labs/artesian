// SPDX-License-Identifier: Apache-2.0

//! Process-backed [`brunnr_core::Agent`] adapter.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use brunnr_core::{
    Agent, AgentCapabilities, AgentError, AgentEvent, AgentEventStream, AgentMessage,
    AgentResponse, AgentResult, AgentSession, Role, SpawnRequest,
};
use futures_util::{future::BoxFuture, stream, FutureExt};
use tokio::{io::AsyncWriteExt, process::Command, time};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessAgentConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub timeout: Duration,
}

impl ProcessAgentConfig {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            working_dir: None,
            timeout: DEFAULT_TIMEOUT,
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

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[derive(Debug, Clone)]
pub struct ProcessAgent {
    config: ProcessAgentConfig,
    sessions: Arc<Mutex<HashMap<String, SessionContext>>>,
    next_session: Arc<AtomicU64>,
}

impl ProcessAgent {
    pub fn new(config: ProcessAgentConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_session: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl Agent for ProcessAgent {
    fn spawn(&self, request: SpawnRequest) -> BoxFuture<'_, AgentResult<AgentSession>> {
        async move {
            if self.config.command.trim().is_empty() {
                return Err(AgentError::Unavailable(
                    "process agent command is empty".to_string(),
                ));
            }
            let id = format!(
                "{}-{}-{}",
                request.role.canonical_alias(),
                sanitize_agent_id(&request.agent),
                self.next_session.fetch_add(1, Ordering::Relaxed)
            );
            let context = SessionContext {
                role: request.role,
                agent: request.agent.clone(),
                model: request.model.clone(),
                working_dir: request
                    .working_dir
                    .map(PathBuf::from)
                    .or_else(|| self.config.working_dir.clone()),
            };
            self.sessions
                .lock()
                .map_err(|error| AgentError::Session(error.to_string()))?
                .insert(id.clone(), context);
            Ok(AgentSession {
                id,
                role: request.role,
                agent: request.agent,
            })
        }
        .boxed()
    }

    fn send(
        &self,
        session: &AgentSession,
        message: AgentMessage,
    ) -> BoxFuture<'_, AgentResult<AgentResponse>> {
        let session_id = session.id.clone();
        async move {
            let context = self
                .sessions
                .lock()
                .map_err(|error| AgentError::Session(error.to_string()))?
                .get(&session_id)
                .cloned()
                .ok_or_else(|| AgentError::Session(format!("unknown session: {session_id}")))?;
            let output = run_process(&self.config, &context, &message.content).await?;
            Ok(AgentResponse { content: output })
        }
        .boxed()
    }

    fn stream(
        &self,
        session: &AgentSession,
        message: AgentMessage,
    ) -> BoxFuture<'_, AgentResult<AgentEventStream>> {
        let session = session.clone();
        async move {
            let response = self.send(&session, message).await?;
            Ok(Box::pin(stream::iter([
                Ok(AgentEvent::Text(response.content)),
                Ok(AgentEvent::Done),
            ])) as AgentEventStream)
        }
        .boxed()
    }

    fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities {
            streaming: false,
            tools: false,
            mcp: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionContext {
    role: Role,
    agent: String,
    model: Option<String>,
    working_dir: Option<PathBuf>,
}

async fn run_process(
    config: &ProcessAgentConfig,
    context: &SessionContext,
    prompt: &str,
) -> AgentResult<String> {
    let mut command = Command::new(&config.command);
    let mut prompt_was_arg = false;
    for arg in &config.args {
        let rendered = render_arg(arg, context, prompt);
        prompt_was_arg |= arg.contains("{prompt}");
        command.arg(rendered);
    }
    if let Some(working_dir) = &context.working_dir {
        command.current_dir(working_dir);
    }
    command.kill_on_drop(true);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| AgentError::Unavailable(error.to_string()))?;

    if !prompt_was_arg && !prompt.is_empty() {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|error| AgentError::Session(error.to_string()))?;
        }
    }

    let output = time::timeout(config.timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            AgentError::Session(format!(
                "process timed out after {}s",
                config.timeout.as_secs()
            ))
        })?
        .map_err(|error| AgentError::Session(error.to_string()))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        return Err(AgentError::Session(format!(
            "process exited with status {}: {}",
            output.status, text
        )));
    }
    Ok(text)
}

fn render_arg(template: &str, context: &SessionContext, prompt: &str) -> String {
    template
        .replace("{prompt}", prompt)
        .replace("{role}", context.role.canonical_alias())
        .replace("{alias}", context.role.norse_alias())
        .replace("{agent}", &context.agent)
        .replace("{model}", context.model.as_deref().unwrap_or_default())
}

fn sanitize_agent_id(agent: &str) -> String {
    let mut output = String::new();
    for character in agent.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else {
            output.push('-');
        }
    }
    let output = output.trim_matches('-');
    if output.is_empty() {
        "agent".to_string()
    } else {
        output.to_string()
    }
}

#[cfg(test)]
mod tests {
    use brunnr_core::{AgentMessage, Role, SpawnRequest};

    use super::*;

    #[tokio::test]
    async fn launches_real_echo_subprocess() {
        let agent =
            ProcessAgent::new(ProcessAgentConfig::new("echo").with_args(vec!["brunnr".into()]));
        let session = agent
            .spawn(SpawnRequest {
                role: Role::Worker,
                agent: "echo".to_string(),
                model: None,
                working_dir: None,
            })
            .await
            .expect("spawn should register session");
        let response = agent
            .send(
                &session,
                AgentMessage {
                    content: String::new(),
                },
            )
            .await
            .expect("echo should launch");

        assert_eq!(response.content.trim(), "brunnr");
    }
}
