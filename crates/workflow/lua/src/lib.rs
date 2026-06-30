//! Sandboxed Lua workflow loader.
//!
//! This crate evaluates workflow definition Lua, converts registered roles and
//! steps into `cowboy-workflow-core` definitions, and converts returned action
//! tables into core `StepAction` values.

mod api;
mod convert;
mod error;
mod imports;
mod loader;
mod runtime;
mod sandbox;

pub use error::{Error, Result};
pub use imports::SourceResolver;
pub use loader::{Loader, compile_snapshot, load};
pub use runtime::{RunStepResult, run_step};
