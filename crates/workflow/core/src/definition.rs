use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, RoleId, Status, StepId, WorkflowError, WorkflowId};

/// Available workflow sources indexed by workflow id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowCatalog {
    /// Workflow source descriptors available for selection.
    pub workflows: BTreeMap<WorkflowId, WorkflowSourceRef>,
}

impl WorkflowCatalog {
    pub fn new() -> Self {
        Self {
            workflows: BTreeMap::new(),
        }
    }
}

impl Default for WorkflowCatalog {
    fn default() -> Self {
        Self::new()
    }
}

/// Source location and metadata for a workflow definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSourceRef {
    /// Stable workflow id used by selectors and catalog references.
    pub id: WorkflowId,
    /// Entry Lua file path relative to the workflow root.
    pub entry: String,
    /// Optional workflow root directory. Built-ins may omit it.
    pub root: Option<String>,
    /// Optional human-readable description used by selectors.
    pub description: Option<String>,
}

/// Compiled workflow graph produced from Lua source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    /// Workflow name/id as declared by `Workflow:new`.
    pub name: WorkflowId,
    /// Optional human-readable description declared by the workflow source.
    #[serde(default)]
    pub description: Option<String>,
    /// Hash of the workflow source bundle used to compile this definition.
    pub source_hash: String,
    /// Step id where new runs begin.
    pub head: StepId,
    /// Role definitions registered by the workflow source.
    pub roles: BTreeMap<RoleId, RoleDefinition>,
    /// Step definitions registered by the workflow source.
    pub steps: BTreeMap<StepId, StepDefinition>,
}

/// Reusable role/persona metadata for agent actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleDefinition {
    /// Stable role id.
    pub id: RoleId,
    /// Instructions/persona text supplied to agent actions using this role.
    pub instructions: String,
    /// Optional named backend agent configured for this role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Additional Lua-defined role metadata.
    #[serde(default)]
    pub properties: Value,
}

/// Compiled step metadata and outgoing transitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepDefinition {
    /// Stable step id.
    pub id: StepId,
    /// Optional default role associated with this step.
    pub role: Option<RoleId>,
    /// Status-to-next-step routing table.
    #[serde(default)]
    pub transitions: StepTransitions,
    /// Additional Lua-defined step metadata.
    #[serde(default)]
    pub properties: Value,
}

/// Routing table for a step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepTransitions {
    /// Map from output status to next step id.
    pub by_status: BTreeMap<Status, StepId>,
}

impl StepTransitions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_status(mut self, status: impl Into<Status>, step: impl Into<StepId>) -> Self {
        self.by_status.insert(status.into(), step.into());
        self
    }

    pub fn insert(&mut self, status: impl Into<Status>, step: impl Into<StepId>) {
        self.by_status.insert(status.into(), step.into());
    }

    pub fn next_for(&self, status: &str) -> Option<&StepId> {
        self.by_status.get(status)
    }
}

/// Result of validating a workflow graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Steps not reachable from the workflow head.
    pub unreachable_steps: Vec<StepId>,
}

pub fn validate_definition(definition: &WorkflowDefinition) -> Result<ValidationReport> {
    if definition.name.trim().is_empty() {
        return Err(WorkflowError::EmptyWorkflowId);
    }
    if definition.head.trim().is_empty() {
        return Err(WorkflowError::EmptyHead {
            workflow: definition.name.clone(),
        });
    }
    if !definition.steps.contains_key(&definition.head) {
        return Err(WorkflowError::MissingHead {
            workflow: definition.name.clone(),
            step: definition.head.clone(),
        });
    }

    for (key, role) in &definition.roles {
        if role.id.trim().is_empty() {
            return Err(WorkflowError::EmptyRoleId);
        }
        if key != &role.id {
            return Err(WorkflowError::RoleIdMismatch {
                key: key.clone(),
                id: role.id.clone(),
            });
        }
        if let Some(agent) = &role.agent {
            if agent.trim().is_empty() {
                return Err(WorkflowError::EmptyRoleAgent {
                    role: role.id.clone(),
                });
            }
        }
    }

    for (key, step) in &definition.steps {
        if step.id.trim().is_empty() {
            return Err(WorkflowError::EmptyStepId);
        }
        if key != &step.id {
            return Err(WorkflowError::StepIdMismatch {
                key: key.clone(),
                id: step.id.clone(),
            });
        }
        if let Some(role) = &step.role {
            if !definition.roles.contains_key(role) {
                return Err(WorkflowError::UnknownRole {
                    step: step.id.clone(),
                    role: role.clone(),
                });
            }
        }
        for (status, target) in &step.transitions.by_status {
            if status.trim().is_empty() {
                return Err(WorkflowError::EmptyTransitionStatus {
                    step: step.id.clone(),
                });
            }
            if !definition.steps.contains_key(target) {
                return Err(WorkflowError::UnknownTransitionTarget {
                    step: step.id.clone(),
                    status: status.clone(),
                    target: target.clone(),
                });
            }
        }
    }

    Ok(ValidationReport {
        unreachable_steps: unreachable_steps(definition),
    })
}

