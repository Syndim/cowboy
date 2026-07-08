//! Shared harness for live ACP backend integration tests.
//!
//! Each backend (GitHub Copilot, Oh My Pi, ...) speaks the same Agent Client
//! Protocol over stdio, so the same test bodies can target any of them. A
//! backend is described by [`AcpBackend`]; every field has a sensible default
//! and can be overridden at runtime via `{env_prefix}_*` environment variables:
//!
//! - `{PREFIX}_COMMAND`      — executable (default: `default_command`)
//! - `{PREFIX}_ARGS`         — whitespace-separated args (default: `default_args`)
//! - `{PREFIX}_MODEL`        — model id (default: `default_model`)
//! - `{PREFIX}_PROVIDER`     — provider; empty string clears it (default: `default_provider`)
//! - `{PREFIX}_CWD`          — working dir for the session (default: current dir)
//! - `{PREFIX}_TIMEOUT_SECS` — per-operation timeout (default: 120)
//!
//! These tests are `#[ignore]`d because they require an authenticated agent CLI
//! on `PATH`. Run one backend with `cargo test --test <backend>_acp -- --ignored`
//! or `just acp-test <backend>`.
#![allow(dead_code)]

use std::time::Duration;

use cowboy_agent_acp::transport::StdioConfig;
use cowboy_agent_acp::{BackendPreset, Client, TransportConfig};
use cowboy_agent_client::{Event, ModelInfo, PromptContent, StopReason};
use serde_json::Value;

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// An ACP-compatible coding-agent backend under integration test.
pub struct AcpBackend {
    /// Human-readable label used in assertion/timeout messages.
    pub name: &'static str,
    /// Environment variable prefix, e.g. `COWBOY_COPILOT` or `COWBOY_OMP`.
    pub env_prefix: &'static str,
    /// Shared launch/model preset; env vars below override its fields.
    pub preset: &'static BackendPreset,
}

impl AcpBackend {
    fn env(&self, suffix: &str) -> Option<String> {
        std::env::var(format!("{}_{}", self.env_prefix, suffix)).ok()
    }

    /// Stdio transport config from env overrides or defaults.
    pub fn transport_config(&self) -> TransportConfig {
        let command = self
            .env("COMMAND")
            .unwrap_or_else(|| self.preset.command.to_string());
        let args = match self.env("ARGS") {
            Some(raw) => raw.split_whitespace().map(str::to_string).collect(),
            None => self.preset.owned_args(),
        };
        TransportConfig::Stdio(StdioConfig {
            command,
            args,
            env: vec![],
        })
    }

    /// Model descriptor from env overrides or defaults.
    ///
    /// An explicit empty `{PREFIX}_PROVIDER` clears the provider field.
    pub fn model(&self) -> ModelInfo {
        let id = self
            .env("MODEL")
            .unwrap_or_else(|| self.preset.model.to_string());
        let provider = match self.env("PROVIDER") {
            Some(provider) => (!provider.is_empty()).then_some(provider),
            None => Some(self.preset.provider.to_string()),
        };
        ModelInfo { id, provider }
    }

    /// Per-operation timeout from env override or default.
    pub fn timeout(&self) -> Duration {
        let secs = self
            .env("TIMEOUT_SECS")
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        Duration::from_secs(secs)
    }

    /// Session working directory from env override or the current dir.
    pub fn cwd(&self) -> anyhow::Result<String> {
        match self.env("CWD") {
            Some(cwd) => Ok(cwd),
            None => Ok(std::env::current_dir()?.display().to_string()),
        }
    }

    /// Connect and run the ACP initialize handshake, bounded by the timeout.
    pub async fn connect(&self) -> anyhow::Result<Client> {
        tokio::time::timeout(self.timeout(), Client::connect(self.transport_config()))
            .await
            .map_err(|_| anyhow::anyhow!("timed out initializing {} over stdio", self.name))?
    }
}

fn chunk_text(content: &Value) -> Option<&str> {
    content
        .as_str()
        .or_else(|| content.get("text").and_then(Value::as_str))
}

/// Connect, assert the initialize handshake exposes agent metadata, then close.
pub async fn run_initialize(backend: &AcpBackend) -> anyhow::Result<()> {
    let mut client = backend.connect().await?;

    assert!(client.is_connected());
    let agent_info = client.agent_info.as_ref().unwrap_or_else(|| {
        panic!(
            "{} initialize response should include agentInfo",
            backend.name
        )
    });
    assert!(!agent_info.name.trim().is_empty());

    client.close().await?;
    Ok(())
}

/// Create a session and run one prompt, asserting a non-empty streamed reply.
pub async fn run_session_prompt(backend: &AcpBackend) -> anyhow::Result<()> {
    let mut client = backend.connect().await?;
    let model = backend.model();
    let cwd = backend.cwd()?;

    let session_id = tokio::time::timeout(backend.timeout(), client.new_session(&cwd, &[], &model))
        .await
        .map_err(|_| anyhow::anyhow!("timed out creating {} ACP session", backend.name))??;
    assert!(!session_id.trim().is_empty());
    assert_eq!(client.session_id(), Some(session_id.as_str()));

    let mut response_text = String::new();
    let stop_reason = tokio::time::timeout(
        backend.timeout(),
        client.prompt(
            &session_id,
            vec![PromptContent::text(
                "Reply with exactly one short sentence that contains the word cowboy.",
            )],
            &mut |event| {
                if let Event::MessageChunk { content } = event
                    && let Some(text) = chunk_text(&content)
                {
                    response_text.push_str(text);
                }
            },
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for {} ACP prompt response", backend.name))??;

    assert!(matches!(
        stop_reason,
        StopReason::EndTurn | StopReason::MaxTokens
    ));
    assert!(
        !response_text.trim().is_empty(),
        "{} produced no agent_message_chunk text",
        backend.name
    );

    client.close().await?;
    Ok(())
}
