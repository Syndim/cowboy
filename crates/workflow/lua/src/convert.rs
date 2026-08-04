use std::collections::BTreeMap;

use cowboy_workflow_core::{
    AgentAction, AgentTaskContract, AskUserAction, Choice, CommandAction, FailAction, Field,
    FieldType, Fields, OutputSpec, RoleDefinition, StatusAction, StepAction, StepDefinition,
    StepTransitions, WorkflowAction, WorkflowDefinition, default_command_status_map,
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
    let config_set = optional_workflow_config_set(&table)?;
    let roles = roles_from_registry(lua)?;
    let steps = steps_from_registry(lua)?;
    Ok(WorkflowDefinition {
        name,
        description,
        config_set,
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
        "agent" => {
            let prompt = required_string(&table, &action, "prompt")?;
            Ok(StepAction::Agent(AgentAction {
                role: expect_role_id(table.get("role")?, "role")?,
                task: agent_task_contract(table.get::<Value>("task")?, &prompt)?,
                prompt,
                output: output_spec(table.get::<Value>("output")?)?,
            }))
        }
        "command" => Ok(StepAction::Command(CommandAction {
            program: non_empty_required_string(&table, &action, "program")?,
            args: string_array_field(&table, &action, "args")?,
            status_map: status_map_field(&table, &action)?,
            timeout_ms: optional_positive_timeout_ms(&table, &action)?,
        })),
        "status" => Ok(StepAction::Status(StatusAction {
            status: required_string(&table, &action, "status")?,
            fields: action_fields(table.get::<Value>("fields")?, &action)?,
            body: optional_string(&table, "body")?.unwrap_or_default(),
        })),
        "ask_user" => Ok(StepAction::AskUser(AskUserAction {
            id: required_string(&table, &action, "id")?,
            message: required_string(&table, &action, "message")?,
            choices: choices_field(table.get::<Value>("choices")?)?,
            status: optional_string(&table, "status")?.unwrap_or_else(|| "answered".to_string()),
            fields: action_fields(table.get::<Value>("fields")?, &action)?,
        })),
        "workflow" => Ok(StepAction::Workflow(WorkflowAction {
            workflow: non_empty_required_string(&table, &action, "workflow")?,
            request: required_string(&table, &action, "request")?,
        })),
        "fail" => Ok(StepAction::Fail(FailAction {
            reason: required_string(&table, &action, "reason")?,
        })),
        other => Err(Error::UnknownAction(other.to_string())),
    }
}

fn agent_task_contract(value: Value, default_turn: &str) -> Result<Option<AgentTaskContract>> {
    match value {
        Value::Nil => Ok(None),
        Value::Table(table) => {
            let key =
                non_empty_required_string(&table, "agent", "key").map_err(|err| match err {
                    Error::MissingActionField { action, .. } => Error::MissingActionField {
                        action,
                        field: "task.key".to_string(),
                    },
                    Error::InvalidActionField { action, reason, .. } => Error::InvalidActionField {
                        action,
                        field: "task.key".to_string(),
                        reason,
                    },
                    other => other,
                })?;
            Ok(Some(AgentTaskContract {
                key,
                instructions: required_string(&table, "agent", "instructions")?,
                recovery_context: optional_string(&table, "recovery_context")?.unwrap_or_default(),
                turn: optional_string(&table, "turn")?.unwrap_or_else(|| default_turn.to_string()),
            }))
        }
        _ => Err(Error::InvalidActionField {
            action: "agent".to_string(),
            field: "task".to_string(),
            reason: "must be a table".to_string(),
        }),
    }
}

/// Parse a `fields` table into a name/value map, e.g. for `status`/`ask_user`
/// actions. An absent or empty table yields an empty map; a non-empty
/// non-object table (e.g. a plain array) is rejected.
fn action_fields(value: Value, action: &str) -> Result<Fields> {
    match lua_to_json(value)? {
        serde_json::Value::Null => Ok(Fields::new()),
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        serde_json::Value::Array(items) if items.is_empty() => Ok(Fields::new()),
        _ => Err(Error::InvalidActionField {
            action: action.to_string(),
            field: "fields".to_string(),
            reason: "must be a table of name/value pairs".to_string(),
        }),
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
            let required_fields = match table.get::<Value>("required_fields")? {
                Value::Nil => Vec::new(),
                Value::Table(t) => table_to_string_vec(&t)?,
                _ => {
                    return Err(Error::InvalidActionField {
                        action: "agent".to_string(),
                        field: "output.required_fields".to_string(),
                        reason: "must be an array of strings".to_string(),
                    });
                }
            };
            let fields = output_fields(table.get::<Value>("fields")?, &required_fields)?;
            Ok(Some(OutputSpec { statuses, fields }))
        }
        _ => Err(Error::InvalidActionField {
            action: "agent".to_string(),
            field: "output".to_string(),
            reason: "must be a table".to_string(),
        }),
    }
}

