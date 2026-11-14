use std::path::Path;
use std::sync::Arc;
use tokio::time::Instant;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::Instrument;
use tracing::instrument::WithSubscriber;

use crate::agent_processes::{self, RegistrationId};
use crate::client::diagnostic_error_text;
use crate::process_tree::ProcessTreeScope;

use super::{StdioConfig, Transport};

/// Stdio transport: a local subprocess using direct JSON-RPC over stdin/stdout.
pub struct StdioTransport {
    writer: BufWriter<ChildStdin>,
    reader: BufReader<ChildStdout>,
    read_buffer: Vec<u8>,
    frame_started: Option<Instant>,
    stdout_bytes: u64,
    frames: u64,
    child: Child,
    command: String,
    pid: Option<u32>,
    /// Owns the agent's whole process tree; descendants inherit the stdout
    /// pipe, so only tree termination lets the pending read reach EOF.
    tree: Arc<ProcessTreeScope>,
    registration: Option<RegistrationId>,
}

impl StdioTransport {
    /// Spawn agent subprocess and return a connected transport.
    /// Appends `additional_args` after the configured args.
    pub async fn connect(config: &StdioConfig, additional_args: &[&str]) -> anyhow::Result<Self> {
        let startup = match &config.startup_readiness {
            Some(readiness) => Some(super::startup::StartupLog::prepare(readiness, config).await?),
            None => None,
        };
        let mut cmd = Command::new(&config.command);
        for arg in &config.args {
            cmd.arg(arg);
        }
        for arg in additional_args {
            cmd.arg(arg);
        }
        if config.clear_env {
            cmd.env_clear();
            for name in &config.allowed_env {
                if let Some(value) = std::env::var_os(name) {
                    cmd.env(name, value);
                }
            }
        }
        for (key, val) in &config.env {
            cmd.env(key, val);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let tree = Arc::new(ProcessTreeScope::new()?);
        tree.configure(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn agent process '{}': {}", config.command, e)
        })?;

        if let Err(err) = tree.attach(&child) {
            let _ = child.kill().await;
            return Err(anyhow::anyhow!(
                "Failed to take ownership of agent process tree for '{}': {}",
                config.command,
                err
            ));
        }

        let registration = agent_processes::register(&tree);
        let command = Path::new(&config.command)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_logger(command.clone(), child.id(), stderr);
        }

        let writer = BufWriter::new(stdin);
        let reader = BufReader::new(stdout);

        tracing::debug!(
            event = "acp_stdio_spawned",
            command = %command,
            pid = child.id(),
            clear_env = config.clear_env,
            argument_count = config.args.len() + additional_args.len(),
            allowed_env_count = config.allowed_env.len(),
            configured_env_count = config.env.len(),
            "Agent subprocess spawned"
        );

        let mut transport = Self {
            writer,
            reader,
            read_buffer: Vec::new(),
            frame_started: None,
            stdout_bytes: 0,
            frames: 0,
            pid: child.id(),
            child,
            command,
            tree,
            registration: Some(registration),
        };
        if let Some(mut startup) = startup {
            let readiness = config.startup_readiness.as_ref().unwrap();
            let timeout = std::time::Duration::from_secs(readiness.timeout_seconds);
            let pid = transport
                .pid
                .ok_or_else(|| anyhow::anyhow!("startup readiness process ID is unavailable"))?;
            tracing::debug!(
                event = "acp_stdio_startup_wait",
                pid,
                timeout_seconds = readiness.timeout_seconds,
                "Waiting for agent startup readiness"
            );
            let result = tokio::time::timeout(timeout, async {
                loop {
                    if let Some(status) = transport.child.try_wait()? {
                        anyhow::bail!("agent exited before startup readiness: {status}");
                    }
                    if startup.ready(pid).await? {
                        if let Some(status) = transport.child.try_wait()? {
                            anyhow::bail!("agent exited before startup readiness: {status}");
                        }
                        break Ok::<(), anyhow::Error>(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "agent startup readiness timed out after {} seconds",
                    readiness.timeout_seconds
                ))
            });
            if let Err(error) = result {
                transport.terminate_tree();
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(5), transport.child.kill())
                        .await;
                return Err(error);
            }
            tracing::debug!(
                event = "acp_stdio_startup_ready",
                pid,
                "Agent startup readiness observed"
            );
        }
        Ok(transport)
    }

    /// Get a reference to the underlying child process.
    #[allow(dead_code)]
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Kill the agent's whole process tree and stop tracking it for shutdown.
    fn terminate_tree(&mut self) -> bool {
        if let Some(registration) = self.registration.take() {
            agent_processes::deregister(registration);
        }

        match self.tree.terminate() {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    command = %self.command,
                    pid = self.pid,
                    error = %error,
                    error_code = "terminate_failed",
                    "Agent process tree termination failed"
                );
                false
            }
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Some(registration) = self.registration.take() {
            agent_processes::deregister(registration);
        }
    }
}

