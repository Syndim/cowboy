use std::collections::BTreeMap;

use cowboy_workflow_core::{
    AgentAction, AskUserAction, FailAction, OutputSpec, RoleDefinition, StatusAction, StepAction,
    StepDefinition, StepTransitions, SuspendAction, WorkflowDefinition,
};
use mlua::{Lua, Table, Value};
use serde_json::{Map, Number};

use crate::{Error, Result};

pub(crate) fn workflow_from_value(
    lua: &Lua,
    value: Value,
    source_hash: String,
) -> Result<WorkflowDefinition> {
    let Value::Table(table) = value else {
        return Err(Error::MissingWorkflow);
    };
    let kind: Option<String> = table.get("__cowboy_kind")?;
    let Some(kind) = kind else {
        return Err(Error::MissingWorkflow);
    };
    if kind != "workflow" {
        return Err(Error::MissingWorkflow);
    }
    let name: String = table.get("name")?;
    if name.trim().is_empty() {
        return Err(Error::EmptyWorkflowName);
    }
    let head_table = expect_table(table.get("head")?, "workflow.head")?;
    let head: String = head_table.get("id")?;

    let description = optional_string(&table, "description")?;
    let roles = roles_from_registry(lua)?;
    let steps = steps_from_registry(lua)?;
    Ok(WorkflowDefinition {
        name,
        description,
        source_hash,
        head,
        roles,
        steps,
    })
}

fn roles_from_registry(lua: &Lua) -> Result<BTreeMap<String, RoleDefinition>> {
    let registry: Table = lua.globals().get("__cowboy_roles")?;
    let mut roles = BTreeMap::new();
    for pair in registry.pairs::<String, Table>() {
        let (key, role) = pair?;
        let id: String = role.get("id")?;
        let instructions: String = role.get("instructions")?;
        if id.trim().is_empty() {
            return Err(Error::InvalidRoleId);
        }
        let agent = optional_role_agent(role.get::<Value>("agent")?)?;
        let properties =
            table_properties_to_json(&role, &["__cowboy_kind", "id", "instructions", "agent"])?;
        roles.insert(
            key,
            RoleDefinition {
                id,
                instructions,
                agent,
                properties,
            },
        );
    }
    Ok(roles)
}

fn steps_from_registry(lua: &Lua) -> Result<BTreeMap<String, StepDefinition>> {
    let registry: Table = lua.globals().get("__cowboy_steps")?;
    let mut steps = BTreeMap::new();
    for pair in registry.pairs::<String, Table>() {
        let (key, step) = pair?;
        let id: String = step.get("id")?;
        if id.trim().is_empty() {
            return Err(Error::InvalidStepId);
        }
        let role = optional_role_id(step.get::<Value>("role")?)?;
        let run_value: Value = step.get("run")?;
        if !matches!(run_value, Value::Function(_)) {
            return Err(Error::MissingRunFunction(id));
        }
        let transitions = transitions_from_step(&step)?;
        let properties = table_properties_to_json(
            &step,
            &["__cowboy_kind", "id", "role", "transitions", "run"],
        )?;
        steps.insert(
            key,
            StepDefinition {
                id,
                role,
                transitions,
                properties,
            },
        );
    }
    Ok(steps)
}

fn transitions_from_step(step: &Table) -> Result<StepTransitions> {
    let step_id: String = step.get("id")?;
    let transitions_table: Table = step.get("transitions")?;
    let mut transitions = StepTransitions::new();
    for pair in transitions_table.pairs::<String, Value>() {
        let (status, target) = pair?;
        if status.trim().is_empty() {
            return Err(Error::InvalidTransitionStatus(step_id));
        }
        let target = expect_step_id(target, "transition target")?
            .ok_or_else(|| Error::InvalidTransitionTarget(step_id.clone()))?;
        transitions.insert(status, target);
    }
    Ok(transitions)
}

pub fn action_from_value(value: Value) -> Result<StepAction> {
    let table = expect_table(value, "action")?;
    let action: String = table.get("action").map_err(|_| Error::MissingActionKind)?;
    match action.as_str() {
        "agent" => Ok(StepAction::Agent(AgentAction {
            role: expect_role_id(table.get("role")?, "role")?,
            prompt: required_string(&table, &action, "prompt")?,
            output: output_spec(table.get::<Value>("output")?)?,
        })),
        "status" => Ok(StepAction::Status(StatusAction {
            status: required_string(&table, &action, "status")?,
            fields: lua_to_json(table.get::<Value>("fields")?)?,
            body: optional_string(&table, "body")?.unwrap_or_default(),
        })),
        "ask_user" => Ok(StepAction::AskUser(AskUserAction {
            id: required_string(&table, &action, "id")?,
            message: required_string(&table, &action, "message")?,
            choices: string_array(table.get::<Value>("choices")?)?,
        })),
        "fail" => Ok(StepAction::Fail(FailAction {
            reason: required_string(&table, &action, "reason")?,
        })),
        "suspend" => Ok(StepAction::Suspend(SuspendAction {
            reason: required_string(&table, &action, "reason")?,
        })),
        other => Err(Error::UnknownAction(other.to_string())),
    }
}

