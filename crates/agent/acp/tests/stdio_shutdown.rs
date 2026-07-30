//! Shutdown regression coverage for the stdio agent transport.
//!
//! Cowboy's process exit blocks on the in-flight read of the agent's stdout
//! pipe. Terminating only the directly spawned agent process is not enough when
//! that agent left descendants behind: descendants inherit the stdout write
//! handle, so the reader never observes EOF and process shutdown hangs forever.

use std::time::Duration;

use cowboy_agent_acp::transport::stdio::StdioTransport;
use cowboy_agent_acp::transport::{StdioConfig, Transport};

/// Command that spawns a long-lived descendant inheriting the transport's
/// stdout pipe, reports readiness, and then stays alive like a real agent.
///
/// Both descendants must be forked *before* the readiness line is written: a
/// reader that observes "ready" and force-terminates immediately can
/// otherwise race ahead of a fork that happens only after the write,
/// terminating the group before that descendant even exists (and is
/// therefore never a member of the terminated group).
fn agent_leaving_a_descendant() -> StdioConfig {
    #[cfg(windows)]
    let (command, args) = (
        "powershell".to_string(),
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -NoNewWindow; Write-Output ready; Start-Sleep -Seconds 30".to_string(),
        ],
    );

    #[cfg(not(windows))]
    let (command, args) = (
        "sh".to_string(),
        vec![
            "-c".to_string(),
            "sleep 30 & sleep 30 & echo ready; wait".to_string(),
        ],
    );

    StdioConfig {
        command,
        args,
        env: vec![],
    }
}

#[tokio::test]
async fn force_terminate_releases_stdout_when_agent_left_descendants() {
    let mut transport = StdioTransport::connect(&agent_leaving_a_descendant(), &[])
        .await
        .expect("spawn agent subprocess");

    let ready = tokio::time::timeout(Duration::from_secs(30), transport.recv())
        .await
        .expect("agent readiness line within 30s")
        .expect("read agent readiness line");
    assert_eq!(ready.as_deref(), Some("ready"));

    transport
        .force_terminate()
        .await
        .expect("force terminate agent subprocess");

    let drained = tokio::time::timeout(Duration::from_secs(5), transport.recv()).await;

    assert!(
        drained.is_ok(),
        "stdout read never completed after force_terminate; a surviving descendant still holds \
         the agent stdout pipe, so Cowboy blocks forever on shutdown"
    );
}
