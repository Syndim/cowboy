use std::future::{Future, pending};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use serde_json::Value;

use crate::{AgentInfo, Event, ModelInfo, PromptContent, StopReason};

/// Awaitable signal that cancels only the currently active prompt turn.
///
/// Backend adapters intentionally receive no workflow ids, prompt-window ids,
/// or durable prompt sequences through this provider-neutral input.
pub struct PromptTurnCancellation {
    signal: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    fired: bool,
}

impl PromptTurnCancellation {
    /// Create a cancellation input that never fires.
    pub fn disabled() -> Self {
        Self::from_future(pending())
    }

    /// Create a cancellation input from a one-shot awaitable signal.
    pub fn from_future(signal: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            signal: Box::pin(signal),
            fired: false,
        }
    }

    /// Wait until the active prompt turn should be cancelled.
    pub async fn cancelled(&mut self) {
        if self.fired {
            return;
        }
        self.signal.as_mut().await;
        self.fired = true;
    }

    /// Poll once without waiting, preserving the signal for a later await.
    pub fn try_cancelled(&mut self) -> bool {
        if self.fired {
            return true;
        }
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        if matches!(self.signal.as_mut().poll(&mut context), Poll::Ready(())) {
            self.fired = true;
        }
        self.fired
    }
}

impl Default for PromptTurnCancellation {
    fn default() -> Self {
        Self::disabled()
    }
}

impl std::fmt::Debug for PromptTurnCancellation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PromptTurnCancellation")
            .finish_non_exhaustive()
    }
}

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

    /// Create a new agent session, optionally requesting a specific model.
    async fn new_session(
        &mut self,
        cwd: &str,
        mcp_servers: &[Value],
        model: Option<&ModelInfo>,
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
        cancellation: PromptTurnCancellation,
        event_handler: &mut (dyn FnMut(Event) + Send),
    ) -> anyhow::Result<StopReason>;

    /// Close the client and release transport resources.
    async fn close(&mut self) -> anyhow::Result<()>;
}
