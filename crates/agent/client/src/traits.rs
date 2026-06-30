use async_trait::async_trait;
use serde_json::Value;

use crate::{AgentInfo, Event, ModelInfo, PromptContent, StopReason};

/// Common runtime interface between Cowboy and an agent backend.
///
/// Implementations may speak ACP, an SDK, HTTP, or another transport. This
/// trait deliberately excludes construction/initialization methods such as
/// `connect`; those belong to backend-specific factories because each backend
/// has different setup requirements.
#[async_trait]
pub trait Client: Send + Sync + std::fmt::Debug {
    /// Whether the underlying client transport/session handle is connected.
    fn is_connected(&self) -> bool;

    /// Provider-reported agent metadata, when available.
    fn agent_info(&self) -> Option<&AgentInfo>;

    /// Current backend session id, when one exists.
    fn session_id(&self) -> Option<&str>;

    /// Create a new agent session.
    async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: &ModelInfo,
    ) -> anyhow::Result<String>;

    /// Whether this backend can load an existing session by id.
    fn supports_load_session(&self) -> bool;

    /// Best-effort load of a previous agent session.
    async fn load_session(
        &mut self,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[Value],
    ) -> anyhow::Result<Vec<Event>>;

    /// Send one prompt to an existing session and stream normalized events.
    async fn prompt(
        &mut self,
        session_id: &str,
        prompt_content: Vec<PromptContent>,
        event_handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason>;

    /// Close the client and release transport resources.
    async fn close(&mut self) -> anyhow::Result<()>;
}