/// Parse the `output.fields` table into declared `Field`s keyed by name,
/// applying `required_fields` membership as each field's `required` flag.
///
/// Each entry accepts either a plain type string (e.g. `"array"`) or a table
/// `{ type = "...", description = "..." }` carrying prompt guidance for the
/// agent about what to return in that field.
fn output_fields(value: Value, required_fields: &[String]) -> Result<BTreeMap<String, Field>> {
    let table = match value {
        Value::Nil => return Ok(BTreeMap::new()),
        Value::Table(table) => table,
        _ => {
            return Err(Error::InvalidActionField {
                action: "agent".to_string(),
                field: "output.fields".to_string(),
                reason: "must be a table".to_string(),
            });
        }
    };

    let mut fields = BTreeMap::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, descriptor) = pair?;
        let Value::String(key) = key else {
            return Err(Error::InvalidActionField {
                action: "agent".to_string(),
                field: "output.fields".to_string(),
                reason: "keys must be strings".to_string(),
            });
        };
        let name = key.to_str()?.to_string();
        let (field_type, description) = field_descriptor(&name, descriptor)?;
        fields.insert(
            name.clone(),
            Field {
                field_type,
                required: required_fields.iter().any(|required| required == &name),
                description,
            },
        );
    }
    Ok(fields)
}

/// Parse one `output.fields` entry into its declared type and description.
fn field_descriptor(name: &str, value: Value) -> Result<(FieldType, String)> {
    match value {
        Value::String(s) => Ok((field_type(name, s.to_str()?.as_ref())?, String::new())),
        Value::Table(table) => {
            let type_str: String = table.get("type").map_err(|_| Error::InvalidActionField {
                action: "agent".to_string(),
                field: format!("output.fields.{name}.type"),
                reason: "must be a string".to_string(),
            })?;
            let description = match table.get::<Value>("description")? {
                Value::Nil => String::new(),
                Value::String(s) => s.to_str()?.to_string(),
                _ => {
                    return Err(Error::InvalidActionField {
                        action: "agent".to_string(),
                        field: format!("output.fields.{name}.description"),
                        reason: "must be a string".to_string(),
                    });
                }
            };
            Ok((field_type(name, &type_str)?, description))
        }
        _ => Err(Error::InvalidActionField {
            action: "agent".to_string(),
            field: format!("output.fields.{name}"),
            reason: "must be a string or a table with type/description".to_string(),
        }),
    }
}

fn field_type(name: &str, value: &str) -> Result<FieldType> {
    match value {
        "array" => Ok(FieldType::Array),
        "boolean" => Ok(FieldType::Boolean),
        "number" => Ok(FieldType::Number),
        "string" => Ok(FieldType::String),
        _ => Err(Error::InvalidActionField {
            action: "agent".to_string(),
            field: format!("output.fields.{name}"),
            reason: "must be one of \"array\", \"boolean\", \"number\", \"string\"".to_string(),
        }),
    }
}

fn non_empty_required_string(table: &Table, action: &str, field: &str) -> Result<String> {
    let value = required_string(table, action, field)?;
    if value.trim().is_empty() {
        return Err(Error::InvalidActionField {
            action: action.to_string(),
            field: field.to_string(),
            reason: "must be a non-empty string".to_string(),
        });
    }

    Ok(value)
}

