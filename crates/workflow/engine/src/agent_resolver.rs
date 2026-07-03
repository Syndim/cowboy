use std::collections::BTreeMap;

use cowboy_workflow_core::{Result, RoleDefinition, WorkflowError};

use crate::runtime::AgentRuntimeConfig;

const DEFAULT_AGENT_NAME: &str = "default";

#[derive(Debug, Clone)]
pub(crate) struct AgentResolver {
    agents: BTreeMap<String, AgentRuntimeConfig>,
    implicit: ImplicitAgent,
}

#[derive(Debug, Clone)]
enum ImplicitAgent {
    Default(String),
    Singleton(String),
    Ambiguous(Vec<String>),
}

impl AgentResolver {
    pub(crate) fn new(agents: Vec<AgentRuntimeConfig>) -> Result<Self> {
        if agents.is_empty() {
            return Err(WorkflowError::InvalidAction(
                "at least one agent must be configured".to_string(),
            ));
        }

        let mut by_name = BTreeMap::new();
        for agent in agents {
            if agent.name.trim().is_empty() {
                return Err(WorkflowError::InvalidAction(
                    "agent name must not be empty".to_string(),
                ));
            }
            if by_name.insert(agent.name.clone(), agent).is_some() {
                return Err(WorkflowError::InvalidAction(
                    "agent names must be unique".to_string(),
                ));
            }
        }

        let implicit = if by_name.contains_key(DEFAULT_AGENT_NAME) {
            ImplicitAgent::Default(DEFAULT_AGENT_NAME.to_string())
        } else if by_name.len() == 1 {
            ImplicitAgent::Singleton(by_name.keys().next().cloned().expect("one agent"))
        } else {
            ImplicitAgent::Ambiguous(by_name.keys().cloned().collect())
        };

        Ok(Self {
            agents: by_name,
            implicit,
        })
    }

    pub(crate) fn resolve(&self, role: &RoleDefinition) -> Result<&AgentRuntimeConfig> {
        if let Some(name) = &role.agent {
            return self.agents.get(name).ok_or_else(|| {
                WorkflowError::InvalidAction(format!(
                    "role {:?} specifies unknown agent {:?}",
                    role.id, name
                ))
            });
        }

        self.resolve_implicit(&format!("role {:?}", role.id))
    }

    pub(crate) fn resolve_default(&self) -> Result<&AgentRuntimeConfig> {
        self.resolve_implicit("agent-backed runtime operation")
    }

    fn resolve_implicit(&self, subject: &str) -> Result<&AgentRuntimeConfig> {
        match &self.implicit {
            ImplicitAgent::Default(name) | ImplicitAgent::Singleton(name) => {
                self.agents.get(name).ok_or_else(|| {
                    WorkflowError::InvalidAction("resolved agent is missing".to_string())
                })
            }
            ImplicitAgent::Ambiguous(names) => Err(WorkflowError::InvalidAction(format!(
                "{subject} does not specify an agent and configured agents are ambiguous: {}; configure an agent named default or set role agent explicitly",
                names.join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use cowboy_agent_client::ModelInfo;
    use serde_json::Value;

    use super::*;

    fn agent(name: &str) -> AgentRuntimeConfig {
        AgentRuntimeConfig {
            name: name.to_string(),
            command: format!("{name}-cmd"),
            args: Vec::new(),
            model: ModelInfo::default(),
        }
    }

    fn role(agent: Option<&str>) -> RoleDefinition {
        RoleDefinition {
            id: "developer".to_string(),
            instructions: "implement".to_string(),
            agent: agent.map(str::to_string),
            properties: Value::Null,
        }
    }

    #[test]
    fn explicit_agent_name_resolves_exact_match() {
        let resolver = AgentResolver::new(vec![agent("default"), agent("planner")]).unwrap();

        let resolved = resolver.resolve(&role(Some("planner"))).unwrap();

        assert_eq!(resolved.name, "planner");
    }

    #[test]
    fn explicit_missing_agent_fails() {
        let resolver = AgentResolver::new(vec![agent("default")]).unwrap();

        let err = resolver.resolve(&role(Some("missing"))).unwrap_err();

        assert!(err.to_string().contains("unknown agent"));
    }

    #[test]
    fn implicit_resolution_prefers_default() {
        let resolver = AgentResolver::new(vec![agent("reviewer"), agent("default")]).unwrap();

        let resolved = resolver.resolve(&role(None)).unwrap();

        assert_eq!(resolved.name, "default");
    }

    #[test]
    fn implicit_resolution_uses_singleton() {
        let resolver = AgentResolver::new(vec![agent("solo")]).unwrap();

        let resolved = resolver.resolve(&role(None)).unwrap();

        assert_eq!(resolved.name, "solo");
    }

    #[test]
    fn implicit_resolution_fails_for_multiple_non_default_agents() {
        let resolver = AgentResolver::new(vec![agent("planner"), agent("reviewer")]).unwrap();

        let err = resolver.resolve(&role(None)).unwrap_err();

        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn empty_agent_list_fails() {
        let err = AgentResolver::new(Vec::new()).unwrap_err();

        assert!(err.to_string().contains("at least one agent"));
    }

    #[test]
    fn blank_agent_name_fails() {
        let err = AgentResolver::new(vec![agent("   ")]).unwrap_err();

        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn duplicate_agent_name_fails() {
        let err = AgentResolver::new(vec![agent("default"), agent("default")]).unwrap_err();

        assert!(err.to_string().contains("unique"));
    }
}
