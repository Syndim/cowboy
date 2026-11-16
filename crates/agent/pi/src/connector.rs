//! [`BackendConnector`] implementation for the pi RPC backend.
//!
//! Converts the backend-neutral [`AgentLaunchSpec`] into a pi RPC subprocess
//! and a connected [`PiRpcClient`]. This is the only place mapping the shared
//! launch contract onto pi protocol details.

use std::sync::Arc;

use async_trait::async_trait;
use cowboy_agent_client::{AgentLaunchSpec, BackendConnector, ResolvedBackend};

use crate::client::PiRpcClient;

/// Connector for pi coding-agent backends speaking the native RPC protocol.
#[derive(Debug, Default)]
pub struct PiRpcBackendConnector;

impl PiRpcBackendConnector {
    /// The backend-kind token this connector registers under.
    pub const KIND: &'static str = "pi-rpc";
}

#[async_trait]
impl BackendConnector for PiRpcBackendConnector {
    fn kind(&self) -> &str {
        Self::KIND
    }

    async fn connect(&self, spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend> {
        if spec.backend != Self::KIND {
            anyhow::bail!(
                "pi connector received launch spec for backend {:?}; expected {:?}",
                spec.backend,
                Self::KIND
            );
        }
        // Cowboy owns the role-session id, so a fresh connect always starts
        // with a new UUID; `load_session` restarts pi onto a persisted id.
        let client = PiRpcClient::connect(spec, None).await?;
        Ok(ResolvedBackend {
            client: Box::new(client),
            model: spec.model.clone(),
            agent: spec.agent.clone(),
        })
    }
}

/// Convenience constructor for registry wiring.
pub fn pi_connector() -> Arc<dyn BackendConnector> {
    Arc::new(PiRpcBackendConnector)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowboy_agent_client::{AgentAccess, AgentWatchdogOptions, ModelInfo};

    fn spec() -> AgentLaunchSpec {
        AgentLaunchSpec {
            agent: "default".to_string(),
            backend: PiRpcBackendConnector::KIND.to_string(),
            command: "pi".to_string(),
            args: vec!["--mode".to_string(), "rpc".to_string()],
            allowed_env: vec!["GLOBAL".to_string(), "AGENT".to_string()],
            access: AgentAccess::default(),
            watchdog: AgentWatchdogOptions {
                response_timeout_seconds: 11,
                cancel_timeout_seconds: 12,
                recovery_operation_timeout_seconds: 13,
            },
            startup_readiness: None,
            model: Some(ModelInfo::with_id("claude-sonnet-4.5")),
        }
    }

    #[test]
    fn backend_kind_is_stable() {
        // The engine routes config tokens through the registry under this
        // exact key; renaming it breaks every `backend = "pi-rpc"` config.
        assert_eq!(PiRpcBackendConnector::KIND, "pi-rpc");
    }

    #[tokio::test]
    async fn connector_rejects_foreign_backend_kind() {
        let mut mismatched = spec();
        mismatched.backend = "acp".to_string();
        let error = PiRpcBackendConnector
            .connect(&mismatched)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("received launch spec for backend \"acp\""),
            "{error}"
        );
    }

    #[tokio::test]
    async fn connector_defers_missing_binary_failure_until_session_cwd_is_known() {
        let mut missing = spec();
        missing.command = "definitely-not-pi-binary-xyz".to_string();
        let mut backend = PiRpcBackendConnector.connect(&missing).await.unwrap();
        let error = backend
            .client
            .new_session(".", &[], None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("definitely-not-pi-binary-xyz"),
            "{error}"
        );
    }
}
