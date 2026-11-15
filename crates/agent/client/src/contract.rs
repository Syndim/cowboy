//! Agent launch contract shared by Cowboy and its agent backends.
//!
//! These types are backend-neutral: the workflow engine fills an
//! [`launch::AgentLaunchSpec`] from configuration and hands it to a registered
//! [`launch::BackendRegistry`], which interprets it for its own protocol. The
//! engine never learns which protocol a backend speaks.

mod access;
mod diagnostics;
mod launch;
mod startup;
mod watchdog;

pub use access::AgentAccess;
pub use diagnostics::diagnostic_error_text;
pub use launch::{AgentLaunchSpec, BackendConnector, BackendRegistry, ResolvedBackend};
pub use startup::StartupReadiness;
pub use watchdog::AgentWatchdogOptions;
