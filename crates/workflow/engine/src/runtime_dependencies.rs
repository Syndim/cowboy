//! Backend-neutral wiring between the engine and agent backends.
//!
//! The engine never imports a concrete backend crate. Configuration is turned
//! into [`AgentLaunchSpec`] values (pure data with an opaque backend token)
//! and handed to the [`BackendRegistry`], which routes the spec to whichever
//! backend connector was installed at the composition root.

use std::sync::Arc;

use async_trait::async_trait;
use cowboy_agent_client::{AgentLaunchSpec, BackendRegistry, diagnostic_error_text};
use cowboy_workflow_agent::{ClientFactory, ResolvedAgentClient};
use cowboy_workflow_core::{Result, RoleDefinition, WorkflowError};

use crate::agent_resolver::AgentResolver;
use crate::runtime::{RuntimeConfig, SelectorMode};
use crate::workflow::AgentRequestTopicGenerator;

/// Default backend kind used when agent configuration predates backend kinds.
///
/// All existing configurations launch ACP agents; the field is optional and
/// defaults to this token so serialized configs stay byte-compatible.
pub const DEFAULT_BACKEND_KIND: &str = "acp";

/// Factory that resolves a role to launch data and connects through the
/// registry. The only backend knowledge here is the opaque token carried in
/// configuration data.
#[derive(Clone)]
pub struct RegistryClientFactory {
    resolver: AgentResolver,
    registry: BackendRegistry,
    global_allowed_env: Vec<String>,
}

impl RegistryClientFactory {
    pub(crate) fn new(
        resolver: AgentResolver,
        registry: BackendRegistry,
        global_allowed_env: Vec<String>,
    ) -> Self {
        Self {
            resolver,
            registry,
            global_allowed_env,
        }
    }

    fn launch_spec_for(&self, role: &RoleDefinition) -> Result<AgentLaunchSpec> {
        let agent = self.resolver.resolve(role)?;
        tracing::debug!(
            role = %role.id,
            backend = %agent.name,
            kind = %agent.backend,
            model_id = ?agent.model.as_ref().map(|model| model.id.as_str()),
            provider = ?agent.model.as_ref().and_then(|model| model.provider.as_deref()),
            argument_count = agent.args.len(),
            model_configured = agent.model.is_some(),
            "resolving agent client for role"
        );
        let mut allowed_env = Vec::new();
        for name in self.global_allowed_env.iter().chain(&agent.allowed_env) {
            if !allowed_env.contains(name) {
                allowed_env.push(name.clone());
            }
        }
        Ok(AgentLaunchSpec {
            agent: agent.name.clone(),
            backend: agent.backend.clone(),
            command: agent.command.clone(),
            args: agent.args.clone(),
            allowed_env,
            access: agent.access.clone(),
            watchdog: cowboy_agent_client::AgentWatchdogOptions {
                response_timeout_seconds: agent.watchdog.response_timeout_seconds,
                cancel_timeout_seconds: agent.watchdog.cancel_timeout_seconds,
                recovery_operation_timeout_seconds: agent
                    .watchdog
                    .recovery_operation_timeout_seconds,
            },
            startup_readiness: agent.startup_readiness.clone(),
            model: agent.model.clone(),
        })
    }
}

#[async_trait]
impl ClientFactory for RegistryClientFactory {
    async fn create_client(
        &self,
        role: &RoleDefinition,
    ) -> cowboy_workflow_agent::Result<ResolvedAgentClient> {
        let spec = self.launch_spec_for(role)?;
        let resolved = self.registry.connect(&spec).await?;
        Ok(ResolvedAgentClient {
            client: resolved.client,
            model: resolved.model,
            backend: resolved.agent,
        })
    }
}

/// Build the launch spec for the default agent (runtime operations such as
/// workflow selection and summarization).
pub(crate) fn default_launch_spec(config: &RuntimeConfig) -> Result<AgentLaunchSpec> {
    let resolver = AgentResolver::new(config.agents.clone())?;
    let agent = resolver.resolve_default()?;
    Ok(agent.launch_spec(&config.allowed_env))
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
    fn agent_factory(
        &self,
        config: &RuntimeConfig,
        registry: &BackendRegistry,
    ) -> Result<SharedClientFactory>;

    async fn generate_request_topic(
        &self,
        config: &RuntimeConfig,
        registry: &BackendRegistry,
        selector: SelectorMode,
        request: &str,
    ) -> Option<String>;
}