fn output_spec(value: Value) -> Result<Option<OutputSpec>> {
    match value {
        Value::Nil => Ok(None),
        Value::Table(table) => {
            let statuses = match table.get::<Value>("status")? {
                Value::Nil => Vec::new(),
                Value::String(s) => vec![s.to_str()?.to_string()],
                Value::Table(t) => table_to_string_vec(&t)?,
                _ => {
                    return Err(Error::InvalidActionField {
                        action: "agent".to_string(),
                        field: "output.status".to_string(),
                        reason: "must be a string or array of strings".to_string(),
                    });
                }
            };
            let fields = lua_to_json(table.get::<Value>("fields")?)?;
            Ok(Some(OutputSpec { statuses, fields }))
        }
        _ => Err(Error::InvalidActionField {
            action: "agent".to_string(),
            field: "output".to_string(),
            reason: "must be a table".to_string(),
        }),
    }
}

fn required_string(table: &Table, action: &str, field: &str) -> Result<String> {
    optional_string(table, field)?.ok_or_else(|| Error::MissingActionField {
        action: action.to_string(),
        field: field.to_string(),
    })
}

fn optional_string(table: &Table, field: &str) -> Result<Option<String>> {
    match table.get::<Value>(field)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        _ => Err(Error::InvalidActionField {
            action: "unknown".to_string(),
            field: field.to_string(),
            reason: "must be a string".to_string(),
        }),
    }
}

fn string_array(value: Value) -> Result<Vec<String>> {
    match value {
        Value::Nil => Ok(Vec::new()),
        Value::Table(table) => table_to_string_vec(&table),
        _ => Err(Error::InvalidActionField {
            action: "ask_user".to_string(),
            field: "choices".to_string(),
            reason: "must be an array of strings".to_string(),
        }),
    }
}

fn table_to_string_vec(table: &Table) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for value in table.sequence_values::<Value>() {
        match value? {
            Value::String(s) => out.push(s.to_str()?.to_string()),
            _ => return Err(Error::UnsupportedValue("string array".to_string())),
        }
    }
    Ok(out)
}

fn table_properties_to_json(table: &Table, reserved: &[&str]) -> Result<serde_json::Value> {
    let mut object = Map::new();
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::String(key) = key else {
            continue;
        };
        let key = key.to_str()?.to_string();
        if reserved.iter().any(|reserved| reserved == &key) {
            continue;
        }
        object.insert(key, lua_to_json(value)?);
    }
    Ok(serde_json::Value::Object(object))
}

fn optional_role_agent(value: Value) -> Result<Option<String>> {
    match value {
        Value::Nil => Ok(None),
        Value::String(agent) => {
            let agent = agent.to_str()?.to_string();
            if agent.trim().is_empty() {
                return Err(Error::InvalidRoleAgent);
            }
            Ok(Some(agent))
        }
        _ => Err(Error::InvalidRoleAgent),
    }
}

fn optional_role_id(value: Value) -> Result<Option<String>> {
    match value {
        Value::Nil => Ok(None),
        other => expect_role_id(other, "role").map(Some),
    }
}

fn expect_role_id(value: Value, path: &str) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.to_str()?.to_string()),
        Value::Table(t) => Ok(t.get("id")?),
        _ => Err(Error::UnsupportedValue(path.to_string())),
    }
}

fn expect_step_id(value: Value, path: &str) -> Result<Option<String>> {
    match value {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        Value::Table(t) => Ok(Some(t.get("id")?)),
        _ => Err(Error::UnsupportedValue(path.to_string())),
    }
}

fn expect_table(value: Value, path: &str) -> Result<Table> {
    match value {
        Value::Table(table) => Ok(table),
        _ => Err(Error::UnsupportedValue(path.to_string())),
    }
}

fn lua_to_json(value: Value) -> Result<serde_json::Value> {
    match value {
        Value::Nil => Ok(serde_json::Value::Null),
        Value::Boolean(v) => Ok(serde_json::Value::Bool(v)),
        Value::Integer(v) => Ok(serde_json::Value::Number(v.into())),
        Value::Number(v) => Number::from_f64(v)
            .map(serde_json::Value::Number)
            .ok_or_else(|| Error::UnsupportedValue("non-finite number".to_string())),
        Value::String(v) => Ok(serde_json::Value::String(v.to_str()?.to_string())),
        Value::Table(table) => table_to_json(table),
        _ => Err(Error::UnsupportedValue("lua value".to_string())),
    }
}

fn table_to_json(table: Table) -> Result<serde_json::Value> {
    let mut array_items = BTreeMap::new();
    let mut object = Map::new();
    let mut is_array = true;
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        match key {
            Value::Integer(index) if index > 0 => {
                array_items.insert(index as usize, lua_to_json(value)?);
            }
            Value::String(key) => {
                is_array = false;
                object.insert(key.to_str()?.to_string(), lua_to_json(value)?);
            }
            _ => {
                is_array = false;
            }
        }
    }
    if is_array {
        let len = array_items.len();
        if array_items.keys().copied().eq(1..=len) {
            return Ok(serde_json::Value::Array(
                array_items.into_values().collect(),
            ));
        }
    }
    Ok(serde_json::Value::Object(object))
}

pub(crate) fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> Result<Value> {
    Ok(match value {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(v) => Value::Boolean(*v),
        serde_json::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                Value::Integer(i)
            } else {
                Value::Number(v.as_f64().unwrap_or_default())
            }
        }
        serde_json::Value::String(v) => Value::String(lua.create_string(v)?),
        serde_json::Value::Array(values) => {
            let table = lua.create_table()?;
            for (i, value) in values.iter().enumerate() {
                table.set(i + 1, json_to_lua(lua, value)?)?;
            }
            Value::Table(table)
        }
        serde_json::Value::Object(values) => {
            let table = lua.create_table()?;
            for (key, value) in values {
                table.set(key.as_str(), json_to_lua(lua, value)?)?;
            }
            Value::Table(table)
        }
    })
}
