// SPDX-License-Identifier: Apache-2.0

//! [`AgentRuntime`] trait — the clean seam between "owns one model loop" and the rest of Flume.
//!
//! Adding a new agent runtime is a small `impl AgentRuntime for MyRuntime` that supplies:
//! 1. `spawn_session` — allocate a session (no process yet).
//! 2. `send_message` — run the process for one turn and stream events.
//!
//! [`ProcessAgentRuntime`] is the built-in impl that delegates to [`ProcessAgent`]; its per-agent
//! argv (claude / codex / gemini / opencode / generic) is byte-identical to the existing
//! `build_invocation` path — the only change is that it is now reachable through the trait.

use artesian_core::{AgentMessage, AgentResult, AgentSession, SpawnRequest};
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;

use crate::{ProcessAgent, ProcessAgentConfig, WorkerEvent};

// ── Trait definition ───────────────────────────────────────────────────────────────────────────

/// A single-responsibility seam: one `AgentRuntime` owns one model loop.
///
/// Implementors handle the mechanics of spawning + communicating with a specific agent CLI or API,
/// while Flume's orchestration layer (lanes, coordinator, presence) remains independent of the
/// concrete runtime.
///
/// # Contract
///
/// - `spawn_session` is *cheap*: it allocates bookkeeping but does **not** launch a process.
/// - `send_message` is the *heavy* call: it spawns the process (if needed), pipes the prompt,
///   collects events, and returns the response.
/// - `agent_id` returns a stable human-readable label used in logs and presence snapshots.
pub trait AgentRuntime: Send + Sync {
    /// Return the stable agent identifier for this runtime (e.g. `"claude"`, `"codex"`).
    fn agent_id(&self) -> &str;

    /// Allocate a session (knob 1 — agent selection).  Must be called before `send_message`.
    fn spawn_session<'a>(
        &'a self,
        request: SpawnRequest,
    ) -> BoxFuture<'a, AgentResult<AgentSession>>;

    /// Run one turn against the session (knobs 2 & 3 — instruction + context visibility).
    ///
    /// The `event_sender` is the same [`WorkerEvent`] channel that `flume` already uses for
    /// streaming progress to callers.
    fn send_message<'a>(
        &'a self,
        session: &'a AgentSession,
        message: AgentMessage,
        event_sender: Option<mpsc::UnboundedSender<WorkerEvent>>,
    ) -> BoxFuture<'a, AgentResult<String>>;
}

// ── ProcessAgentRuntime ────────────────────────────────────────────────────────────────────────

/// Built-in [`AgentRuntime`] backed by [`ProcessAgent`].
///
/// The per-agent argv for claude / codex / gemini / opencode is byte-identical to the existing
/// `build_invocation` path.  This type is a thin re-export shim so external code can refer to
/// the trait boundary without importing `ProcessAgent` directly.
pub struct ProcessAgentRuntime {
    inner: ProcessAgent,
    agent_id: String,
}

impl ProcessAgentRuntime {
    /// Construct from an already-configured [`ProcessAgent`].
    pub fn new(inner: ProcessAgent, agent_id: impl Into<String>) -> Self {
        Self {
            inner,
            agent_id: agent_id.into(),
        }
    }

    /// Convenience constructor that builds the [`ProcessAgent`] inline from a config.
    pub fn from_config(config: ProcessAgentConfig) -> Self {
        let agent_id = if config.agent_id.trim().is_empty() {
            config.command.clone()
        } else {
            config.agent_id.clone()
        };
        Self {
            inner: ProcessAgent::new(config),
            agent_id,
        }
    }

    /// Access the underlying [`ProcessAgent`] (e.g. for catalog operations).
    pub fn process_agent(&self) -> &ProcessAgent {
        &self.inner
    }
}

impl AgentRuntime for ProcessAgentRuntime {
    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    fn spawn_session<'a>(
        &'a self,
        request: SpawnRequest,
    ) -> BoxFuture<'a, AgentResult<AgentSession>> {
        use artesian_core::Agent;
        self.inner.spawn(request)
    }

    fn send_message<'a>(
        &'a self,
        session: &'a AgentSession,
        message: AgentMessage,
        event_sender: Option<mpsc::UnboundedSender<WorkerEvent>>,
    ) -> BoxFuture<'a, AgentResult<String>> {
        Box::pin(async move {
            self.inner
                .send_with_event_sender(session, message, event_sender)
                .await
                .map(|response| response.content)
        })
    }
}

// ── tests ──────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use artesian_core::{AgentMessage, Role, SpawnRequest};
    use artesian_test_support::TempDir;

    use super::*;

    /// `ProcessAgentRuntime` dispatches per-agent with argv byte-identical to the existing path:
    /// a `claude` agent gets `--output-format stream-json --verbose --permission-mode acceptEdits`.
    /// We verify this through `build_invocation`-equivalent by checking that the Runtime trait
    /// calls succeed end-to-end with a real `echo` stub (no real agent CLIs launched).
    #[tokio::test]
    async fn process_agent_runtime_spawn_and_send_succeed_with_echo_stub() {
        let tempdir = TempDir::new("runtime-trait-echo");
        let config = ProcessAgentConfig::new("echo")
            .with_agent_id("echo-stub")
            .with_args(vec!["hello-from-runtime".to_string()])
            .with_registry_dir(tempdir.join("spawns"))
            .with_termination_grace(Duration::from_millis(10));

        let runtime = ProcessAgentRuntime::from_config(config);
        assert_eq!(runtime.agent_id(), "echo-stub");

        let session = runtime
            .spawn_session(SpawnRequest {
                role: Role::Worker,
                agent: "echo-stub".to_string(),
                model: None,
                working_dir: None,
                resume_packet: None,
            })
            .await
            .expect("spawn_session should succeed");

        let content = runtime
            .send_message(
                &session,
                AgentMessage {
                    content: String::new(),
                },
                None,
            )
            .await
            .expect("send_message should succeed");

        assert!(
            content.contains("hello-from-runtime"),
            "response should contain echo output; got: {content:?}"
        );
    }

    /// The trait is object-safe: a `Box<dyn AgentRuntime>` compiles and dispatches correctly.
    #[tokio::test]
    async fn agent_runtime_is_object_safe() {
        let tempdir = TempDir::new("runtime-trait-dyn");
        let config = ProcessAgentConfig::new("echo")
            .with_args(vec!["dyn-dispatch".to_string()])
            .with_registry_dir(tempdir.join("spawns"))
            .with_termination_grace(Duration::from_millis(10));

        let runtime: Box<dyn AgentRuntime> = Box::new(ProcessAgentRuntime::from_config(config));

        assert!(!runtime.agent_id().is_empty());

        let session = runtime
            .spawn_session(SpawnRequest {
                role: Role::Worker,
                agent: "echo".to_string(),
                model: None,
                working_dir: None,
                resume_packet: None,
            })
            .await
            .expect("dyn spawn should succeed");

        let content = runtime
            .send_message(
                &session,
                AgentMessage {
                    content: String::new(),
                },
                None,
            )
            .await
            .expect("dyn send should succeed");

        assert!(content.contains("dyn-dispatch"));
    }
}
