use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use cowboy_workflow_core::{
    CompiledWorkflow, WorkflowDefinition, WorkflowSourceRef, WorkflowSourceSnapshot,
    validate_definition,
};
use mlua::{Lua, Value};
use parking_lot::Mutex;

use crate::api::{ImportMode, SharedSources, install_import, install_workflow_api};
use crate::convert::workflow_from_value;
use crate::imports::normalize_relative_path;
use crate::sandbox::create_sandbox;
use crate::{Error, Result};

/// Load a workflow source ref from disk and compile it into a workflow definition.
pub fn load(source: &WorkflowSourceRef) -> Result<CompiledWorkflow> {
    let root = source.root.as_ref().ok_or(Error::MissingRoot)?;
    let root = PathBuf::from(root).canonicalize()?;
    let entry = normalize_relative_path(&source.entry)?;
    let sources: SharedSources = Arc::new(Mutex::new(BTreeMap::new()));
    let entry_content = std::fs::read_to_string(root.join(&entry))?;
    sources.lock().insert(entry.clone(), entry_content.clone());

    let lua = setup_lua(ImportMode::Filesystem {
        root: root.clone(),
        sources: sources.clone(),
    })?;
    let workflow = lua.load(&entry_content).set_name(&entry).eval::<Value>()?;
    let definition = workflow_from_value(&lua, workflow, source.id.clone())?;
    validate_definition(&definition)?;
    let source_bundle = WorkflowSourceSnapshot {
        root: Some(root.to_string_lossy().to_string()),
        entry,
        files: sources.lock().clone(),
    };
    Ok(CompiledWorkflow {
        definition,
        source_bundle,
    })
}

/// Compile a snapshotted source bundle into a workflow definition.
pub fn compile_snapshot(bundle: &WorkflowSourceSnapshot) -> Result<WorkflowDefinition> {
    let lua = setup_lua(ImportMode::Snapshot {
        sources: Arc::new(Mutex::new(bundle.files.clone())),
    })?;
    let entry = normalize_relative_path(&bundle.entry)?;
    let source = bundle
        .files
        .get(&entry)
        .ok_or_else(|| Error::MissingEntry(entry.clone()))?;
    let workflow = lua.load(source).set_name(&entry).eval::<Value>()?;
    let definition = workflow_from_value(&lua, workflow, "pending".to_string())?;
    validate_definition(&definition)?;
    Ok(definition)
}

/// Adapter for the core `DefinitionLoader` trait.
#[derive(Debug, Clone, Copy, Default)]
pub struct Loader;

#[async_trait::async_trait]
impl cowboy_workflow_core::DefinitionLoader for Loader {
    async fn load(
        &self,
        source: &WorkflowSourceRef,
    ) -> cowboy_workflow_core::Result<CompiledWorkflow> {
        load(source)
            .map_err(|err| cowboy_workflow_core::WorkflowError::InvalidAction(err.to_string()))
    }
}

