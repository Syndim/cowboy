use std::sync::Arc;

use async_trait::async_trait;
use cowboy_agent_acp::transport::{StdioConfig, TransportConfig};
use cowboy_agent_acp::{AgentWatchdogOptions, Client as AcpClient};
use cowboy_workflow_agent::{ClientFactory, ResolvedAgentClient};
use cowboy_workflow_core::{Result, RoleDefinition, WorkflowError};

use crate::agent_resolver::AgentResolver;
use crate::runtime::{AgentRuntimeConfig, RuntimeConfig, SelectorMode};
use crate::workflow::AgentRequestTopicGenerator;

#[async_trait]
pub(crate) trait AcpConnector: Send + Sync {
    async fn connect(
        &self,
        transport: TransportConfig,
        watchdog: AgentWatchdogOptions,
    ) -> anyhow::Result<AcpClient>;
}

#[derive(Debug, Default)]
pub(crate) struct ProductionAcpConnector;

#[async_trait]
impl AcpConnector for ProductionAcpConnector {
    async fn connect(
        &self,
        transport: TransportConfig,
        watchdog: AgentWatchdogOptions,
    ) -> anyhow::Result<AcpClient> {
        AcpClient::connect_with_options(transport, watchdog).await
    }
}

#[derive(Clone)]
pub(crate) struct SharedClientFactory(Arc<dyn ClientFactory>);

impl SharedClientFactory {
    pub(crate) fn new(factory: impl ClientFactory + 'static) -> Self {
        Self(Arc::new(factory))
    }
}

