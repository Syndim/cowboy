//! File-based diagnostic logging for Cowboy binaries.
//!
//! Library crates emit `tracing` events (`info!`/`debug!`/`warn!`/`error!`);
//! each binary calls [`init_file_logging`] once at startup to route those
//! events to a log file. Verbosity is controlled by an environment variable, so
//! getting more detail is just a matter of raising the level — no rebuild.

use std::any::Any;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Once;

use time::{Date, OffsetDateTime};
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

/// Route `tracing` diagnostics to `<dir>/<name>.<date>.<pid>.log`
/// (created if missing, appended across runs for the same process/date name).
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
    let file_name = log_file_name(name, OffsetDateTime::now_utc().date(), process::id());
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

fn log_file_name(name: &str, date: Date, pid: u32) -> String {
    format!("{name}.{date}.{pid}.log")
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
            log_panic(
                info.payload(),
                info.location().map(|location| location.line()),
            );

            previous(info);
        }));
    });
}

fn log_panic(payload: &(dyn Any + Send), line: Option<u32>) {
    tracing::error!(
        error_code = "process_panic",
        payload_bytes = panic_payload_from_any(payload).len(),
        line,
        "process panic"
    );
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
    fn log_file_name_includes_date_and_pid_in_order() {
        let date = Date::from_calendar_date(2026, time::Month::July, 3).unwrap();

        assert_eq!(
            log_file_name("cowboy", date, 4242),
            "cowboy.2026-07-03.4242.log"
        );
    }

    #[test]
    fn default_directives_match_binary_roles() {
        let _ = EnvFilter::new(DEFAULT_DIRECTIVE);
        let _ = EnvFilter::new(TEST_APP_DIRECTIVE);
        assert_eq!(DEFAULT_DIRECTIVE, "info");
        assert_eq!(TEST_APP_DIRECTIVE, "debug");
    }

    #[test]
    fn acp_debug_directive_does_not_raise_other_targets_or_product_default() {
        #[derive(Clone, Default)]
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        for (directive, acp_debug) in [
            (DEFAULT_DIRECTIVE, false),
            ("info,cowboy_agent_acp=debug", true),
        ] {
            let sink = Sink::default();
            let writer = sink.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::new(directive))
                .with_ansi(false)
                .without_time()
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                tracing::debug!(target: "cowboy_agent_acp::client", event = "acp_request_send_started", "ACP send started");
                tracing::debug!(target: "cowboy_workflow_engine", "unrelated-debug-marker");
                tracing::info!(target: "cowboy_workflow_engine", "normal-info-marker");
            });
            let bytes = sink.0.lock().unwrap();
            let log = std::str::from_utf8(&bytes).unwrap();
            assert_eq!(log.contains("acp_request_send_started"), acp_debug);
            assert!(!log.contains("unrelated-debug-marker"));
            assert!(log.contains("normal-info-marker"));
        }
    }

    #[test]
    fn panic_logs_only_safe_metadata() {
        #[derive(Clone, Default)]
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_panic(&"private-panic-canary", Some(42));
        });
        let bytes = sink.0.lock().unwrap();
        let log = std::str::from_utf8(&bytes).unwrap();
        assert!(log.contains("process_panic"));
        assert!(log.contains("payload_bytes=20"));
        assert!(log.contains("line=42"));
        assert!(!log.contains("private-panic-canary"));
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