/// Build a fresh sandboxed Lua VM for one definition/load or step execution.
///
/// We intentionally recreate the VM instead of preserving Lua state as durable
/// workflow state. A run is recovered from the source snapshot plus persisted
/// `WorkflowRun` data, not from a serialized Lua coroutine/global environment.
/// Fresh setup also prevents step code from accidentally depending on mutable
/// globals left behind by earlier steps in the same process. If performance
/// becomes a problem, the runner can cache a compiled VM for the lifetime of a
/// single process, but restart/resume must still rebuild from the snapshot and
/// Lua globals must remain non-durable.
pub(crate) fn setup_lua(import_mode: ImportMode) -> Result<Lua> {
    let lua = create_sandbox()?;
    install_workflow_api(&lua)?;
    install_import(&lua, import_mode)?;
    Ok(lua)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn snapshot(source: &str) -> WorkflowSourceSnapshot {
        WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".into(),
            files: BTreeMap::from([("main.lua".into(), source.into())]),
        }
    }

    #[test]
    fn compiles_roles_steps_and_transitions() {
        let source = snapshot(
            r#"
            local role = role("developer", "implement things")
            role.language = "rust"
            local a = step("a", { role = role })
            a.kind = "agent"
            a.run = function(ctx) return action.status { status = "success" } end
            local b = step("b")
            b.run = function(ctx) return action.status { status = "success" } end
            a:on("success", b)
            return workflow("wf", a)
            "#,
        );
        let definition = compile_snapshot(&source).unwrap();
        assert_eq!(definition.name, "wf");
        assert_eq!(definition.head, "a");
        assert_eq!(
            definition.roles["developer"].instructions,
            "implement things"
        );
        assert_eq!(definition.roles["developer"].properties["language"], "rust");
        assert_eq!(definition.steps["a"].role.as_deref(), Some("developer"));
        assert_eq!(definition.steps["a"].properties["kind"], "agent");
        assert_eq!(definition.steps["a"].transitions.by_status["success"], "b");
        assert_eq!(definition.roles["developer"].agent, None);
    }

    #[test]
    fn role_table_agent_becomes_typed_metadata() {
        let source = snapshot(
            r#"
            local role = role("developer", { instructions = "implement things", agent = "planner", language = "rust" })
            local step = step("start", { role = role })
            step.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", step)
            "#,
        );

        let definition = compile_snapshot(&source).unwrap();

        assert_eq!(
            definition.roles["developer"].agent.as_deref(),
            Some("planner")
        );
        assert!(
            definition.roles["developer"]
                .properties
                .get("agent")
                .is_none()
        );
        assert_eq!(definition.roles["developer"].properties["language"], "rust");
    }

    #[test]
    fn rejects_blank_role_agent() {
        let source = snapshot(
            r#"
            local role = role("developer", { agent = "   " })
            local step = step("start", { role = role })
            step.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", step)
            "#,
        );

        let err = compile_snapshot(&source).unwrap_err();
        assert!(matches!(err, Error::Lua(_)) || matches!(err, Error::InvalidRoleAgent));
        assert!(err.to_string().contains("role agent"));
    }

    #[test]
    fn rejects_non_string_role_agent_after_mutation() {
        let source = snapshot(
            r#"
            local role = role("developer", "implement things")
            role.agent = 42
            local step = step("start", { role = role })
            step.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", step)
            "#,
        );

        let err = compile_snapshot(&source).unwrap_err();
        assert!(matches!(err, Error::InvalidRoleAgent));
    }

    #[test]
    fn rejects_missing_run_function() {
        let source = snapshot(
            r#"
            local step = step("empty")
            return workflow("wf", step)
            "#,
        );
        let err = compile_snapshot(&source).unwrap_err();
        assert!(matches!(err, Error::MissingRunFunction(step) if step == "empty"));
    }

    #[test]
    fn rejects_role_helper_as_missing_workflow() {
        let source = snapshot(r#"return role("planner", "Plan work")"#);
        let err = compile_snapshot(&source).unwrap_err();
        assert!(matches!(err, Error::MissingWorkflow));
    }

    #[test]
    fn rejects_table_helper_as_missing_workflow() {
        let source = snapshot(r#"return { helper = true }"#);
        let err = compile_snapshot(&source).unwrap_err();
        assert!(matches!(err, Error::MissingWorkflow));
    }

    #[test]
    fn rejects_function_helper_as_missing_workflow() {
        let source = snapshot(r#"return function() end"#);
        let err = compile_snapshot(&source).unwrap_err();
        assert!(matches!(err, Error::MissingWorkflow));
    }

    #[test]
    fn loader_reads_workflow_root_imports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("roles")).unwrap();
        std::fs::write(
            dir.path().join("roles/dev.lua"),
            r#"return role("developer", "imported role")"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.lua"),
            r#"
            local role = require("roles/dev.lua")
            local step = step("implement", { role = role })
            step.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", step)
            "#,
        )
        .unwrap();

        let source = WorkflowSourceRef {
            id: "wf".into(),
            root: Some(dir.path().to_string_lossy().to_string()),
            entry: "main.lua".into(),
            description: None,
        };
        let compiled = load(&source).unwrap();
        assert!(compiled.source_bundle.files.contains_key("main.lua"));
        assert!(compiled.source_bundle.files.contains_key("roles/dev.lua"));
        assert_eq!(
            compiled.definition.roles["developer"].instructions,
            "imported role"
        );
    }

    #[test]
    fn captures_workflow_description_from_config_table() {
        let source = snapshot(
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", start, { description = "does a thing" })
            "#,
        );
        let definition = compile_snapshot(&source).unwrap();
        assert_eq!(definition.description.as_deref(), Some("does a thing"));
    }

    #[test]
    fn captures_workflow_description_from_string_shorthand() {
        let source = snapshot(
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", start, "short desc")
            "#,
        );
        let definition = compile_snapshot(&source).unwrap();
        assert_eq!(definition.description.as_deref(), Some("short desc"));
    }

    #[test]
    fn workflow_without_config_has_no_description() {
        let source = snapshot(
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", start)
            "#,
        );
        let definition = compile_snapshot(&source).unwrap();
        assert_eq!(definition.description, None);
    }

    #[test]
    fn compiles_branched_brainstorm_workflow() {
        let source = snapshot(
            r#"
            local ideator = role("ideator", "Generate creative ideas without judgment")
            local critic = role("critic", "Find themes, promising ideas, and challenges")
            local synthesizer = role("synthesizer", "Distill ideas into actionable proposals")

            local ideate = step("ideator", { role = ideator })
            ideate.run = function(ctx) return action.status { status = "ideate" } end

            local critique = step("critic", { role = critic })
            critique.run = function(ctx) return action.status { status = "feedback" } end

            local synthesize = step("synthesizer", { role = synthesizer })
            synthesize.run = function(ctx) return action.status { status = "success" } end

            ideate:on("ideate", critique)
            ideate:on("done", synthesize)
            critique:on("feedback", ideate)

            return workflow("brainstorm", ideate)
            "#,
        );

        let definition = compile_snapshot(&source).unwrap();

        assert_eq!(definition.name, "brainstorm");
        assert_eq!(definition.head, "ideator");
        assert_eq!(definition.steps.len(), 3);
        assert_eq!(
            definition.steps["ideator"].transitions.by_status["ideate"],
            "critic"
        );
        assert_eq!(
            definition.steps["ideator"].transitions.by_status["done"],
            "synthesizer"
        );
        assert_eq!(
            definition.steps["critic"].transitions.by_status["feedback"],
            "ideator"
        );
    }

    #[test]
    fn compiles_cyclic_debate_workflow() {
        let source = snapshot(
            r#"
            local proponent = role("proponent", "Argue for the proposition")
            local opponent = role("opponent", "Argue against the proposition")
            local host = role("host", "Summarize the debate and deliver a verdict")

            local pro = step("proponent", { role = proponent })
            pro.run = function(ctx) return action.status { status = "speak" } end

            local con = step("opponent", { role = opponent })
            con.run = function(ctx) return action.status { status = "speak" } end

            local summary = step("host", { role = host })
            summary.run = function(ctx) return action.status { status = "success" } end

            pro:on("speak", con)
            pro:on("conceded", summary)
            pro:on("final", con)
            con:on("speak", pro)
            con:on("conceded", summary)
            con:on("final", summary)

            return workflow("debate", pro)
            "#,
        );

        let definition = compile_snapshot(&source).unwrap();

        assert_eq!(definition.name, "debate");
        assert_eq!(
            definition.steps["proponent"].transitions.by_status["speak"],
            "opponent"
        );
        assert_eq!(
            definition.steps["opponent"].transitions.by_status["speak"],
            "proponent"
        );
        assert_eq!(
            definition.steps["proponent"].transitions.by_status["conceded"],
            "host"
        );
        assert_eq!(
            definition.steps["opponent"].transitions.by_status["final"],
            "host"
        );
    }
}
