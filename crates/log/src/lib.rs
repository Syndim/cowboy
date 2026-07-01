//! File-based diagnostic logging for Cowboy binaries.
//!
//! Library crates emit `tracing` events (`info!`/`debug!`/`warn!`/`error!`);
//! each binary calls [`init_file_logging`] once at startup to route those
//! events to a log file. Verbosity is controlled by an environment variable, so
//! getting more detail is just a matter of raising the level — no rebuild.

use std::any::Any;
use std::io;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::Once;

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

/// Install a process-wide panic hook that mirrors panic details into `tracing`.
///
/// This is intentionally separate from [`init_file_logging`]: binaries should
/// initialize logging first, then install the hook so panics include the same
/// file sink as normal diagnostics. The previous panic hook still runs, so
/// stderr keeps Rust's standard panic report.
pub fn install_panic_hook() {
    static INSTALL: Once = Once::new();

    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let payload = panic_payload(info);
            let location = info
                .location()
                .map(|location| format!("{}:{}", location.file(), location.line()))
                .unwrap_or_else(|| "<unknown>".to_string());
            let thread = std::thread::current();
            let thread_name = thread.name().unwrap_or("<unnamed>");

            tracing::error!(
                panic_payload = %payload,
                panic_location = %location,
                panic_thread = %thread_name,
                "process panic"
            );

            previous(info);
        }));
    });
}

fn panic_payload(info: &PanicHookInfo<'_>) -> String {
    panic_payload_from_any(info.payload())
}

fn panic_payload_from_any(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
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

    #[test]
    fn extracts_string_panic_payloads() {
        assert_eq!(panic_payload_from_any(&"boom"), "boom");
        assert_eq!(
            panic_payload_from_any(&"owned boom".to_string()),
            "owned boom"
        );
    }
}
