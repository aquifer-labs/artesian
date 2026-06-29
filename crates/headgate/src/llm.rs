// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "llm")]

//! LLM client seam for the ACC control plane.
//!
//! Both LLM-backed ACC components — the [`crate::JudgeQualifyGate`] and the
//! [`crate::LlmCompressor`] — talk to a model through the [`LlmClient`] trait. Two transports
//! ship: [`OpenAiCompatibleClient`] for any OpenAI-compatible `/chat/completions` server
//! (Ollama, LM Studio, `mlx_lm.server`, vLLM, or a hosted endpoint) and [`CommandLlmClient`]
//! for an agent CLI (Codex / Claude Code / Gemini / opencode) invoked as a subprocess.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::{HeadgateError, HeadgateResult};

/// A single chat completion request.
#[derive(Debug, Clone, Default)]
pub struct LlmRequest {
    pub system: Option<String>,
    pub prompt: String,
    pub max_tokens: Option<usize>,
    pub temperature: Option<f32>,
}

impl LlmRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Self::default()
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }
}

/// A model the ACC components can call for judging or compression.
pub trait LlmClient: Send + Sync {
    fn label(&self) -> &str {
        "judge"
    }

    fn complete(&self, request: LlmRequest) -> BoxFuture<'_, HeadgateResult<String>>;
}

/// Client for any OpenAI-compatible `/chat/completions` endpoint.
///
/// `base_url` is the API root including the version segment, e.g. `http://localhost:11434/v1`
/// (Ollama), `http://localhost:1234/v1` (LM Studio), `http://localhost:8080/v1`
/// (`mlx_lm.server`). An optional bearer `api_key` is sent when present.
pub struct OpenAiCompatibleClient {
    base_url: String,
    model: String,
    api_key: Option<String>,
    timeout: Duration,
    client: reqwest::Client,
}

impl OpenAiCompatibleClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            api_key: None,
            timeout: Duration::from_secs(120),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

impl LlmClient for OpenAiCompatibleClient {
    fn label(&self) -> &str {
        &self.model
    }

    fn complete(&self, request: LlmRequest) -> BoxFuture<'_, HeadgateResult<String>> {
        async move {
            let mut messages = Vec::new();
            if let Some(system) = &request.system {
                messages.push(ChatMessage {
                    role: "system",
                    content: system,
                });
            }
            messages.push(ChatMessage {
                role: "user",
                content: &request.prompt,
            });

            let body = ChatRequest {
                model: &self.model,
                messages,
                max_tokens: request.max_tokens,
                temperature: request.temperature,
                stream: false,
            };

            let mut builder = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .timeout(self.timeout)
                .json(&body);
            if let Some(api_key) = &self.api_key {
                builder = builder.bearer_auth(api_key);
            }

            let response = builder
                .send()
                .await
                .map_err(|error| HeadgateError::Llm(format!("request failed: {error}")))?;
            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(HeadgateError::Llm(format!("HTTP {status}: {text}")));
            }
            let parsed: ChatResponse = response
                .json()
                .await
                .map_err(|error| HeadgateError::Llm(format!("decode failed: {error}")))?;
            parsed
                .choices
                .into_iter()
                .next()
                .map(|choice| choice.message.content)
                .ok_or_else(|| HeadgateError::Llm("response had no choices".to_string()))
        }
        .boxed()
    }
}

/// Client that shells out to an agent CLI, writing the prompt to stdin and reading the
/// completion from stdout. `{system}` and `{prompt}` placeholders in `args` are substituted;
/// if neither appears, the prompt is piped via stdin.
pub struct CommandLlmClient {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    timeout: Duration,
}

