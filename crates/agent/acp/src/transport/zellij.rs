//! ZellijTransport — 通过 Zellij 终端多路复用器与远程 Agent 通信
//!
//! 通信流程：
//! 1. attach 到 Zellij session（本地或远程）
//! 2. 创建 pane 运行 Agent 命令
//! 3. 发送：zellij action write-chars → Agent stdin
//! 4. 接收：zellij action dump-screen → 解析 Agent stdout 中的 JSON-RPC

use std::collections::VecDeque;

use async_trait::async_trait;

use super::{Transport, ZellijConfig};

impl std::fmt::Debug for ZellijTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZellijTransport")
            .field("session", &self.session)
            .field("pane_id", &self.pane_id)
            .finish()
    }
}

/// ZellijTransport — Zellij pane I/O 传输
pub struct ZellijTransport {
    session: String,
    pane_id: String,
    zellij_command: String,
    /// 已接收但未消费的 JSON-RPC 消息缓冲
    message_buffer: VecDeque<String>,
    /// 上次 dump-screen 已处理的行数（避免重复解析）
    last_seen_line: usize,
}

impl ZellijTransport {
    /// Connect to a Zellij session and spawn Agent in a new pane.
    /// If `resume_session_id` is provided, appends `--resume=<id>` to the agent args.
    pub async fn connect(
        config: &ZellijConfig,
        resume_session_id: Option<&str>,
    ) -> anyhow::Result<Self> {
        Self::connect_with_command(config, resume_session_id, "zellij").await
    }

    async fn connect_with_command(
        config: &ZellijConfig,
        resume_session_id: Option<&str>,
        zellij_command: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let zellij_command = zellij_command.into();

        // 1. Attach to session (create if needed)
        if let Some(url) = &config.remote_url {
            let mut attach_cmd = tokio::process::Command::new(&zellij_command);
            attach_cmd.args(["attach", &format!("{url}/{}", config.session)]);
            if let Some(t) = &config.token {
                attach_cmd.args(["--token", t]);
            }
            attach_cmd.arg("--create-background");
            let status = attach_cmd.status().await?;
            if !status.success() {
                anyhow::bail!("Failed to attach to remote Zellij at {url}");
            }
            tracing::info!(
                url,
                session = config.session,
                "Attached to remote Zellij session"
            );
        } else {
            // Local: ensure session exists
            let status = tokio::process::Command::new(&zellij_command)
                .args(["attach", "--create-background", &config.session])
                .status()
                .await?;
            if !status.success() {
                anyhow::bail!("Failed to create Zellij session '{}'", config.session);
            }
        }

        // 2. Create pane running the Agent command
        let mut agent_cmd = vec![config.command.clone()];
        agent_cmd.extend(config.args.iter().cloned());
        if let Some(session_id) = resume_session_id {
            agent_cmd.push(format!("--resume={session_id}"));
        }

        let pane_output = tokio::process::Command::new(&zellij_command)
            .args([
                "--session",
                &config.session,
                "action",
                "new-pane",
                "--name",
                "acp-agent",
                "--close-on-exit",
                "--",
            ])
            .args(&agent_cmd)
            .output()
            .await?;

        if !pane_output.status.success() {
            let stderr = String::from_utf8_lossy(&pane_output.stderr);
            anyhow::bail!("Failed to create Zellij pane: {stderr}");
        }

        // Parse pane ID from output
        let pane_id = String::from_utf8_lossy(&pane_output.stdout)
            .trim()
            .to_string();

        tracing::info!(
            session = config.session,
            pane_id,
            "Zellij pane created for agent"
        );

        Ok(Self {
            session: config.session.clone(),
            pane_id,
            zellij_command,
            message_buffer: VecDeque::new(),
            last_seen_line: 0,
        })
    }

    /// Dump the pane screen and extract new JSON-RPC lines
    async fn poll_screen(&mut self) -> anyhow::Result<()> {
        let output = tokio::process::Command::new(&self.zellij_command)
            .args([
                "--session",
                &self.session,
                "action",
                "dump-screen",
                "--pane-id",
                &self.pane_id,
                "/dev/stdout",
            ])
            .output()
            .await?;

        let screen = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = screen.lines().collect();

        // Only process new lines since last poll
        let new_lines = if self.last_seen_line < lines.len() {
            &lines[self.last_seen_line..]
        } else {
            &[]
        };

        for line in new_lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Check if it's valid JSON-RPC
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if value.get("jsonrpc").is_some() {
                    self.message_buffer.push_back(trimmed.to_string());
                }
            }
        }

        self.last_seen_line = lines.len();
        Ok(())
    }

    /// Get the pane ID (for debugging/logging)
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    /// Get the session name
    pub fn session(&self) -> &str {
        &self.session
    }
}

