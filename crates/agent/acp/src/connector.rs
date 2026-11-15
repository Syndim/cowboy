//! [`BackendConnector`] implementation for ACP backends.
//!
//! Converts the backend-neutral [`AgentLaunchSpec`] into ACP transport,
//! watchdog, and access configuration and connects an ACP client. This is the
//! only place that maps the shared launch contract onto ACP protocol details.

use std::sync::Arc;

use async_trait::async_trait;
use cowboy_agent_client::{
    AgentLaunchSpec, AgentWatchdogOptions, BackendConnector, ResolvedBackend,
};

use crate::client::Client as AcpClient;
use crate::transport::{StdioConfig, TransportConfig};

/// Merge global and per-agent environment allow-lists, preserving order and
/// dropping duplicates.
pub fn merge_allowed_env(
    global_allowed_env: &[String],
    agent_allowed_env: &[String],
) -> Vec<String> {
    let mut allowed_env = Vec::new();
    for name in global_allowed_env.iter().chain(agent_allowed_env) {
        if !allowed_env.contains(name) {
            allowed_env.push(name.clone());
        }
    }
    allowed_env
}

/// Build the stdio transport configuration for one agent launch spec.
pub fn transport_for(spec: &AgentLaunchSpec) -> TransportConfig {
    TransportConfig::Stdio(StdioConfig {
        command: spec.command.clone(),
        args: spec.args.clone(),
        clear_env: true,
        allowed_env: spec.allowed_env.clone(),
        env: Vec::new(),
        startup_readiness: spec.startup_readiness.clone(),
    })
}

/// Build the watchdog policy for one agent launch spec.
pub fn watchdog_options_for(spec: &AgentLaunchSpec) -> AgentWatchdogOptions {
    spec.watchdog
}

/// Connector for ACP-speaking agent backends.
#[derive(Debug, Default)]
pub struct AcpBackendConnector;

impl AcpBackendConnector {
    /// The backend-kind token this connector registers under.
    pub const KIND: &'static str = "acp";
}

#[async_trait]
impl BackendConnector for AcpBackendConnector {
    fn kind(&self) -> &str {
        Self::KIND
    }

    async fn connect(&self, spec: &AgentLaunchSpec) -> anyhow::Result<ResolvedBackend> {
        let client = AcpClient::connect_with_options_and_access(
            transport_for(spec),
            watchdog_options_for(spec),
            spec.access.clone(),
        )
        .await?;
        Ok(ResolvedBackend {
            client: Box::new(client),
            model: spec.model.clone(),
            agent: spec.agent.clone(),
        })
    }

    async fn terminate_all_agents(&self, timeout: std::time::Duration) -> usize {
        crate::terminate_all_agent_processes(timeout).await
    }
}

/// Convenience constructor wiring [`AcpBackendConnector`] into a registry entry.
pub fn acp_connector() -> Arc<dyn BackendConnector> {
    Arc::new(AcpBackendConnector)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowboy_agent_client::{AgentAccess, ModelInfo, StartupReadiness};

    fn spec() -> AgentLaunchSpec {
        AgentLaunchSpec {
            agent: "default".to_string(),
            backend: AcpBackendConnector::KIND.to_string(),
            command: "copilot".to_string(),
            args: vec!["--acp".to_string()],
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
    fn transport_maps_spec_command_args_and_env() {
        let TransportConfig::Stdio(transport) = transport_for(&spec()) else {
            panic!("ACP uses stdio transport")
        };
        assert_eq!(transport.command, "copilot");
        assert_eq!(transport.args, ["--acp"]);
        assert!(transport.clear_env);
        assert_eq!(transport.allowed_env, ["GLOBAL", "AGENT"]);
        assert!(transport.startup_readiness.is_none());
    }

    #[test]
    fn transport_preserves_startup_readiness() {
        let mut configured = spec();
        configured.startup_readiness = Some(StartupReadiness {
            log_path: "~/logs/session_*_{pid}/process-*.log".to_string(),
            contains: "ACP server started".to_string(),
            timeout_seconds: 300,
        });
        let TransportConfig::Stdio(transport) = transport_for(&configured) else {
            panic!("ACP uses stdio transport")
        };
        assert_eq!(
            transport
                .startup_readiness
                .as_ref()
                .map(|r| r.contains.clone()),
            Some("ACP server started".to_string())
        );
    }

    #[test]
    fn watchdog_maps_spec_policy() {
        assert_eq!(
            watchdog_options_for(&spec()),
            AgentWatchdogOptions {
                response_timeout_seconds: 11,
                cancel_timeout_seconds: 12,
                recovery_operation_timeout_seconds: 13,
            }
        );
    }

    #[test]
    fn merge_allowed_env_appends_globals_then_agent_without_duplicates() {
        assert_eq!(
            merge_allowed_env(
                &["GLOBAL".into(), "SHARED".into()],
                &["SHARED".into(), "AGENT".into()]
            ),
            ["GLOBAL", "SHARED", "AGENT"]
        );
    }
}
