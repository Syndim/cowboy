use std::sync::Arc;

use async_trait::async_trait;
use cowboy_agent_acp::Client as AcpClient;
use cowboy_agent_acp::transport::{StdioConfig, TransportConfig};
use cowboy_workflow_agent::{ClientFactory, ResolvedAgentClient};
use cowboy_workflow_core::{Result, RoleDefinition, WorkflowError};

use crate::agent_resolver::AgentResolver;
use crate::runtime::{AgentRuntimeConfig, RuntimeConfig, SelectorMode};
use crate::workflow::AgentRequestTopicGenerator;

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

#[derive(Debug, Default)]
pub(crate) struct ProductionRuntimeDependencies;

#[async_trait]
impl RuntimeDependencies for ProductionRuntimeDependencies {
    fn agent_factory(&self, config: &RuntimeConfig) -> Result<SharedClientFactory> {
        tracing::debug!(
            agents = config.agents.len(),
            "ACP client factory configured"
        );
        Ok(SharedClientFactory::new(AcpClientFactory {
            resolver: AgentResolver::new(config.agents.clone())?,
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

        match generate_request_topic_result(config, request).await {
            Ok(topic) => Some(topic),
            Err(err) => {
                tracing::warn!(error = %err, "request topic generation failed");
                None
            }
        }
    }
}

async fn generate_request_topic_result(config: &RuntimeConfig, request: &str) -> Result<String> {
    let resolver = AgentResolver::new(config.agents.clone())?;
    let agent = resolver.resolve_default()?;
    let client = AcpClient::connect(transport_for(agent))
        .await
        .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
    let generator = AgentRequestTopicGenerator::new(
        client,
        config.cwd.to_string_lossy().to_string(),
        agent.model.clone(),
    );
    generator.generate(request).await
}

#[derive(Debug, Clone)]
struct AcpClientFactory {
    resolver: AgentResolver,
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
            model_id = %agent.model.id,
            provider = ?agent.model.provider,
            "resolving ACP client for role"
        );
        let client = AcpClient::connect(transport_for(agent)).await?;
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