fn required_string(table: &Table, action: &str, field: &str) -> Result<String> {
    optional_string(table, field)?.ok_or_else(|| Error::MissingActionField {
        action: action.to_string(),
        field: field.to_string(),
    })
}

fn optional_workflow_config_set(table: &Table) -> Result<Option<String>> {
    match table.get::<Value>("config_set")? {
        Value::Nil => Ok(None),
        Value::String(value) => {
            let value = value.to_str()?.to_string();
            if value.trim().is_empty() {
                return Err(Error::InvalidWorkflowConfigSet);
            }

            Ok(Some(value))
        }
        _ => Err(Error::InvalidWorkflowConfigSet),
    }
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

/// Parse the `ask_user` `choices` table into `Choice`s keyed by answer key.
///
/// Accepts a table mapping each accepted answer key to a human-readable
/// description string, e.g. `{ yes = "Approve the release", no = "Reject
/// the release" }`. Choices are returned sorted by key for deterministic
/// ordering. An absent table yields an empty (free-form) choice list.
fn choices_field(value: Value) -> Result<Vec<Choice>> {
    let table = match value {
        Value::Nil => return Ok(Vec::new()),
        Value::Table(table) => table,
        _ => {
            return Err(Error::InvalidActionField {
                action: "ask_user".to_string(),
                field: "choices".to_string(),
                reason: "must be a table of choice key/description pairs".to_string(),
            });
        }
    };

    let mut choices = BTreeMap::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, description) = pair?;
        let Value::String(key) = key else {
            return Err(Error::InvalidActionField {
                action: "ask_user".to_string(),
                field: "choices".to_string(),
                reason: "keys must be strings".to_string(),
            });
        };
        let Value::String(description) = description else {
            return Err(Error::InvalidActionField {
                action: "ask_user".to_string(),
                field: "choices".to_string(),
                reason: "values must be strings".to_string(),
            });
        };
        choices.insert(key.to_str()?.to_string(), description.to_str()?.to_string());
    }

    Ok(choices
        .into_iter()
        .map(|(key, description)| Choice { key, description })
        .collect())
}

fn string_array_field(table: &Table, action: &str, field: &str) -> Result<Vec<String>> {
    match table.get::<Value>(field)? {
        Value::Nil => Ok(Vec::new()),
        Value::Table(table) => strict_string_array(&table, action, field),
        _ => Err(Error::InvalidActionField {
            action: action.to_string(),
            field: field.to_string(),
            reason: "must be an array of strings".to_string(),
        }),
    }
}

fn strict_string_array(table: &Table, action: &str, field: &str) -> Result<Vec<String>> {
    let len = table.raw_len();
    let mut out = vec![None; len];
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::Integer(index) = key else {
            return Err(Error::InvalidActionField {
                action: action.to_string(),
                field: field.to_string(),
                reason: "must be an array of strings".to_string(),
            });
        };
        if index < 1 || index as usize > len {
            return Err(Error::InvalidActionField {
                action: action.to_string(),
                field: field.to_string(),
                reason: "must be a contiguous array of strings".to_string(),
            });
        }
        let Value::String(value) = value else {
            return Err(Error::InvalidActionField {
                action: action.to_string(),
                field: field.to_string(),
                reason: "must be an array of strings".to_string(),
            });
        };
        out[index as usize - 1] = Some(value.to_str()?.to_string());
    }

    out.into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Error::InvalidActionField {
            action: action.to_string(),
            field: field.to_string(),
            reason: "must be a contiguous array of strings".to_string(),
        })
}

fn optional_positive_timeout_ms(table: &Table, action: &str) -> Result<Option<u64>> {
    match table.get::<Value>("timeout_ms")? {
        Value::Nil => Ok(None),
        Value::Integer(value) if value > 0 => Ok(Some(value as u64)),
        Value::Integer(_) => Err(Error::InvalidActionField {
            action: action.to_string(),
            field: "timeout_ms".to_string(),
            reason: "must be greater than zero".to_string(),
        }),
        _ => Err(Error::InvalidActionField {
            action: action.to_string(),
            field: "timeout_ms".to_string(),
            reason: "must be a positive integer".to_string(),
        }),
    }
}

