//! Agent client interface and normalized runtime types.
//!
//! Cowboy talks to coding agents through this crate's `Client` trait.
//! Concrete backends, such as ACP or future SDK-backed clients, live in sibling
//! crates and map provider-specific events into the normalized types here.

pub mod contract;
pub mod traits;
pub mod types;

pub use contract::{
    AgentAccess, AgentLaunchSpec, AgentWatchdogOptions, BackendConnector, BackendRegistry,
    ResolvedBackend, StartupReadiness, diagnostic_error_text,
};
pub use traits::{Client, PromptTurnCancellation};
pub use types::{AgentInfo, AgentSessionDescriptor, Event, ModelInfo, PromptContent, StopReason};
