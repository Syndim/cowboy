//! Backend-neutral agent launch and connection contracts.
//!
//! The workflow engine resolves which configured agent a role uses, copies the
//! launch parameters into an [`AgentLaunchSpec`], and asks the registered
//! [`BackendRegistry`] to connect. Backend crates (ACP, pi RPC, future SDK
//! clients) implement [`BackendConnector`] and interpret the spec for their own
//! protocol. The engine never matches on protocol kind: the backend token is an
//! opaque registry key.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::access::AgentAccess;
use super::startup::StartupReadiness;
use super::watchdog::AgentWatchdogOptions;
use crate::types::ModelInfo;

/// Everything a backend needs to launch and connect to one configured agent.
///
/// Pure launch data with an opaque backend-kind token. The engine never
/// interprets the token; it only routes the spec to the registry entry
/// registered under that kind.
#[derive(Debug, Clone)]
pub struct AgentLaunchSpec {
    /// Configured agent name the spec was built from (diagnostics only).
    pub agent: String,
    /// Opaque backend kind token (e.g. `"acp"`, `"pi-rpc"`), matching the
    /// [`BackendConnector::kind`] of the connector that should handle it.
    pub backend: String,
    /// Executable to spawn.
    pub command: String,
    /// Arguments that put the executable into backend-server mode.
    pub args: Vec<String>,
    /// Environment variable names the spawned process may inherit.
    pub allowed_env: Vec<String>,
    /// Agent tool and permission boundary the backend must enforce.
    pub access: AgentAccess,
    /// Watchdog timeouts for one prompt turn.
    pub watchdog: AgentWatchdogOptions,
    /// Optional log-file readiness probe applied before the first prompt.
    pub startup_readiness: Option<StartupReadiness>,
    /// Model the backend should use, when the configuration requests one.
    pub model: Option<ModelInfo>,
}

/// A connected backend client plus the launch metadata that produced it.
#[derive(Debug)]
pub struct ResolvedBackend {
    pub client: Box<dyn crate::Client>,
    /// Backend model value carried from the launch spec.
    pub model: Option<ModelInfo>,
    /// Configured agent name the client was launched for.
    pub agent: String,
}

/// Factory that creates backend clients for workflow roles.
#[async_trait]
pub trait BackendConnector: Send + Sync {
    /// Opaque backend kind this connector handles (e.g. `"acp"`).
    fn kind(&self) -> &str;

    /// Connect to a backend described by `spec`.
    async fn connect(&self, spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend>;

    /// Terminate every live agent process tree, bounded by `timeout`, and
    /// report how many were terminated. Connectors that never spawn real agent
    /// processes keep the default no-op.
    async fn terminate_all_agents(&self, _timeout: std::time::Duration) -> usize {
        0
    }
}

/// Registry mapping opaque backend-kind tokens to connectors.
///
/// This is the single point in the system where a backend token is resolved to
/// a concrete protocol implementation. Everything above it — engine, executor,
/// actions — sees only `dyn Client`.
#[derive(Clone, Default)]
pub struct BackendRegistry {
    connectors: BTreeMap<String, Arc<dyn BackendConnector>>,
}

impl BackendRegistry {
    /// Build a registry from connector implementations.
    pub fn new(connectors: Vec<Arc<dyn BackendConnector>>) -> Self {
        let mut registry = Self::default();
        for connector in connectors {
            let kind = connector.kind().to_string();
            if registry.connectors.insert(kind, connector).is_some() {
                panic!("duplicate backend connector kind registered");
            }
        }
        registry
    }

    /// Look up the connector registered for `backend`, if any.
    pub fn connector(&self, backend: &str) -> Option<&Arc<dyn BackendConnector>> {
        self.connectors.get(backend)
    }

    /// Registered backend kinds, sorted, for diagnostics and help text.
    pub fn kinds(&self) -> Vec<String> {
        self.connectors.keys().cloned().collect()
    }