impl CommandLlmClient {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            env: HashMap::new(),
            timeout: Duration::from_secs(120),
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl LlmClient for CommandLlmClient {
    fn label(&self) -> &str {
        &self.command
    }

    fn complete(&self, request: LlmRequest) -> BoxFuture<'_, HeadgateResult<String>> {
        async move {
            let system = request.system.clone().unwrap_or_default();
            let uses_placeholder = self
                .args
                .iter()
                .any(|arg| arg.contains("{prompt}") || arg.contains("{system}"));
            let rendered_args: Vec<String> = self
                .args
                .iter()
                .map(|arg| {
                    arg.replace("{prompt}", &request.prompt)
                        .replace("{system}", &system)
                })
                .collect();

            let mut command = tokio::process::Command::new(&self.command);
            command
                .args(&rendered_args)
                .envs(&self.env)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if !uses_placeholder {
                command.stdin(Stdio::piped());
            }

            let mut child = command
                .spawn()
                .map_err(|error| HeadgateError::Llm(format!("spawn failed: {error}")))?;
            if !uses_placeholder {
                if let Some(mut stdin) = child.stdin.take() {
                    let payload = if system.is_empty() {
                        request.prompt.clone()
                    } else {
                        format!("{system}\n\n{}", request.prompt)
                    };
                    stdin
                        .write_all(payload.as_bytes())
                        .await
                        .map_err(|error| HeadgateError::Llm(format!("stdin failed: {error}")))?;
                    drop(stdin);
                }
            }

            let output = tokio::time::timeout(self.timeout, child.wait_with_output())
                .await
                .map_err(|_| HeadgateError::Llm("command timed out".to_string()))?
                .map_err(|error| HeadgateError::Llm(format!("command failed: {error}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(HeadgateError::Llm(format!(
                    "command exited with {}: {stderr}",
                    output.status
                )));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        .boxed()
    }
}

/// Build an [`LlmClient`] from an [`artesian_core::AccLlmConfig`]. The bearer key, when
/// `api_key_env` is set, is read from that environment variable at build time.
pub fn llm_client_from_config(
    config: &artesian_core::AccLlmConfig,
) -> HeadgateResult<std::sync::Arc<dyn LlmClient>> {
    match config.provider.as_str() {
        "openai" | "openai-compatible" => {
            let base_url = config.base_url.clone().ok_or_else(|| {
                HeadgateError::Llm("openai provider requires base_url".to_string())
            })?;
            let model = config
                .model
                .clone()
                .ok_or_else(|| HeadgateError::Llm("openai provider requires model".to_string()))?;
            let mut client = OpenAiCompatibleClient::new(base_url, model);
            if let Some(env) = &config.api_key_env {
                if let Ok(key) = std::env::var(env) {
                    client = client.with_api_key(key);
                }
            }
            Ok(std::sync::Arc::new(client))
        }
        // Local OpenAI-compatible servers — zero token cost, private, no cloud dependency.
        // Default ports: Ollama :11434, LM Studio :1234, mlx_lm.server :8080.
        "ollama" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434/v1".to_string());
            let model = config
                .model
                .clone()
                .ok_or_else(|| HeadgateError::Llm("ollama provider requires model".to_string()))?;
            Ok(std::sync::Arc::new(OpenAiCompatibleClient::new(
                base_url, model,
            )))
        }
        "lm-studio" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:1234/v1".to_string());
            let model = config.model.clone().ok_or_else(|| {
                HeadgateError::Llm("lm-studio provider requires model".to_string())
            })?;
            Ok(std::sync::Arc::new(OpenAiCompatibleClient::new(
                base_url, model,
            )))
        }
        "mlx" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:8080/v1".to_string());
            let model = config
                .model
                .clone()
                .unwrap_or_else(|| "default".to_string());
            Ok(std::sync::Arc::new(OpenAiCompatibleClient::new(
                base_url, model,
            )))
        }
        "command" => {
            let command = config.command.clone().ok_or_else(|| {
                HeadgateError::Llm("command provider requires command".to_string())
            })?;
            Ok(std::sync::Arc::new(CommandLlmClient::new(
                command,
                config.args.clone(),
            )))
        }
        other => Err(HeadgateError::Llm(format!("unknown llm provider: {other}"))),
    }
}

/// A canned client for tests and offline development — returns a fixed response.
pub struct StaticLlmClient {
    response: String,
}

impl StaticLlmClient {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
        }
    }
}

impl LlmClient for StaticLlmClient {
    fn label(&self) -> &str {
        "static"
    }

    fn complete(&self, _request: LlmRequest) -> BoxFuture<'_, HeadgateResult<String>> {
        let response = self.response.clone();
        async move { Ok(response) }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_client_returns_canned_response() {
        let client = StaticLlmClient::new("hello");
        let out = client
            .complete(LlmRequest::new("anything"))
            .await
            .expect("complete");
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn command_client_pipes_prompt_via_stdin() {
        // `cat` echoes stdin back — a stand-in for an agent CLI.
        let client = CommandLlmClient::new("cat", vec![]);
        let out = client
            .complete(LlmRequest::new("round-trip"))
            .await
            .expect("complete");
        assert!(out.contains("round-trip"));
    }

    #[tokio::test]
    async fn command_client_substitutes_prompt_placeholder() {
        let client = CommandLlmClient::new("echo", vec!["[{prompt}]".to_string()]);
        let out = client
            .complete(LlmRequest::new("xyz"))
            .await
            .expect("complete");
        assert_eq!(out, "[xyz]");
    }

    #[test]
    fn factory_builds_clients_and_rejects_bad_config() {
        use artesian_core::AccLlmConfig;

        let openai = AccLlmConfig {
            provider: "openai".to_string(),
            base_url: Some("http://localhost:11434/v1".to_string()),
            model: Some("llama3".to_string()),
            api_key_env: None,
            command: None,
            args: Vec::new(),
        };
        assert!(llm_client_from_config(&openai).is_ok());

        let command = AccLlmConfig {
            provider: "command".to_string(),
            base_url: None,
            model: None,
            api_key_env: None,
            command: Some("cat".to_string()),
            args: Vec::new(),
        };
        assert!(llm_client_from_config(&command).is_ok());

        // openai without base_url/model is rejected.
        let incomplete = AccLlmConfig {
            provider: "openai".to_string(),
            base_url: None,
            model: None,
            api_key_env: None,
            command: None,
            args: Vec::new(),
        };
        assert!(llm_client_from_config(&incomplete).is_err());

        let unknown = AccLlmConfig {
            provider: "telepathy".to_string(),
            ..incomplete
        };
        assert!(llm_client_from_config(&unknown).is_err());
    }
}
