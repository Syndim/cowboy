//! Workflow-first Cowboy terminal application.
//!
//! This crate contains only CLI/configuration and terminal rendering code.
//! Workflow runtime logic lives in `cowboy-workflow-engine`.

pub mod app;
pub mod config;

pub use app::run_tui;
pub use config::{AppConfig, default_config_path, load_config};
