//! Bounded process exit for the `cowboy` binary.
//!
//! Dropping a Tokio runtime waits **without a timeout** for in-flight mandatory
//! blocking tasks, such as a pending read on an agent's stdout pipe. Building
//! the runtime explicitly lets `main` bound that final wait, so returning from
//! `main` always terminates the process.

use std::future::Future;
use std::time::Duration;

/// Default bound on the final Tokio runtime teardown wait.
pub const DEFAULT_PROCESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `work` to completion on a fresh multi-thread runtime, then shut the
/// runtime down within `shutdown_timeout` and return the result.
///
/// The work future is awaited in full, so persistence started by the
/// application still completes; only leftover background/blocking tasks are
/// abandoned once the bound elapses.
pub fn run_with_bounded_shutdown<F, T>(
    work: impl FnOnce() -> F,
    shutdown_timeout: Duration,
) -> std::io::Result<T>
where
    F: Future<Output = T>,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let value = runtime.block_on(work());
    runtime.shutdown_timeout(shutdown_timeout);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn bounded_shutdown_returns_despite_stuck_blocking_task() {
        let started = Instant::now();
        let value = run_with_bounded_shutdown(
            || async {
                tokio::task::spawn_blocking(|| {
                    std::thread::sleep(Duration::from_secs(120));
                });
                tokio::task::yield_now().await;
                "done"
            },
            Duration::from_millis(200),
        )
        .expect("build runtime");

        assert_eq!(value, "done");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "bounded shutdown did not return promptly: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn bounded_shutdown_completes_pending_work_before_returning() {
        let value = run_with_bounded_shutdown(
            || async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                41 + 1
            },
            DEFAULT_PROCESS_SHUTDOWN_TIMEOUT,
        )
        .expect("build runtime");

        assert_eq!(value, 42);
    }
}