pub fn next_step<'a>(
    definition: &'a WorkflowDefinition,
    step_id: &StepId,
    status: &str,
) -> Result<Option<&'a StepId>> {
    let Some(step) = definition.steps.get(step_id) else {
        return Err(WorkflowError::UnknownStep {
            step: step_id.clone(),
        });
    };
    if let Some(next) = step.transitions.next_for(status) {
        return Ok(Some(next));
    }
    if status == "success" {
        return Ok(None);
    }
    Err(WorkflowError::UnknownRuntimeTransition {
        step: step_id.clone(),
        status: status.to_string(),
    })
}

fn unreachable_steps(definition: &WorkflowDefinition) -> Vec<StepId> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([definition.head.clone()]);
    while let Some(step_id) = queue.pop_front() {
        if !seen.insert(step_id.clone()) {
            continue;
        }
        let Some(step) = definition.steps.get(&step_id) else {
            continue;
        };
        for target in step.transitions.by_status.values() {
            queue.push_back(target.clone());
        }
    }

    definition
        .steps
        .keys()
        .filter(|step| !seen.contains(*step))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str) -> StepDefinition {
        StepDefinition {
            id: id.to_string(),
            role: None,
            transitions: StepTransitions::new(),
            properties: Value::Null,
        }
    }

    fn role(id: &str, agent: Option<&str>) -> RoleDefinition {
        RoleDefinition {
            id: id.to_string(),
            instructions: "implement".to_string(),
            agent: agent.map(str::to_string),
            properties: Value::Null,
        }
    }

    fn definition() -> WorkflowDefinition {
        let mut plan = step("plan");
        plan.transitions.insert("success", "review");
        let review = step("review");
        WorkflowDefinition {
            name: "default".to_string(),
            description: None,
            source_hash: "hash".to_string(),
            head: "plan".to_string(),
            roles: BTreeMap::new(),
            steps: BTreeMap::from([("plan".to_string(), plan), ("review".to_string(), review)]),
        }
    }

    #[test]
    fn validates_reachable_graph() {
        let report = validate_definition(&definition()).unwrap();
        assert!(report.unreachable_steps.is_empty());
    }

    #[test]
    fn reports_unreachable_steps() {
        let mut definition = definition();
        definition
            .steps
            .insert("orphan".to_string(), step("orphan"));
        let report = validate_definition(&definition).unwrap();
        assert_eq!(report.unreachable_steps, vec!["orphan".to_string()]);
    }

    #[test]
    fn rejects_missing_head() {
        let mut definition = definition();
        definition.head = "missing".to_string();
        let err = validate_definition(&definition).unwrap_err();
        assert_eq!(
            err,
            WorkflowError::MissingHead {
                workflow: "default".to_string(),
                step: "missing".to_string()
            }
        );
    }

    #[test]
    fn validates_named_role_agent() {
        let mut definition = definition();
        definition
            .roles
            .insert("developer".to_string(), role("developer", Some("planner")));

        validate_definition(&definition).unwrap();
    }

    #[test]
    fn rejects_blank_role_agent() {
        let mut definition = definition();
        definition
            .roles
            .insert("developer".to_string(), role("developer", Some("   ")));

        let err = validate_definition(&definition).unwrap_err();
        assert_eq!(
            err,
            WorkflowError::EmptyRoleAgent {
                role: "developer".to_string()
            }
        );
    }

    #[test]
    fn rejects_unknown_transition_target() {
        let mut definition = definition();
        definition
            .steps
            .get_mut("plan")
            .unwrap()
            .transitions
            .insert("failed", "missing");
        let err = validate_definition(&definition).unwrap_err();
        assert_eq!(
            err,
            WorkflowError::UnknownTransitionTarget {
                step: "plan".to_string(),
                status: "failed".to_string(),
                target: "missing".to_string()
            }
        );
    }

    #[test]
    fn success_without_transition_is_terminal() {
        let definition = definition();
        let next = next_step(&definition, &"review".to_string(), "success").unwrap();
        assert!(next.is_none());
    }

    #[test]
    fn non_success_without_transition_fails() {
        let definition = definition();
        let err = next_step(&definition, &"review".to_string(), "needs_fix").unwrap_err();
        assert_eq!(
            err,
            WorkflowError::UnknownRuntimeTransition {
                step: "review".to_string(),
                status: "needs_fix".to_string()
            }
        );
    }

    #[test]
    fn unknown_current_step_fails() {
        let definition = definition();
        let err = next_step(&definition, &"missing".to_string(), "success").unwrap_err();
        assert_eq!(
            err,
            WorkflowError::UnknownStep {
                step: "missing".to_string()
            }
        );
    }
}
