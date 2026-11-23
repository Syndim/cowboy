//! ACP implementation of Cowboy's agent-client interface.
//!
//! ACP is one of Cowboy's pluggable agent backends: this crate registers an
//! [`connector::AcpBackendConnector`] with the shared [`cowboy_agent_client::BackendRegistry`]
//! so the workflow engine can launch ACP agents without knowing the protocol.

mod agent_processes;
pub mod backend;
pub mod client;
pub mod connector;
pub mod messages;
mod process_tree;
#[cfg(test)]
mod test_util;
pub mod transport;

pub use agent_processes::terminate_all_agent_processes;
pub use backend::BackendPreset;
pub use client::Client;
pub use connector::{AcpBackendConnector, ClaudeCodeBackendConnector, acp_connector};
pub use cowboy_agent_client::{AgentAccess, AgentWatchdogOptions};
pub use transport::TransportConfig;
