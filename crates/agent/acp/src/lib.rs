//! ACP implementation of Cowboy's agent-client interface.

mod agent_processes;
pub mod backend;
pub mod client;
pub mod messages;
mod process_tree;
#[cfg(test)]
mod test_util;
pub mod transport;

pub use agent_processes::terminate_all_agent_processes;
pub use backend::BackendPreset;
pub use client::{AgentWatchdogOptions, Client};
pub use transport::TransportConfig;
