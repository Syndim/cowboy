//! Live ACP integration tests against Oh My Pi (`omp acp`).
//!
//! `#[ignore]`d: requires an authenticated Oh My Pi CLI on `PATH`. Run with
//! `just acp-test omp` or `cargo test --test omp_acp -- --ignored`.
//! See [`support`] for env-var overrides.

mod support;

use cowboy_agent_acp::BackendPreset;
use support::{AcpBackend, run_initialize, run_session_prompt};

const OMP: AcpBackend = AcpBackend {
    name: "Oh My Pi",
    env_prefix: "COWBOY_OMP",
    preset: &BackendPreset::OMP,
};

#[tokio::test]
#[ignore = "requires authenticated Oh My Pi CLI (`omp acp`) on PATH; run with --ignored"]
async fn omp_acp_initializes() -> anyhow::Result<()> {
    run_initialize(&OMP).await
}

#[tokio::test]
#[ignore = "requires authenticated Oh My Pi CLI (`omp acp`) on PATH; run with --ignored"]
async fn omp_acp_creates_session_and_answers_prompt() -> anyhow::Result<()> {
    run_session_prompt(&OMP).await
}
