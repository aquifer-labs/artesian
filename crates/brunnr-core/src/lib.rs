// SPDX-License-Identifier: Apache-2.0

//! Core role, agent-adapter, queue, and configuration seams for Brunnr.

mod agent;
mod config;
mod roles;

pub use agent::{
    Agent, AgentCapabilities, AgentError, AgentEvent, AgentEventStream, AgentMessage,
    AgentResponse, AgentResult, AgentSession, SpawnRequest,
};
pub use config::{AgentBinding, BrunnrConfig, MemoryBackendKind, MemoryConfig, Mode};
pub use roles::{Erindi, ErindiStatus, Galdr, Role, RoleParseError, Thing};
