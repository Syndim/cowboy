//! Workflow-first Cowboy terminal application.
//!
//! This crate owns config loading, runtime dispatch, logging setup, terminal
//! rendering, and TUI state. Command argument grammar lives in
//! `cowboy-command-parser`; workflow runtime logic lives in
//! `cowboy-workflow-engine`.

pub mod app;
pub mod config;
pub mod run_summary;

pub use app::run_tui;
pub use config::{AppConfig, default_config_path, load_config};
