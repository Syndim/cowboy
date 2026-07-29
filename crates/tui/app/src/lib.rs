//! Workflow-first Cowboy terminal application.
//!
//! This crate owns config loading, runtime dispatch, logging setup, terminal
//! rendering, and TUI state. Command argument grammar lives in
//! `cowboy-command-parser`; workflow runtime logic lives in
//! `cowboy-workflow-engine`.

pub mod app;
pub mod config;
pub mod process_exit;
mod export;
pub mod resolution;
pub mod run_summary;

pub use app::run_tui;
pub use config::{AppConfig, default_config_path, load_config};
pub use process_exit::{DEFAULT_PROCESS_SHUTDOWN_TIMEOUT, run_with_bounded_shutdown};
pub use export::{ExportResult, export_run};
