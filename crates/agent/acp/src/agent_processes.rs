//! Process-wide registry of live agent process trees.
//!
//! Transports are owned by clients inside background tasks, so a process exit
//! path cannot reach them through ordinary references. Every spawned agent tree
//! registers here and deregisters on close, giving Cowboy a bounded
//! [`terminate_all_agent_processes`] entry point for deterministic shutdown.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use crate::process_tree::ProcessTreeScope;

/// Identifies one registered agent process tree.
pub(crate) type RegistrationId = u64;

type Registry = Mutex<BTreeMap<RegistrationId, Weak<ProcessTreeScope>>>;

fn global_registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(not(test))]
fn registry() -> &'static Registry {
    global_registry()
}

/// Tests share one process, so registry tests use a thread-local registry to
/// stay isolated from transports spawned by other tests running in parallel.
#[cfg(test)]
fn registry() -> &'static Registry {
    test_scope::current().unwrap_or_else(global_registry)
}

#[cfg(test)]
mod test_scope {
    use std::cell::Cell;

    use super::Registry;

    thread_local! {
        static SCOPED: Cell<Option<&'static Registry>> = const { Cell::new(None) };
    }

    pub(super) fn current() -> Option<&'static Registry> {
        SCOPED.with(Cell::get)
    }

    /// Installs a registry private to the current test thread.
    pub(super) struct ScopedRegistry;

    impl ScopedRegistry {
        pub(super) fn new() -> Self {
            let registry: &'static Registry = Box::leak(Box::new(std::sync::Mutex::new(
                std::collections::BTreeMap::new(),
            )));
            SCOPED.with(|scoped| scoped.set(Some(registry)));
            Self
        }
    }

    impl Drop for ScopedRegistry {
        fn drop(&mut self) {
            SCOPED.with(|scoped| scoped.set(None));
        }
    }
}

fn next_id() -> RegistrationId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::SeqCst)
}

/// Register a live agent process tree; the registry holds a weak reference.
pub(crate) fn register(scope: &Arc<ProcessTreeScope>) -> RegistrationId {
    let id = next_id();
    if let Ok(mut registry) = registry().lock() {
        registry.insert(id, Arc::downgrade(scope));
    }

    id
}

/// Drop a previously registered agent process tree.
pub(crate) fn deregister(id: RegistrationId) {
    if let Ok(mut registry) = registry().lock() {
        registry.remove(&id);
    }
}

fn take_live_scopes() -> Vec<Arc<ProcessTreeScope>> {
    let Ok(mut registry) = registry().lock() else {
        return Vec::new();
    };

    let scopes = registry
        .values()
        .filter_map(Weak::upgrade)
        .collect::<Vec<_>>();
    registry.clear();
    scopes
}

fn terminate_live_scopes() -> usize {
    let mut terminated = 0;
    for scope in take_live_scopes() {
        match scope.terminate() {
            Ok(()) => terminated += 1,
            Err(err) => {
                tracing::warn!(error = %err, "failed to terminate agent process tree");
            }
        }
    }

    terminated
}

/// Terminate every live agent process tree, bounded by `timeout`.
///
/// Returns how many trees were terminated. Never panics; termination failures
/// are logged and skipped.
pub async fn terminate_all_agent_processes(timeout: Duration) -> usize {
    match tokio::time::timeout(timeout, async { terminate_live_scopes() }).await {
        Ok(terminated) => {
            tracing::debug!(terminated, "terminated live agent process trees");
            terminated
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = timeout.as_millis(),
                "timed out terminating live agent process trees"
            );
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::transport::stdio::StdioTransport;
    use crate::transport::{StdioConfig, Transport};

    /// Both descendants must be forked *before* the readiness line is
    /// written: a reader that observes "ready" and kills the process group
    /// immediately can otherwise race ahead of a fork that happens only
    /// after the write, terminating the group before that descendant even
    /// exists (and is therefore never a member of the killed group).
    fn agent_leaving_a_descendant() -> StdioConfig {
        #[cfg(windows)]
        let (command, args) = (
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -NoNewWindow; Write-Output ready; Start-Sleep -Seconds 30".to_string(),
            ],
        );

        #[cfg(not(windows))]
        let (command, args) = (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "sleep 30 & sleep 30 & echo ready; wait".to_string(),
            ],
        );

        StdioConfig {
            command,
            args,
            clear_env: false,
            allowed_env: vec![],
            env: vec![],
        }
    }

    #[tokio::test]
    async fn terminate_all_agent_processes_terminates_registered_transport() {
        let _scope = test_scope::ScopedRegistry::new();
        let mut transport = StdioTransport::connect(&agent_leaving_a_descendant(), &[])
            .await
            .expect("spawn agent subprocess");
        let ready = tokio::time::timeout(Duration::from_secs(30), transport.recv())
            .await
            .expect("agent readiness line within 30s")
            .expect("read agent readiness line");
        assert_eq!(ready.as_deref(), Some("ready"));

        let terminated = terminate_all_agent_processes(Duration::from_secs(5)).await;
        assert!(terminated >= 1, "expected a registered agent process tree");

        let drained = tokio::time::timeout(Duration::from_secs(5), transport.recv()).await;
        assert!(
            drained.is_ok(),
            "pending stdout read did not complete after terminate_all_agent_processes"
        );
    }

    #[tokio::test]
    async fn closed_transport_is_deregistered() {
        let _scope = test_scope::ScopedRegistry::new();
        let config = agent_leaving_a_descendant();
        let mut transport = StdioTransport::connect(&config, &[])
            .await
            .expect("spawn agent subprocess");
        transport.close().await.expect("close transport");

        assert_eq!(
            terminate_all_agent_processes(Duration::from_secs(5)).await,
            0
        );
    }
}
