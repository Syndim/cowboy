//! Workflow catalog loading and safe materialization helpers.
//!
//! This crate keeps built-in and filesystem catalog policy outside the TUI.

mod builtin;
mod error;
mod improvement;
mod loader;
mod source;

pub use builtin::{
    builtin_default_source_ref, builtin_default_workflow_source, builtin_workflow_sources,
};
pub use error::{Error, Result};
pub use improvement::{
    AppliedWorkflowImprovement, WorkflowSourceUpdate, apply_improvement, apply_update,
};
pub use loader::{CatalogRoot, CatalogRootKind, WorkflowCatalogLoader};
pub use source::{
    LoadedWorkflowSource, load_source_ref, materialize_source_ref, normalize_workflow_entry,
};
