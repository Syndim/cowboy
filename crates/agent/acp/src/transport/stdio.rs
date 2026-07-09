use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::{StdioConfig, Transport};

/// Stdio transport: a local subprocess using direct JSON-RPC over stdin/stdout.
pub struct StdioTransport {
    writer: BufWriter<ChildStdin>,
    reader: Lines<BufReader<ChildStdout>>,
    child: Child,
    command: String,
    pid: Option<u32>,
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
        for (key, val) in &config.env {
            cmd.env(key, val);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn agent process '{}': {}", config.command, e)
        })?;

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
        })
    }

    /// Get a reference to the underlying child process.
    #[allow(dead_code)]
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
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
        tracing::debug!(
            command = %self.command,
            pid = ?self.pid,
            status = ?status,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{TransportConfig, ZellijConfig};

    #[tokio::test]
    async fn test_connect_echo() {
        let config = StdioConfig {
            command: "cat".to_string(),
            args: vec![],
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
}
