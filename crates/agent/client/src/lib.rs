//! Agent client interface and normalized runtime types.
//!
//! Cowboy talks to coding agents through this crate's `Client` trait.
//! Concrete backends, such as ACP or future SDK-backed clients, live in sibling
//! crates and map provider-specific events into the normalized types here.

pub mod traits;
pub mod types;

pub use traits::Client;
pub use types::{AgentInfo, Event, ModelInfo, PromptContent, StopReason};