#[async_trait]
impl ClientFactory for SharedClientFactory {
    async fn create_client(
        &self,
        role: &RoleDefinition,
    ) -> cowboy_workflow_agent::Result<ResolvedAgentClient> {
        self.0.create_client(role).await
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait RuntimeDependencies: Send + Sync {
    fn agent_factory(&self, config: &RuntimeConfig) -> Result<SharedClientFactory>;

    async fn generate_request_topic(
        &self,
        config: &RuntimeConfig,
        selector: SelectorMode,
        request: &str,
    ) -> Option<String>;
}

pub(crate) struct ProductionRuntimeDependencies {
    connector: Arc<dyn AcpConnector>,
}

impl ProductionRuntimeDependencies {
    pub(crate) fn new(connector: Arc<dyn AcpConnector>) -> Self {
        Self { connector }
    }
}

impl Default for ProductionRuntimeDependencies {
    fn default() -> Self {
        Self::new(Arc::new(ProductionAcpConnector))
    }
}

#[async_trait]
impl RuntimeDependencies for ProductionRuntimeDependencies {
    fn agent_factory(&self, config: &RuntimeConfig) -> Result<SharedClientFactory> {
        tracing::debug!(
            agents = config.agents.len(),
            "ACP client factory configured"
        );
        Ok(SharedClientFactory::new(AcpClientFactory {
            resolver: AgentResolver::new(config.agents.clone())?,
            connector: self.connector.clone(),
        }))
    }

    async fn generate_request_topic(
        &self,
        config: &RuntimeConfig,
        selector: SelectorMode,
        request: &str,
    ) -> Option<String> {
        if matches!(selector, SelectorMode::Deterministic) {
            return None;
        }

        match generate_request_topic_result(config, self.connector.as_ref(), request).await {
            Ok(topic) => Some(topic),
            Err(err) => {
                tracing::warn!(error = %err, "request topic generation failed");
                None
            }
        }
    }
}

async fn generate_request_topic_result(
    config: &RuntimeConfig,
    connector: &dyn AcpConnector,
    request: &str,
) -> Result<String> {
    let resolver = AgentResolver::new(config.agents.clone())?;
    let agent = resolver.resolve_default()?;
    let client = connector
        .connect(transport_for(agent), watchdog_options_for(agent))
        .await
        .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
    let generator = AgentRequestTopicGenerator::new(
        client,
        config.cwd.to_string_lossy().to_string(),
        agent.model.clone(),
    );
    generator.generate(request).await
}

#[derive(Clone)]
struct AcpClientFactory {
    resolver: AgentResolver,
    connector: Arc<dyn AcpConnector>,
}

#[async_trait]
impl ClientFactory for AcpClientFactory {
    async fn create_client(
        &self,
        role: &RoleDefinition,
    ) -> cowboy_workflow_agent::Result<ResolvedAgentClient> {
        let agent = self.resolver.resolve(role)?;
        tracing::debug!(
            role = %role.id,
            agent = %agent.name,
            command = %agent.command,
            args = ?agent.args,
            model_id = ?agent.model.as_ref().map(|model| model.id.as_str()),
            provider = ?agent.model.as_ref().and_then(|model| model.provider.as_deref()),
            "resolving ACP client for role"
        );
        let client = self
            .connector
            .connect(transport_for(agent), watchdog_options_for(agent))
            .await?;
        Ok(ResolvedAgentClient {
            client: Box::new(client),
            model: agent.model.clone(),
            backend: agent.name.clone(),
        })
    }
}

pub(crate) fn transport_for(agent: &AgentRuntimeConfig) -> TransportConfig {
    TransportConfig::Stdio(StdioConfig {
        command: agent.command.clone(),
        args: agent.args.clone(),
        env: Vec::new(),
    })
}

pub(crate) fn watchdog_options_for(agent: &AgentRuntimeConfig) -> AgentWatchdogOptions {
    AgentWatchdogOptions {
        response_timeout_seconds: agent.watchdog.response_timeout_seconds,
        cancel_timeout_seconds: agent.watchdog.cancel_timeout_seconds,
        recovery_operation_timeout_seconds: agent.watchdog.recovery_operation_timeout_seconds,
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;
    use crate::runtime::AgentWatchdogRuntimeConfig;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedAcpConnection {
        command: String,
        watchdog: AgentWatchdogOptions,
    }

    #[derive(Clone, Default)]
    struct RecordingAcpConnector {
        connections: Arc<Mutex<Vec<RecordedAcpConnection>>>,
    }

    impl RecordingAcpConnector {
        fn connections(&self) -> Vec<RecordedAcpConnection> {
            self.connections.lock().clone()
        }
    }

    #[async_trait]
    impl AcpConnector for RecordingAcpConnector {
        async fn connect(
            &self,
            transport: TransportConfig,
            watchdog: AgentWatchdogOptions,
        ) -> anyhow::Result<AcpClient> {
            let TransportConfig::Stdio(transport) = transport else {
                unreachable!("engine agents use stdio transport")
            };
            self.connections.lock().push(RecordedAcpConnection {
                command: transport.command,
                watchdog,
            });
            Err(anyhow::anyhow!("recording connector"))
        }
    }

    fn agent(
        name: &str,
        command: &str,
        response_timeout_seconds: u64,
        cancel_timeout_seconds: u64,
        recovery_operation_timeout_seconds: u64,
    ) -> AgentRuntimeConfig {
        AgentRuntimeConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            model: None,
            watchdog: AgentWatchdogRuntimeConfig {
                response_timeout_seconds,
                cancel_timeout_seconds,
                recovery_operation_timeout_seconds,
            },
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            cwd: std::env::current_dir().unwrap(),
            state_dir: "state".into(),
            workflow_store: "state/workflow.redb".into(),
            workflow_dirs: Vec::new(),
            agents: vec![
                agent("default", "default-agent-command", 11, 12, 13),
                agent("named", "named-agent-command", 21, 22, 23),
            ],
            config_sets: Default::default(),
        }
    }

    #[test]
    fn runtime_dependencies_preserve_selected_agent_watchdog_policy() {
        let agent = AgentRuntimeConfig {
            name: "reviewer".to_string(),
            command: "agent".to_string(),
            args: vec![],
            model: None,
            watchdog: AgentWatchdogRuntimeConfig {
                response_timeout_seconds: 7,
                cancel_timeout_seconds: 8,
                recovery_operation_timeout_seconds: 9,
            },
        };

        assert_eq!(
            watchdog_options_for(&agent),
            AgentWatchdogOptions {
                response_timeout_seconds: 7,
                cancel_timeout_seconds: 8,
                recovery_operation_timeout_seconds: 9,
            }
        );
    }

    #[tokio::test]
    async fn role_agent_client_construction_uses_explicit_named_watchdog() {
        let recording = Arc::new(RecordingAcpConnector::default());
        let dependencies = ProductionRuntimeDependencies::new(recording.clone());
        let factory = dependencies.agent_factory(&config()).unwrap();
        let role = RoleDefinition {
            id: "developer".to_string(),
            instructions: "implement".to_string(),
            agent: Some("named".to_string()),
            properties: serde_json::Value::Null,
        };

        let error = match factory.create_client(&role).await {
            Ok(_) => panic!("recording connector should fail client construction"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("recording connector"));
        assert_eq!(
            recording.connections(),
            [RecordedAcpConnection {
                command: "named-agent-command".to_string(),
                watchdog: AgentWatchdogOptions {
                    response_timeout_seconds: 21,
                    cancel_timeout_seconds: 22,
                    recovery_operation_timeout_seconds: 23,
                },
            }]
        );
    }

    #[tokio::test]
    async fn request_topic_client_construction_uses_default_watchdog() {
        let recording = Arc::new(RecordingAcpConnector::default());
        let dependencies = ProductionRuntimeDependencies::new(recording.clone());

        let topic = dependencies
            .generate_request_topic(&config(), SelectorMode::Agent, "summarize this request")
            .await;

        assert_eq!(topic, None);
        assert_eq!(
            recording.connections(),
            [RecordedAcpConnection {
                command: "default-agent-command".to_string(),
                watchdog: AgentWatchdogOptions {
                    response_timeout_seconds: 11,
                    cancel_timeout_seconds: 12,
                    recovery_operation_timeout_seconds: 13,
                },
            }]
        );
    }
}