fn log_stderr_diagnostic(command: &str, pid: Option<u32>, line: &[u8]) {
    let Ok(text) = std::str::from_utf8(line) else {
        return;
    };
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let (logger, text) = text
        .strip_prefix('[')
        .and_then(|tail| tail.split_once(']'))
        .filter(|(prefix, tail)| {
            !prefix.is_empty()
                && prefix.len() <= 64
                && prefix.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'_' | b'-' | b'.')
                })
                && tail.starts_with(char::is_whitespace)
        })
        .map_or((None, text), |(prefix, tail)| (Some(prefix), tail.trim()));
    let labeled = ["Error: ", "error: ", "ERROR ", "WARN "]
        .into_iter()
        .find_map(|prefix| text.strip_prefix(prefix));
    let text = labeled.unwrap_or(text).trim();
    let (mut error, code) = if text.starts_with(['{', '[']) {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        if json.get("jsonrpc").is_some() {
            return;
        }
        let error = json
            .get("error")
            .filter(|error| error.is_object())
            .or_else(|| {
                (json
                    .get("level")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|level| {
                        level.eq_ignore_ascii_case("error") || level.eq_ignore_ascii_case("warn")
                    })
                    || json
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .is_some())
                .then_some(&json)
            });
        let Some(error) = error else {
            return;
        };
        let Some(message) = error.get("message").and_then(serde_json::Value::as_str) else {
            return;
        };
        (
            diagnostic_error_text(message),
            error.get("code").and_then(serde_json::Value::as_i64),
        )
    } else {
        let lower = text.to_ascii_lowercase();
        let ordinary = [
            "error",
            "warn",
            "failed",
            "failure",
            "exception",
            "invalid",
            "not valid",
            "not found",
            "broken pipe",
            "refused",
            "denied",
            "timed out",
            "timeout",
            "unavailable",
            "database is locked",
            "database locked",
            "cannot",
            "unable",
        ]
        .iter()
        .any(|reason| lower.contains(reason));
        let error = if labeled.is_some() || ordinary {
            diagnostic_error_text(text)
        } else {
            "[redacted]".to_string()
        };
        (error, None)
    };
    if let Some(start) = error.char_indices().find_map(|(start, ch)| {
        (ch == '{' || (ch == '[' && !error[start..].starts_with("[redacted]"))).then_some(start)
    }) {
        error.truncate(start);
        error.push_str("[redacted]");
    }
    tracing::warn!(event = "acp_stderr_diagnostic", command = %command, pid, logger, stderr = %error, rpc_error_code = code, "Agent subprocess stderr");
}

