//! File-based diagnostic logging for Cowboy binaries.
//!
//! Library crates emit `tracing` events (`info!`/`debug!`/`warn!`/`error!`);
//! each binary calls [`init_file_logging`] once at startup to route those
//! events to a log file. Verbosity is controlled by an environment variable, so
//! getting more detail is just a matter of raising the level — no rebuild.

use std::io;
use std::path::{Path, PathBuf};

use tracing_appender::rolling;
use tracing_subscriber::EnvFilter;

/// Default filter for the final `cowboy` product binary.
///
/// Keep startup/runtime logs at `info` by default. Raise verbosity with
/// `COWBOY_LOG` or `RUST_LOG` when debugging ACP or agent communication, e.g.
/// `COWBOY_LOG="info,cowboy_agent_acp=debug,cowboy_workflow_agent=debug,cowboy_workflow_engine=debug"`.
pub const DEFAULT_DIRECTIVE: &str = "info";

/// Default filter for local test-app binaries such as `engine-cli`.
///
/// Test apps are diagnostic tools, so they default to `debug`; production-facing
/// binaries should use [`DEFAULT_DIRECTIVE`] instead.
pub const TEST_APP_DIRECTIVE: &str = "debug";

/// Route `tracing` diagnostics to `<dir>/<name>.log` (created if missing,
/// appended across runs).
///
/// The level filter is read from `COWBOY_LOG`, then `RUST_LOG`, then
/// `default_directive` (e.g. `"info"`). Both env vars accept full
/// `tracing` directives, so per-crate control works too, e.g.
/// `COWBOY_LOG="info,cowboy_agent_acp=trace"`.
///
/// Writes are synchronous: a log line reaches disk before the next statement
/// runs, so a crash still leaves the trace that led up to it. Idempotent — a
/// second call (or a subscriber installed elsewhere) is ignored.
///
/// Returns the resolved log file path.
pub fn init_file_logging(
    dir: impl AsRef<Path>,
    name: &str,
    default_directive: &str,
) -> io::Result<PathBuf> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let file_name = format!("{name}.log");
    let appender = rolling::never(dir, &file_name);

    let filter = std::env::var("COWBOY_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .map(EnvFilter::new)
        .unwrap_or_else(|| EnvFilter::new(default_directive));

    let _ = tracing_subscriber::fmt()
        .with_writer(appender)
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .try_init();

    Ok(dir.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_directives_match_binary_roles() {
        let _ = EnvFilter::new(DEFAULT_DIRECTIVE);
        let _ = EnvFilter::new(TEST_APP_DIRECTIVE);
        assert_eq!(DEFAULT_DIRECTIVE, "info");
        assert_eq!(TEST_APP_DIRECTIVE, "debug");
    }
}
