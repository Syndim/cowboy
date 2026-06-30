//! Core workflow domain model and traits.
//!
//! This crate intentionally contains no Lua runtime, storage backend, TUI, or
//! agent protocol implementation. It owns the serializable workflow data model,
//! validation rules, and small traits other crates implement.

pub mod action;
pub mod definition;
pub mod engine;
pub mod error;
pub mod ids;
pub mod state;
pub mod summary;
pub mod traits;

pub use action::*;
pub use definition::*;
pub use engine::*;
pub use error::*;
pub use ids::*;
pub use state::*;
pub use summary::*;
pub use traits::*;
