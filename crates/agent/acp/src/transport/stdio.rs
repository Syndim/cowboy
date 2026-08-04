use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::agent_processes::{self, RegistrationId};
use crate::process_tree::ProcessTreeScope;

use super::{StdioConfig, Transport};

/// Stdio transport: a local subprocess using direct JSON-RPC over stdin/stdout.
pub struct StdioTransport {
    writer: BufWriter<ChildStdin>,
    reader: Lines<BufReader<ChildStdout>>,
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

        let pid = child.id();
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_logger(config.command.clone(), pid, stderr);
        }

        let writer = BufWriter::new(stdin);
        let reader = BufReader::new(stdout).lines();

        let env_keys = config.env.iter().map(|(key, _)| key).collect::<Vec<_>>();
        tracing::debug!(
            command = %config.command,
            args = ?config.args,
            additional_args = ?additional_args,
            clear_env = config.clear_env,
            allowed_env = ?config.allowed_env,
            env_keys = ?env_keys,
            pid = ?pid,
            "Agent subprocess spawned"
        );

        Ok(Self {
            writer,
            reader,
            child,
            command: config.command.clone(),
            pid,
            tree,
            registration: Some(registration),
        })
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
            Err(err) => {
                tracing::warn!(
                    command = %self.command,
                    pid = ?self.pid,
                    error = %err,
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

fn spawn_stderr_logger(command: String, pid: Option<u32>, stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) if line.trim().is_empty() => {}
                Ok(Some(line)) => {
                    tracing::warn!(
                        command = %command,
                        pid = ?pid,
                        stderr = %line,
                        "Agent subprocess stderr"
                    );
                }
                Ok(None) => {
                    tracing::debug!(command = %command, pid = ?pid, "Agent subprocess stderr closed");
                    break;
                }
                Err(err) => {
                    tracing::warn!(
                        command = %command,
                        pid = ?pid,
                        error = %err,
                        "Agent subprocess stderr read failed"
                    );
                    break;
                }
            }
        }
    });
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&mut self, message: &str) -> anyhow::Result<()> {
        self.writer.write_all(message.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> anyhow::Result<Option<String>> {
        loop {
            match self.reader.next_line().await {
                Ok(Some(line)) if line.trim().is_empty() => {
                    tracing::trace!(command = %self.command, pid = ?self.pid, "Agent subprocess stdout empty line skipped");
                }
                Ok(Some(line)) => {
                    tracing::trace!(
                        command = %self.command,
                        pid = ?self.pid,
                        bytes = line.len(),
                        "Agent subprocess stdout line received"
                    );
                    return Ok(Some(line));
                }
                Ok(None) => {
                    let status = self.child.try_wait().ok().flatten();
                    tracing::debug!(
                        command = %self.command,
                        pid = ?self.pid,
                        status = ?status,
                        "Agent subprocess stdout closed"
                    );
                    return Ok(None);
                }
                Err(err) => {
                    tracing::warn!(
                        command = %self.command,
                        pid = ?self.pid,
                        error = %err,
                        "Agent subprocess stdout read failed"
                    );
                    return Err(err.into());
                }
            }
        }
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        let status = self.child.try_wait().ok().flatten();
        let tree_terminated = self.terminate_tree();
        tracing::debug!(
            command = %self.command,
            pid = ?self.pid,
            status = ?status,
            tree_terminated,
            "Closing agent subprocess"
        );
        if status.is_none()
            && let Err(err) = self.child.kill().await
        {
            tracing::warn!(
                command = %self.command,
                pid = ?self.pid,
                error = %err,
                "Agent subprocess kill failed"
            );
        }
        Ok(())
    }

    async fn force_terminate(&mut self) -> anyhow::Result<()> {
        let status = self.child.try_wait()?;
        if status.is_none() {
            let tree_terminated = self.terminate_tree();
            tracing::warn!(
                command = %self.command,
                pid = ?self.pid,
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
        };
        let mut transport = StdioTransport::connect(&config, &[]).await.unwrap();
        let pid = transport.pid.expect("child pid");

        transport.force_terminate().await.unwrap();

        assert!(transport.child.try_wait().unwrap().is_some());
        assert_eq!(transport.pid, Some(pid));
    }
}
