use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mlua::{Lua, Value};
use parking_lot::Mutex;

use crate::imports::normalize_relative_path;
use crate::{Error, Result};

pub type SharedSources = Arc<Mutex<BTreeMap<String, String>>>;

pub enum ImportMode {
    Filesystem {
        root: PathBuf,
        sources: SharedSources,
    },
    Snapshot {
        sources: SharedSources,
    },
}

pub fn install_workflow_api(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    let registry_roles = lua.create_table()?;
    let registry_steps = lua.create_table()?;
    globals.set("__cowboy_roles", registry_roles.clone())?;
    globals.set("__cowboy_steps", registry_steps.clone())?;

    let step_methods = lua.create_table()?;
    let step_on =
        lua.create_function(|_, (step, status, target): (mlua::Table, String, Value)| {
            if status.trim().is_empty() {
                let id = step
                    .get::<String>("id")
                    .unwrap_or_else(|_| "<unknown>".to_string());
                return Err(mlua::Error::external(Error::InvalidTransitionStatus(id)));
            }
            step.get::<mlua::Table>("transitions")?
                .set(status, target)?;
            Ok(step)
        })?;
    step_methods.set("on", step_on)?;
    let step_meta = lua.create_table()?;
    step_meta.set("__index", step_methods)?;
    globals.set("__cowboy_step_metatable", step_meta.clone())?;

    let role_fn = lua.create_function(move |lua, (id, config): (String, Option<Value>)| {
        if id.trim().is_empty() {
            return Err(mlua::Error::external(Error::InvalidRoleId));
        }
        let role = lua.create_table()?;
        role.set("__cowboy_kind", "role")?;
        role.set("id", id.clone())?;
        match config {
            Some(Value::String(instructions)) => {
                role.set("instructions", instructions.to_str()?.to_string())?;
            }
            Some(Value::Table(table)) => {
                let instructions = table
                    .get::<Option<String>>("instructions")?
                    .unwrap_or_default();
                role.set("instructions", instructions)?;
                match table.get::<Value>("agent")? {
                    Value::Nil => {}
                    Value::String(agent) => {
                        let agent = agent.to_str()?.to_string();
                        if agent.trim().is_empty() {
                            return Err(mlua::Error::external(Error::InvalidRoleAgent));
                        }
                        role.set("agent", agent)?;
                    }
                    _ => return Err(mlua::Error::external(Error::InvalidRoleAgent)),
                }
                copy_config_fields(&role, &table, &["instructions", "agent"])?;
            }
            Some(other) => {
                return Err(mlua::Error::external(Error::UnsupportedValue(format!(
                    "role config: {}",
                    other.type_name()
                ))));
            }
            None => role.set("instructions", "")?,
        }
        lua.globals()
            .get::<mlua::Table>("__cowboy_roles")?
            .set(id, role.clone())?;
        Ok(role)
    })?;
    globals.set("role", role_fn)?;

    let step_fn = lua.create_function(move |lua, (id, config): (String, Option<Value>)| {
        if id.trim().is_empty() {
            return Err(mlua::Error::external(Error::InvalidStepId));
        }
        let step = lua.create_table()?;
        step.set("__cowboy_kind", "step")?;
        step.set("id", id.clone())?;
        step.set("transitions", lua.create_table()?)?;
        if let Some(config) = config {
            match config {
                Value::Table(table) => {
                    if let Some(role) = table.get::<Option<Value>>("role")? {
                        step.set("role", role)?;
                    }
                    if let Some(run) = table.get::<Option<Value>>("run")? {
                        step.set("run", run)?;
                    }
                    copy_config_fields(&step, &table, &["role", "run"])?;
                }
                other => {
                    return Err(mlua::Error::external(Error::UnsupportedValue(format!(
                        "step config: {}",
                        other.type_name()
                    ))));
                }
            }
        }
        let metatable = lua
            .globals()
            .get::<mlua::Table>("__cowboy_step_metatable")?;
        let _ = step.set_metatable(Some(metatable));
        lua.globals()
            .get::<mlua::Table>("__cowboy_steps")?
            .set(id, step.clone())?;
        Ok(step)
    })?;
    globals.set("step", step_fn)?;

    let workflow_fn = lua.create_function(
        |lua, (name, head, config): (String, Value, Option<Value>)| {
            if name.trim().is_empty() {
                return Err(mlua::Error::external(Error::EmptyWorkflowName));
            }
            let workflow = lua.create_table()?;
            workflow.set("__cowboy_kind", "workflow")?;
            workflow.set("name", name)?;
            workflow.set("head", head)?;
            match config {
                Some(Value::String(description)) => {
                    workflow.set("description", description.to_str()?.to_string())?;
                }
                Some(Value::Table(table)) => {
                    copy_config_fields(&workflow, &table, &["name", "head"])?;
                }
                Some(other) => {
                    return Err(mlua::Error::external(Error::UnsupportedValue(format!(
                        "workflow config: {}",
                        other.type_name()
                    ))));
                }
                None => {}
            }
            Ok(workflow)
        },
    )?;
    globals.set("workflow", workflow_fn)?;

    let action = lua.create_table()?;
    for kind in ["agent", "status", "ask_user", "fail"] {
        let function = lua.create_function(move |_, table: mlua::Table| {
            table.set("action", kind)?;
            Ok(table)
        })?;
        action.set(kind, function)?;
    }
    globals.set("action", action)?;
    Ok(())
}

fn copy_config_fields(
    target: &mlua::Table,
    config: &mlua::Table,
    reserved: &[&str],
) -> mlua::Result<()> {
    for pair in config.clone().pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::String(key_str) = &key else {
            continue;
        };
        let key_string = key_str.to_str()?.to_string();
        if reserved.iter().any(|reserved| *reserved == key_string) {
            continue;
        }
        target.set(key, value)?;
    }
    Ok(())
}

pub fn install_import(lua: &Lua, mode: ImportMode) -> Result<()> {
    let import =
        lua.create_function(move |lua, path: String| match &mode {
            ImportMode::Filesystem { root, sources } => {
                let normalized = normalize_relative_path(&path).map_err(mlua::Error::external)?;
                if !sources.lock().contains_key(&normalized) {
                    let content =
                        read_workflow_file(root, &normalized).map_err(mlua::Error::external)?;
                    sources.lock().insert(normalized.clone(), content);
                }
                let content = sources.lock().get(&normalized).cloned().ok_or_else(|| {
                    mlua::Error::external(Error::MissingEntry(normalized.clone()))
                })?;
                lua.load(&content).set_name(&normalized).eval::<Value>()
            }
            ImportMode::Snapshot { sources } => {
                let normalized = normalize_relative_path(&path).map_err(mlua::Error::external)?;
                let content = sources.lock().get(&normalized).cloned().ok_or_else(|| {
                    mlua::Error::external(Error::MissingEntry(normalized.clone()))
                })?;
                lua.load(&content).set_name(&normalized).eval::<Value>()
            }
        })?;
    lua.globals().set("require", import)?;
    Ok(())
}

fn read_workflow_file(root: &Path, normalized: &str) -> Result<String> {
    let canonical_root = root.canonicalize()?;
    let path = canonical_root.join(normalized);
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&canonical_root) {
        return Err(Error::ImportOutsideRoot(normalized.to_string()));
    }
    Ok(std::fs::read_to_string(canonical)?)
}