#[async_trait]
impl Transport for ZellijTransport {
    async fn send(&mut self, message: &str) -> anyhow::Result<()> {
        let status = tokio::process::Command::new(&self.zellij_command)
            .args([
                "--session",
                &self.session,
                "action",
                "write-chars",
                "--pane-id",
                &self.pane_id,
                &format!("{message}\n"),
            ])
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!(
                "Failed to write to Zellij pane {} in session {}",
                self.pane_id,
                self.session
            );
        }
        Ok(())
    }

    async fn recv(&mut self) -> anyhow::Result<Option<String>> {
        // Return buffered message if available
        if let Some(msg) = self.message_buffer.pop_front() {
            return Ok(Some(msg));
        }

        // Poll screen with backoff until we get a message
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(300));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.poll_screen().await?;
                     if let Some(msg) = self.message_buffer.pop_front() {
                        return Ok(Some(msg));
                    }
                }
                _ = &mut timeout => {
                    return Ok(None); // Timeout
                }
            }
        }
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        // Close the pane
        let _ = tokio::process::Command::new(&self.zellij_command)
            .args([
                "--session",
                &self.session,
                "action",
                "close-pane",
                "--pane-id",
                &self.pane_id,
            ])
            .status()
            .await;

        tracing::info!(
            session = self.session,
            pane_id = self.pane_id,
            "Zellij pane closed"
        );
        Ok(())
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportConfig;

    #[test]
    fn test_zellij_transport_config_serde() {
        let config = TransportConfig::Zellij(ZellijConfig {
            remote_url: Some("https://remote.example.com".into()),
            token: Some("secret".into()),
            session: "my-session".into(),
            command: "claude".into(),
            args: vec!["--model".into(), "opus".into()],
            env: vec![],
        });
        let json = serde_json::to_string(&config).unwrap();
        let parsed: TransportConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, TransportConfig::Zellij(_)));
    }

    #[test]
    fn test_zellij_config_local() {
        let config = TransportConfig::Zellij(ZellijConfig {
            remote_url: None,
            token: None,
            session: "local-session".into(),
            command: "claude".into(),
            args: vec![],
            env: vec![],
        });
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("local-session"));
    }

    #[test]
    fn test_message_buffer_is_fifo() {
        let mut transport = ZellijTransport {
            session: "test-session".into(),
            pane_id: "1".into(),
            zellij_command: "zellij".into(),
            message_buffer: VecDeque::new(),
            last_seen_line: 0,
        };

        transport.message_buffer.push_back("first".into());
        transport.message_buffer.push_back("second".into());

        assert_eq!(
            transport.message_buffer.pop_front().as_deref(),
            Some("first")
        );
        assert_eq!(
            transport.message_buffer.pop_front().as_deref(),
            Some("second")
        );
    }

    #[tokio::test]
    async fn test_zellij_session_lifecycle() {
        let dir = tempfile::TempDir::new().unwrap();
        let fake_zellij = dir.path().join("zellij");
        let command_log = dir.path().join("commands.log");
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case " $* " in
  *" action new-pane "*)
    printf 'pane-1\n'
    ;;
esac
exit 0
"#,
            command_log.display()
        );
        std::fs::write(&fake_zellij, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&fake_zellij).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_zellij, permissions).unwrap();
        }

        let session_name = "test-zellij-session".to_string();
        let config = ZellijConfig {
            remote_url: None,
            token: None,
            session: session_name.clone(),
            command: "cat".into(),
            args: vec![],
            env: vec![],
        };

        let mut transport =
            ZellijTransport::connect_with_command(&config, None, fake_zellij.to_string_lossy())
                .await
                .unwrap();
        assert_eq!(transport.session(), &session_name);
        assert_eq!(transport.pane_id(), "pane-1");

        transport.close().await.unwrap();

        let commands = std::fs::read_to_string(command_log).unwrap();
        assert!(commands.contains("attach --create-background test-zellij-session"));
        assert!(commands.contains("--session test-zellij-session action new-pane"));
        assert!(
            commands.contains("--session test-zellij-session action close-pane --pane-id pane-1")
        );
    }
}