fn spawn_stderr_logger(command: String, pid: Option<u32>, mut stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut buffer = [0; 8192];
        let mut bytes = 0u64;
        let mut line = Vec::with_capacity(4096);
        let mut discard_line = false;
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) => {
                    if !discard_line {
                        log_stderr_diagnostic(&command, pid, &line);
                    }
                    tracing::debug!(event = "acp_stderr_eof", command = %command, pid, bytes, "ACP stderr closed");
                    break;
                }
                Ok(count) => {
                    if bytes == 0 {
                        tracing::debug!(event = "acp_stderr_first_read", command = %command, pid, bytes = count, "ACP stderr first read");
                    }
                    bytes = bytes.saturating_add(count as u64);
                    for byte in &buffer[..count] {
                        if *byte == b'\n' {
                            if !discard_line {
                                log_stderr_diagnostic(&command, pid, &line);
                            }
                            line.clear();
                            discard_line = false;
                        } else if !discard_line {
                            if line.len() < 4096 {
                                line.push(*byte);
                            } else {
                                line.clear();
                                discard_line = true;
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(event = "acp_stderr_read_failed", command = %command, pid, bytes, error = %error, error_code = ?error.kind(), "ACP stderr read failed");
                    break;
                }
            }
        }
    }.in_current_span().with_current_subscriber());
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&mut self, message: &str) -> anyhow::Result<()> {
        let started = Instant::now();
        let bytes = message.len();
        let mut completed_bytes = 0usize;
        for (boundary, payload) in [
            ("write_all", message.as_bytes()),
            ("newline", b"\n".as_slice()),
        ] {
            tracing::debug!(
                event = "acp_stdio_write_started",
                command = %self.command,
                pid = self.pid,
                boundary,
                bytes = payload.len(),
                completed_bytes,
                "ACP stdio write started"
            );
            if let Err(error) = self.writer.write_all(payload).await {
                tracing::debug!(event = "acp_stdio_write_failed", command = %self.command, pid = self.pid, boundary, bytes, completed_bytes, elapsed_ms = started.elapsed().as_millis() as u64, error = %error, error_code = ?error.kind(), "ACP stdio write failed");
                tracing::warn!(event = "acp_stdio_send_failed", command = %self.command, pid = self.pid, boundary, error = %error, error_code = ?error.kind(), "ACP stdio send failed");
                return Err(error.into());
            }
            completed_bytes += payload.len();
            tracing::debug!(
                event = "acp_stdio_write_complete",
                command = %self.command,
                pid = self.pid,
                boundary,
                bytes,
                completed_bytes,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "ACP stdio write complete"
            );
        }
        tracing::debug!(
            event = "acp_stdio_flush_started",
            command = %self.command,
            pid = self.pid,
            bytes,
            completed_bytes,
            "ACP stdio flush started"
        );
        if let Err(error) = self.writer.flush().await {
            tracing::debug!(event = "acp_stdio_flush_failed", command = %self.command, pid = self.pid, bytes, completed_bytes, elapsed_ms = started.elapsed().as_millis() as u64, error = %error, error_code = ?error.kind(), "ACP stdio flush failed");
            tracing::warn!(event = "acp_stdio_send_failed", command = %self.command, pid = self.pid, boundary = "flush", error = %error, error_code = ?error.kind(), "ACP stdio send failed");
            return Err(error.into());
        }
        tracing::debug!(
            event = "acp_stdio_flush_complete",
            command = %self.command,
            pid = self.pid,
            bytes,
            completed_bytes,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "ACP stdio flush complete"
        );
        Ok(())
    }

    async fn recv(&mut self) -> anyhow::Result<Option<String>> {
        loop {
            let started = *self.frame_started.get_or_insert_with(Instant::now);
            let available = match self.reader.fill_buf().await {
                Ok(available) => available,
                Err(error) => {
                    tracing::warn!(event = "acp_stdout_read_failed", command = %self.command, pid = self.pid, bytes = self.read_buffer.len(), stdout_bytes = self.stdout_bytes, frames = self.frames, elapsed_ms = started.elapsed().as_millis() as u64, error = %error, error_code = ?error.kind(), "ACP stdout read failed");
                    return Err(error.into());
                }
            };
            let eof = available.is_empty();
            let newline = available.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(available.len(), |index| index + 1);
            if self.stdout_bytes == 0 && count > 0 {
                tracing::debug!(
                    event = "acp_stdout_first_read",
                    command = %self.command,
                    pid = self.pid,
                    bytes = count,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "ACP stdout first read"
                );
            }
            self.read_buffer.extend_from_slice(&available[..count]);
            self.stdout_bytes = self.stdout_bytes.saturating_add(count as u64);
            self.reader.consume(count);
            if eof {
                tracing::debug!(
                    event = "acp_stdout_eof",
                    command = %self.command,
                    pid = self.pid,
                    bytes = self.read_buffer.len(),
                    stdout_bytes = self.stdout_bytes,
                    frames = self.frames,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "ACP stdout closed"
                );
                if self.read_buffer.is_empty() {
                    self.frame_started = None;
                    return Ok(None);
                }
            }
            if newline.is_none() && !eof {
                continue;
            }
            self.frames = self.frames.saturating_add(1);
            self.frame_started = None;
            let bytes = self.read_buffer.len();
            let frame = std::mem::take(&mut self.read_buffer);
            let mut line = match String::from_utf8(frame) {
                Ok(line) => line,
                Err(_) => {
                    tracing::debug!(
                        event = "acp_stdout_frame_failed",
                        command = %self.command,
                        pid = self.pid,
                        boundary = if eof { "eof" } else { "newline" },
                        bytes,
                        stdout_bytes = self.stdout_bytes,
                        frames = self.frames,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        error_code = "invalid_utf8",
                        "ACP stdout frame failed"
                    );
                    tracing::warn!(event = "acp_stdout_read_failed", command = %self.command, pid = self.pid, error = "ACP frame is not valid UTF-8", error_code = "invalid_utf8", "ACP stdout read failed");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "ACP stdout frame is invalid UTF-8",
                    )
                    .into());
                }
            };
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            tracing::debug!(
                event = "acp_stdout_frame_complete",
                command = %self.command,
                pid = self.pid,
                boundary = if eof { "eof" } else { "newline" },
                bytes,
                stdout_bytes = self.stdout_bytes,
                frames = self.frames,
                empty = line.trim().is_empty(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "ACP stdout frame complete"
            );
            if !line.trim().is_empty() {
                return Ok(Some(line));
            }
        }
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        let status = self.child.try_wait().ok().flatten();
        let tree_terminated = self.terminate_tree();
        tracing::debug!(
            command = %self.command,
            pid = self.pid,
            status = ?status,
            tree_terminated,
            "Closing agent subprocess"
        );
        if status.is_none()
            && let Err(error) = self.child.kill().await
        {
            tracing::warn!(command = %self.command, pid = self.pid, error = %error, error_code = "kill_failed", "Agent subprocess kill failed");
        }
        Ok(())
    }

    async fn force_terminate(&mut self) -> anyhow::Result<()> {
        let status = self.child.try_wait()?;
        if status.is_none() {
            let tree_terminated = self.terminate_tree();
            tracing::warn!(
                command = %self.command,
                pid = self.pid,
                tree_terminated,
                "Force terminating agent subprocess"
            );
            self.child.kill().await?;
            let _ = self.child.wait().await?;
        } else {
            self.terminate_tree();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{TransportConfig, ZellijConfig};
    use tokio::process::Command;

    const APPROVED_ENV: &str = "COWBOY_STDIO_TEST_APPROVED";
    const UNAPPROVED_ENV: &str = "COWBOY_STDIO_TEST_UNAPPROVED";

    async fn run_environment_probe(mode: &str) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::stdio::tests::stdio_environment_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("COWBOY_STDIO_TEST_MODE", mode)
            .env(APPROVED_ENV, "ambient-approved")
            .env(UNAPPROVED_ENV, "ambient-unapproved")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "stdio environment probe failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn sanitized_environment_forwards_only_allowlisted_variables() {
        run_environment_probe("sanitized").await;
    }

    #[tokio::test]
    async fn explicit_environment_overrides_allowlisted_ambient_value() {
        run_environment_probe("override").await;
    }

    #[tokio::test]
    async fn sanitized_environment_is_preserved_when_resume_argument_restarts_stdio_transport() {
        run_environment_probe("resume").await;
    }

    #[tokio::test]
    #[ignore]
    async fn stdio_environment_probe() {
        let mode = std::env::var("COWBOY_STDIO_TEST_MODE").unwrap();
        let explicit = (mode == "override")
            .then(|| (APPROVED_ENV.to_string(), "explicit-approved".to_string()))
            .into_iter()
            .collect();
        let config = StdioConfig {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                format!(
                    "printf '%s\\n' \"approved=${{{APPROVED_ENV}-missing}}\" \
                     \"unapproved=${{{UNAPPROVED_ENV}-missing}}\" \"args=$*\"",
                ),
                "environment-probe".to_string(),
            ],
            clear_env: true,
            allowed_env: vec![
                APPROVED_ENV.to_string(),
                "COWBOY_STDIO_TEST_MISSING".to_string(),
            ],
            env: explicit,
            startup_readiness: None,
        };
        let additional_args = if mode == "resume" {
            vec!["--resume=session-123"]
        } else {
            vec![]
        };
        let mut transport = StdioTransport::connect(&config, &additional_args)
            .await
            .unwrap();
        let approved = transport.recv().await.unwrap().unwrap();
        let unapproved = transport.recv().await.unwrap().unwrap();
        let args = transport.recv().await.unwrap().unwrap();

        assert_eq!(
            approved,
            if mode == "override" {
                "approved=explicit-approved"
            } else {
                "approved=ambient-approved"
            }
        );
        assert_eq!(unapproved, "unapproved=missing");
        assert_eq!(
            args,
            if mode == "resume" {
                "args=--resume=session-123"
            } else {
                "args="
            }
        );
    }

    #[tokio::test]
    async fn test_connect_echo() {
        let config = StdioConfig {
            command: "cat".to_string(),
            args: vec![],
            clear_env: false,
            allowed_env: vec![],
            env: vec![],
            startup_readiness: None,
        };

        let mut transport = StdioTransport::connect(&config, &[]).await.unwrap();

        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#;
        transport.send(msg).await.unwrap();

        let received = transport.recv().await.unwrap();
        assert_eq!(received, Some(msg.to_string()));

        transport.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_connect_appends_additional_args() {
        let config = StdioConfig {
            command: "echo".to_string(),
            args: vec!["configured".to_string()],
            clear_env: false,
            allowed_env: vec![],
            env: vec![],
            startup_readiness: None,
        };
        let additional_args = vec!["extra"];

        let mut transport = StdioTransport::connect(&config, &additional_args)
            .await
            .unwrap();

        let first = transport.recv().await.unwrap();
        assert_eq!(first, Some("configured extra".to_string()));
    }

    #[tokio::test]
    async fn test_connect_invalid_command() {
        let config = StdioConfig {
            command: "nonexistent-binary-12345".to_string(),
            args: vec![],
            clear_env: false,
            allowed_env: vec![],
            env: vec![],
            startup_readiness: None,
        };

        let result = StdioTransport::connect(&config, &[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_recv_eof() {
        let config = StdioConfig {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            clear_env: false,
            allowed_env: vec![],
            env: vec![],
            startup_readiness: None,
        };

        let mut transport = StdioTransport::connect(&config, &[]).await.unwrap();

        let first = transport.recv().await.unwrap();
        assert_eq!(first, Some("hello".to_string()));

        let eof = transport.recv().await.unwrap();
        assert_eq!(eof, None);
    }

    #[tokio::test]
    async fn test_wrong_config_type() {
        // StdioTransport should not be constructed from ZellijConfig
        // This is now a compile-time guarantee since connect takes &StdioConfig.
        // We keep this test to verify TransportConfig enum still round-trips.
        let config = TransportConfig::Zellij(ZellijConfig {
            remote_url: None,
            token: None,
            session: "test".to_string(),
            command: "agent".to_string(),
            args: vec![],
            env: vec![],
        });
        assert!(matches!(config, TransportConfig::Zellij(_)));
    }

    #[tokio::test]
    async fn force_terminate_stops_stdio_child_by_pid() {
        let config = StdioConfig {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "sleep 60".to_string()],
            clear_env: false,
            allowed_env: vec![],
            env: vec![],
            startup_readiness: None,
        };
        let mut transport = StdioTransport::connect(&config, &[]).await.unwrap();
        let pid = transport.pid.expect("child pid");

        transport.force_terminate().await.unwrap();

        assert!(transport.child.try_wait().unwrap().is_some());
        assert_eq!(transport.pid, Some(pid));
    }
}