/// Protocol-free dependencies. The backend registry is owned by the runtime
/// and passed to each operation, so the composition root decides which
/// backends exist.
#[derive(Debug, Default)]
pub(crate) struct ProductionRuntimeDependencies;

#[async_trait]
impl RuntimeDependencies for ProductionRuntimeDependencies {
    fn agent_factory(
        &self,
        config: &RuntimeConfig,
        registry: &BackendRegistry,
    ) -> Result<SharedClientFactory> {
        tracing::debug!(
            agents = config.agents.len(),
            backends = ?registry.kinds(),
            "agent client factory configured"
        );
        Ok(SharedClientFactory::new(RegistryClientFactory::new(
            AgentResolver::new(config.agents.clone())?,
            registry.clone(),
            config.allowed_env.clone(),
        )))
    }

    async fn generate_request_topic(
        &self,
        config: &RuntimeConfig,
        registry: &BackendRegistry,
        selector: SelectorMode,
        request: &str,
    ) -> Option<String> {
        if matches!(selector, SelectorMode::Deterministic) {
            return None;
        }

        match generate_request_topic_result(config, registry, request).await {
            Ok(topic) => Some(topic),
            Err(error) => {
                tracing::warn!(
                    error = %diagnostic_error_text(&error.to_string()),
                    error_code = "topic_generation_failed",
                    "request topic generation failed"
                );
                None
            }
        }
    }
}

