//! Live ACP integration tests against GitHub Copilot (`copilot --acp`).
//!
//! `#[ignore]`d: requires an authenticated Copilot CLI on `PATH`. Run with
//! `just acp-test copilot` or `cargo test --test copilot_acp -- --ignored`.
//! See [`support`] for env-var overrides.

mod support;

use cowboy_agent_acp::BackendPreset;
use support::{AcpBackend, run_initialize, run_session_prompt};

const COPILOT: AcpBackend = AcpBackend {
    name: "GitHub Copilot",
    env_prefix: "COWBOY_COPILOT",
    preset: &BackendPreset::COPILOT,
};

#[tokio::test]
#[ignore = "requires authenticated GitHub Copilot CLI (`copilot --acp`) on PATH; run with --ignored"]
async fn copilot_acp_initializes() -> anyhow::Result<()> {
    run_initialize(&COPILOT).await
}

#[tokio::test]
#[ignore = "requires authenticated GitHub Copilot CLI (`copilot --acp`) on PATH; run with --ignored"]
async fn copilot_acp_creates_session_and_answers_prompt() -> anyhow::Result<()> {
    run_session_prompt(&COPILOT).await
}
