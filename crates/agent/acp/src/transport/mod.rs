pub mod stdio;
pub mod zellij;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// ACP transport layer, abstracting the underlying I/O channel.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send one JSON-RPC message line.
    async fn send(&mut self, message: &str) -> anyhow::Result<()>;
    /// Receive the next message line, blocking until one is available.
    async fn recv(&mut self) -> anyhow::Result<Option<String>>;
    /// Close the connection.
    async fn close(&mut self) -> anyhow::Result<()>;
}

/// Stdio transport config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StdioConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// Zellij transport config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZellijConfig {
    pub remote_url: Option<String>,
    pub token: Option<String>,
    pub session: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// Transport configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TransportConfig {
    /// Local subprocess with direct JSON-RPC over stdio.
    #[serde(rename = "stdio")]
    Stdio(StdioConfig),
    /// Remote agent via Zellij, using ACP over Zellij Web Remote Attach.
    #[serde(rename = "zellij")]
    Zellij(ZellijConfig),
    /// Mock transport for testing
    #[cfg(test)]
    #[serde(rename = "mock")]
    Mock(MockConfig),
}

/// Mock transport config — pre-recorded response lines for testing
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockConfig {
    /// Pre-configured JSON-RPC response lines (consumed FIFO)
    pub responses: Vec<String>,
}

/// Mock transport that plays back pre-configured responses.
///
/// Responses are consumed FIFO. When exhausted, `recv()` returns `None` (EOF).
#[cfg(test)]
pub struct MockTransport {
    incoming: std::collections::VecDeque<String>,
}

#[cfg(test)]
impl MockTransport {
    pub fn new(config: &MockConfig) -> Self {
        Self {
            incoming: config.responses.iter().cloned().collect(),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl Transport for MockTransport {
    async fn send(&mut self, _message: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recv(&mut self) -> anyhow::Result<Option<String>> {
        Ok(self.incoming.pop_front())
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
