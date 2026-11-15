//! pi coding-agent RPC backend for Cowboy.
//!
//! Implements [`cowboy_agent_client::Client`] and [`cowboy_agent_client::BackendConnector`]
//! for pi's native JSONL RPC protocol (`pi --mode rpc`). Registered in the
//! composition root's backend registry under the `"pi-rpc"` kind.

mod client;
mod connector;
pub mod process;

pub use client::PiRpcClient;
pub use connector::{PiRpcBackendConnector, pi_connector};

#[doc(hidden)]
pub mod diagnostics {
    pub use crate::client::{Command, io_loop};
}