/// Parse the `status_map` table into exit-code (or `"_"` catch-all) keys
/// mapped to output statuses. An absent table defaults to
/// `{"0" => "success", "_" => "failed"}`.
fn status_map_field(table: &Table, action: &str) -> Result<BTreeMap<String, String>> {
    match table.get::<Value>("status_map")? {
        Value::Nil => Ok(default_command_status_map()),
        Value::Table(map) => {
            let mut status_map = BTreeMap::new();
            for pair in map.pairs::<Value, Value>() {
                let (key, value) = pair?;
                let key = status_map_key(key, action)?;
                let Value::String(value) = value else {
                    return Err(Error::InvalidActionField {
                        action: action.to_string(),
                        field: "status_map".to_string(),
                        reason: "values must be non-empty strings".to_string(),
                    });
                };
                let value = value.to_str()?.to_string();
                if value.trim().is_empty() {
                    return Err(Error::InvalidActionField {
                        action: action.to_string(),
                        field: "status_map".to_string(),
                        reason: "values must be non-empty strings".to_string(),
                    });
                }
                status_map.insert(key, value);
            }
            if status_map.is_empty() {
                return Err(Error::InvalidActionField {
                    action: action.to_string(),
                    field: "status_map".to_string(),
                    reason: "must not be empty".to_string(),
                });
            }
            Ok(status_map)
        }
        _ => Err(Error::InvalidActionField {
            action: action.to_string(),
            field: "status_map".to_string(),
            reason: "must be a table".to_string(),
        }),
    }
}

/// Parse one `status_map` key: an exit code, as a Lua integer or numeric
/// string, or the `"_"` catch-all.
fn status_map_key(key: Value, action: &str) -> Result<String> {
    match key {
        Value::Integer(code) => Ok(code.to_string()),
        Value::String(key) => {
            let key = key.to_str()?.to_string();
            if key == "_" || key.parse::<i64>().is_ok() {
                Ok(key)
            } else {
                Err(Error::InvalidActionField {
                    action: action.to_string(),
                    field: "status_map".to_string(),
                    reason: "keys must be exit code numbers or \"_\"".to_string(),
                })
            }
        }
        _ => Err(Error::InvalidActionField {
            action: action.to_string(),
            field: "status_map".to_string(),
            reason: "keys must be exit code numbers or \"_\"".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_structured_agent_task_contract() {
        let lua = Lua::new();
        let value = lua
            .load(
                r#"return {
                    action = "agent",
                    role = "developer",
                    prompt = "Full debug prompt",
                    task = {
                        key = "implementation",
                        instructions = "Implement the plan.",
                        recovery_context = "Plan doc: docs/plans/change.md",
                        turn = "Changes needed:\n- fix retry\n\nContext:\nreview",
                    },
                }"#,
            )
            .eval()
            .unwrap();
        let StepAction::Agent(action) = action_from_value(value).unwrap() else {
            panic!("expected agent action")
        };
        let task = action.task.unwrap();
        assert_eq!(task.key, "implementation");
        assert_eq!(task.instructions, "Implement the plan.");
        assert_eq!(task.recovery_context, "Plan doc: docs/plans/change.md");
        assert!(task.turn.contains("Changes needed:"));
    }

    #[test]
    fn legacy_agent_prompt_remains_supported() {
        let lua = Lua::new();
        let value = lua
            .load(
                r#"return {
                    action = "agent",
                    role = "developer",
                    prompt = "Legacy full prompt",
                }"#,
            )
            .eval()
            .unwrap();
        let StepAction::Agent(action) = action_from_value(value).unwrap() else {
            panic!("expected agent action")
        };
        assert_eq!(action.prompt, "Legacy full prompt");
        assert!(action.task.is_none());
    }

    #[test]
    fn agent_action_structured_contract_converts() {
        converts_structured_agent_task_contract();
    }
}