    /// Connect through the connector registered for `spec.backend`.
    ///
    /// Fails with a listing of known kinds when the token is unknown so
    /// configuration mistakes surface at connect time, not deep inside a run.
    pub async fn connect(&self, spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend> {
        let connector = self.connectors.get(&spec.backend).ok_or_else(|| {
            anyhow::anyhow!(
                "agent launch spec declares unknown backend kind {:?}; known kinds: {}",
                spec.backend,
                self.kinds().join(", ")
            )
        })?;
        connector.connect(spec).await
    }

    /// Terminate every live agent process across all registered connectors,
    /// bounded by `timeout`, and report how many were terminated.
    pub async fn terminate_all_agents(&self, timeout: std::time::Duration) -> usize {
        let mut terminated = 0;
        for connector in self.connectors.values() {
            terminated += connector.terminate_all_agents(timeout).await;
        }
        terminated
    }
}

impl std::fmt::Debug for dyn BackendConnector + Send + Sync {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackendConnector")
            .field("kind", &self.kind())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;

    #[derive(Debug)]
    struct FakeConnector {
        kind: &'static str,
    }

    #[async_trait]
    impl crate::Client for FakeClient {
        fn is_connected(&self) -> bool {
            true
        }

        fn agent_info(&self) -> Option<&crate::AgentInfo> {
            None
        }

        fn session_id(&self) -> Option<&str> {
            None
        }

        fn session_descriptor(&self) -> Option<&crate::AgentSessionDescriptor> {
            None
        }

        async fn new_session(
            &mut self,
            _cwd: &str,
            _mcp_servers: &[serde_json::Value],
            _model: Option<&crate::ModelInfo>,
        ) -> anyhow::Result<String> {
            Ok("session".to_string())
        }

        fn supports_load_session(&self) -> bool {
            false
        }

        async fn load_session(
            &mut self,
            _session_id: &str,
            _cwd: &str,
            _mcp_servers: &[serde_json::Value],
        ) -> anyhow::Result<Vec<Event>> {
            Ok(Vec::new())
        }

        async fn prompt(
            &mut self,
            _session_id: &str,
            _prompt_content: Vec<crate::PromptContent>,
            _cancellation: crate::PromptTurnCancellation,
            _event_handler: &mut (dyn FnMut(Event) + Send),
        ) -> anyhow::Result<crate::StopReason> {
            Ok(crate::StopReason::EndTurn)
        }

        async fn close(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FakeClient;

    #[async_trait]
    impl BackendConnector for FakeConnector {
        fn kind(&self) -> &str {
            self.kind
        }

        async fn connect(&self, spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend> {
            Ok(ResolvedBackend {
                client: Box::new(FakeClient),
                model: spec.model.clone(),
                agent: spec.agent.clone(),
            })
        }
    }

    fn spec(backend: &str) -> AgentLaunchSpec {
        AgentLaunchSpec {
            agent: "default".to_string(),
            backend: backend.to_string(),
            command: "agent".to_string(),
            args: Vec::new(),
            allowed_env: Vec::new(),
            access: AgentAccess::default(),
            watchdog: AgentWatchdogOptions::default(),
            startup_readiness: None,
            model: None,
        }
    }

    #[tokio::test]
    async fn registry_routes_spec_to_matching_connector() {
        let registry = BackendRegistry::new(vec![
            Arc::new(FakeConnector { kind: "acp" }),
            Arc::new(FakeConnector { kind: "pi-rpc" }),
        ]);

        let resolved = registry.connect(&spec("pi-rpc")).await.unwrap();
        assert_eq!(resolved.agent, "default");
        assert_eq!(registry.kinds(), ["acp", "pi-rpc"]);
    }

    #[tokio::test]
    async fn registry_names_known_kinds_for_unknown_backend() {
        let registry = BackendRegistry::new(vec![Arc::new(FakeConnector { kind: "acp" })]);

        let error = registry.connect(&spec("typo")).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown backend kind \"typo\"; known kinds: acp"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn registry_termination_summarizes_all_connectors() {
        #[derive(Debug)]
        struct TerminatingConnector {
            count: usize,
        }

        #[async_trait]
        impl BackendConnector for TerminatingConnector {
            fn kind(&self) -> &str {
                match self.count {
                    2 => "two",
                    3 => "three",
                    _ => "other",
                }
            }

            async fn connect(&self, _spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend> {
                Err(anyhow::anyhow!("unused"))
            }

            async fn terminate_all_agents(&self, _timeout: std::time::Duration) -> usize {
                self.count
            }
        }

        let registry = BackendRegistry::new(vec![
            Arc::new(TerminatingConnector { count: 2 }),
            Arc::new(TerminatingConnector { count: 3 }),
        ]);

        assert_eq!(
            registry
                .terminate_all_agents(std::time::Duration::from_secs(1))
                .await,
            5
        );
    }
}