async fn generate_request_topic_result(
    config: &RuntimeConfig,
    registry: &BackendRegistry,
    request: &str,
) -> Result<String> {
    let spec = default_launch_spec(config)?;
    let resolved = registry
        .connect(&spec)
        .await
        .map_err(|err| WorkflowError::InvalidAction(err.to_string()))?;
    let generator = AgentRequestTopicGenerator::new(
        resolved.client,
        config.cwd.to_string_lossy().to_string(),
        spec.model.clone(),
    );
    generator.generate(request).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use cowboy_agent_client::{AgentAccess, AgentWatchdogOptions, BackendConnector};

    use super::*;
    use crate::runtime::{
        AgentRuntimeConfig, AgentStartupReadiness, AgentWatchdogRuntimeConfig, RuntimeConfig,
    };

    fn agent(name: &str, command: &str) -> AgentRuntimeConfig {
        AgentRuntimeConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            backend: DEFAULT_BACKEND_KIND.to_string(),
            model: None,
            access: AgentAccess::default(),
            allowed_env: Vec::new(),
            watchdog: AgentWatchdogRuntimeConfig::default(),
            startup_readiness: None,
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            cwd: std::env::current_dir().unwrap(),
            state_dir: "state".into(),
            workflow_store: "state/data.db".into(),
            workflow_dirs: Vec::new(),
            allowed_env: Vec::new(),
            allowed_cmds: None,
            agents: vec![
                agent("default", "default-agent-command"),
                agent("named", "named-agent-command"),
            ],
            config_sets: BTreeMap::from([(
                "default".to_string(),
                crate::runtime::RunnerLimitsConfig::default(),
            )]),
        }
    }

    /// Connector that records the specs it received and fails connection, so
    /// tests can assert on the launch data the engine produced.
    #[derive(Debug, Clone, Default)]
    struct RecordingConnector {
        specs: Arc<std::sync::Mutex<Vec<AgentLaunchSpec>>>,
    }

    #[async_trait]
    impl BackendConnector for RecordingConnector {
        fn kind(&self) -> &str {
            DEFAULT_BACKEND_KIND
        }

        async fn connect(
            &self,
            spec: &AgentLaunchSpec,
        ) -> anyhow::Result<cowboy_agent_client::ResolvedBackend> {
            self.specs.lock().unwrap().push(spec.clone());
            Err(anyhow::anyhow!("recording connector"))
        }
    }

    fn recording_registry() -> (BackendRegistry, Arc<std::sync::Mutex<Vec<AgentLaunchSpec>>>) {
        let connector = Arc::new(RecordingConnector::default());
        let specs = connector.specs.clone();
        (BackendRegistry::new(vec![connector]), specs)
    }

    #[tokio::test]
    async fn registry_factory_resolves_role_to_full_launch_spec() {
        let (registry, specs) = recording_registry();
        let mut config = config();
        config.agents[1].name = "reviewer".to_string();
        config.agents[1].command = "reviewer-command".to_string();
        config.agents[1].args = vec!["--flag".to_string()];
        config.agents[1].allowed_env = vec!["REVIEWER".to_string()];
        config.agents[1].watchdog = AgentWatchdogRuntimeConfig {
            response_timeout_seconds: 21,
            cancel_timeout_seconds: 22,
            recovery_operation_timeout_seconds: 23,
        };
        config.agents[1].access = AgentAccess::DenyEscalation {
            acp_mode: "read-only".to_string(),
        };
        config.allowed_env = vec!["GLOBAL".to_string()];
        let dependencies = ProductionRuntimeDependencies;
        let factory = dependencies.agent_factory(&config, &registry).unwrap();
        let role = RoleDefinition {
            id: "developer".to_string(),
            instructions: "implement".to_string(),
            agent: Some("reviewer".to_string()),
            properties: serde_json::Value::Null,
        };

        assert!(factory.create_client(&role).await.is_err());

        let specs = specs.lock().unwrap().clone();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.agent, "reviewer");
        assert_eq!(spec.backend, DEFAULT_BACKEND_KIND);
        assert_eq!(spec.command, "reviewer-command");
        assert_eq!(spec.args, ["--flag"]);
        assert_eq!(spec.allowed_env, ["GLOBAL", "REVIEWER"]);
        assert_eq!(
            spec.watchdog,
            AgentWatchdogOptions {
                response_timeout_seconds: 21,
                cancel_timeout_seconds: 22,
                recovery_operation_timeout_seconds: 23,
            }
        );
        assert_eq!(
            spec.access,
            AgentAccess::DenyEscalation {
                acp_mode: "read-only".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn registry_factory_resolves_implicit_default_agent() {
        let (registry, specs) = recording_registry();
        let dependencies = ProductionRuntimeDependencies;
        let factory = dependencies.agent_factory(&config(), &registry).unwrap();
        let role = RoleDefinition {
            id: "implicit".to_string(),
            instructions: "work".to_string(),
            agent: None,
            properties: serde_json::Value::Null,
        };

        assert!(factory.create_client(&role).await.is_err());

        let specs = specs.lock().unwrap().clone();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].agent, "default");
        assert_eq!(specs[0].command, "default-agent-command");
    }

    #[tokio::test]
    async fn request_topic_generation_uses_default_agent_launch_spec() {
        let (registry, specs) = recording_registry();
        let dependencies = ProductionRuntimeDependencies;

        let topic = dependencies
            .generate_request_topic(
                &config(),
                &registry,
                crate::runtime::SelectorMode::Agent,
                "summarize",
            )
            .await;

        assert_eq!(topic, None);
        let specs = specs.lock().unwrap().clone();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].agent, "default");
        assert_eq!(specs[0].command, "default-agent-command");
    }

    #[tokio::test]
    async fn unknown_backend_kind_fails_with_known_kinds() {
        let mut config = config();
        config.agents[0].backend = "typo".to_string();
        let (registry, specs) = recording_registry();
        let dependencies = ProductionRuntimeDependencies;
        let factory = dependencies.agent_factory(&config, &registry).unwrap();
        let role = RoleDefinition {
            id: "developer".to_string(),
            instructions: "work".to_string(),
            agent: Some("default".to_string()),
            properties: serde_json::Value::Null,
        };

        let error = match factory.create_client(&role).await {
            Ok(_) => panic!("unknown backend kind must fail"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("unknown backend kind \"typo\""), "{error}");
        assert_eq!(specs.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn default_agent_watchdog_and_readiness_flow_into_launch_spec() {
        let (registry, specs) = recording_registry();
        let mut config = config();
        config.agents[0].watchdog = AgentWatchdogRuntimeConfig {
            response_timeout_seconds: 11,
            cancel_timeout_seconds: 12,
            recovery_operation_timeout_seconds: 13,
        };
        config.agents[0].startup_readiness = Some(AgentStartupReadiness {
            log_path: "~/logs/session_{pid}/process.log".to_string(),
            contains: "ready".to_string(),
            timeout_seconds: 300,
        });
        let dependencies = ProductionRuntimeDependencies;
        let factory = dependencies.agent_factory(&config, &registry).unwrap();
        let role = RoleDefinition {
            id: "implicit".to_string(),
            instructions: "work".to_string(),
            agent: None,
            properties: serde_json::Value::Null,
        };

        assert!(factory.create_client(&role).await.is_err());
        let specs = specs.lock().unwrap().clone();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].watchdog,
            AgentWatchdogOptions {
                response_timeout_seconds: 11,
                cancel_timeout_seconds: 12,
                recovery_operation_timeout_seconds: 13,
            }
        );
        assert!(specs[0].startup_readiness.is_some());
    }
}
