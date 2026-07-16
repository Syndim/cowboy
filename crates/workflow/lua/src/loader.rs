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
    use crate::runtime::run_step;
    use cowboy_workflow_core::StepAction;
    use std::collections::BTreeMap;
    fn snapshot(source: &str) -> WorkflowSourceSnapshot {
        WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".into(),
            files: BTreeMap::from([("main.lua".into(), source.into())]),
        }
    }

    fn load_example_compiled_workflow(name: &str) -> CompiledWorkflow {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("examples/workflows");
        let source = WorkflowSourceRef {
            id: name.into(),
            root: Some(root.to_string_lossy().into_owned()),
            entry: format!("workflows/{name}.lua"),
            description: None,
        };

        load(&source).unwrap()
    }

    fn load_example_workflow(name: &str) -> WorkflowDefinition {
        load_example_compiled_workflow(name).definition
    }

    fn assert_expected_role_agents(
        workflow_name: &str,
        definition: &WorkflowDefinition,
        expected_agents: &[(&str, &str)],
    ) {
        for (role_name, expected_agent) in expected_agents {
            let role = definition.roles.get(*role_name).unwrap_or_else(|| {
                panic!("{workflow_name} workflow should include {role_name} role")
            });

            assert_eq!(
                role.agent.as_deref(),
                Some(*expected_agent),
                "{workflow_name} {role_name} role should use the expected agent"
            );
            assert!(
                !role.instructions.trim().is_empty(),
                "{workflow_name} {role_name} role should have instructions"
            );
        }
    }

    fn assert_review_transition(
        workflow_name: &str,
        definition: &WorkflowDefinition,
        status: &str,
        expected_step: &str,
    ) {
        let review = definition
            .steps
            .get("review")
            .unwrap_or_else(|| panic!("{workflow_name} workflow should include review step"));
        assert_eq!(
            review.transitions.by_status.get(status).map(String::as_str),
            Some(expected_step),
            "{workflow_name} review status {status} should route to {expected_step}"
        );
    }

    fn assert_step_transition(
        workflow_name: &str,
        definition: &WorkflowDefinition,
        step_id: &str,
        status: &str,
        expected_step: &str,
    ) {
        let step = definition
            .steps
            .get(step_id)
            .unwrap_or_else(|| panic!("{workflow_name} workflow should include {step_id} step"));
        assert_eq!(
            step.transitions.by_status.get(status).map(String::as_str),
            Some(expected_step),
            "{workflow_name} {step_id} status {status} should route to {expected_step}"
        );
    }

    fn result_feedback_review_step<'a>(
        workflow_name: &str,
        definition: &'a WorkflowDefinition,
    ) -> &'a str {
        let confirm_result_answer = definition
            .steps
            .get("confirm_result_answer")
            .unwrap_or_else(|| {
                panic!("{workflow_name} workflow should include confirm_result_answer step")
            });

        assert_eq!(
            confirm_result_answer
                .transitions
                .by_status
                .get("confirmed")
                .map(String::as_str),
            Some("commit"),
            "{workflow_name} confirm_result_answer should still route confirmed results to commit"
        );

        let review_step = confirm_result_answer
            .transitions
            .by_status
            .get("changes_requested")
            .unwrap_or_else(|| {
                panic!(
                    "{workflow_name} confirm_result_answer should route user change requests through a reviewer step"
                )
            });
        assert_ne!(
            review_step, "revise",
            "{workflow_name} confirm_result_answer changes_requested should not bypass reviewer triage"
        );
        assert!(
            definition.steps.contains_key(review_step),
            "{workflow_name} confirm_result_answer changes_requested should target an existing step; got {review_step}"
        );

        review_step
    }

    fn assert_result_feedback_prompt_guidance(prompt: &str, workflow_name: &str) {
        assert_prompt_contains(prompt, "User result feedback:", workflow_name);
        assert_prompt_contains(prompt, "Step: confirm_result_answer", workflow_name);
        assert_prompt_contains(prompt, "Status: changes_requested", workflow_name);
        assert_prompt_contains(
            prompt,
            "Feedback: User says the implementation missed the CLI flag",
            workflow_name,
        );
        assert_prompt_contains(
            prompt,
            "Work dir: docs/plans/fix_result_feedback_gate",
            workflow_name,
        );
        assert_prompt_contains(
            prompt,
            "Plan doc: docs/plans/fix_result_feedback_gate/plan.md",
            workflow_name,
        );
        assert_prompt_contains(
            prompt,
            "RCA doc: docs/plans/fix_result_feedback_gate/rca.md",
            workflow_name,
        );
        assert_prompt_contains(
            prompt,
            "Repro test: crates/workflow/lua/src/loader.rs::examples_workflows_review_result_feedback_agent_triages_user_feedback",
            workflow_name,
        );

        let changes_guidance = prompt_window_after(prompt, "\"changes_requested\"", workflow_name);
        assert!(
            changes_guidance.contains("implementation"),
            "{workflow_name} result-feedback prompt should reserve changes_requested for implementation feedback\nGuidance:\n{changes_guidance}"
        );

        let replan_guidance = prompt_window_after(prompt, "\"replan_requested\"", workflow_name);
        assert!(
            replan_guidance.contains("plan")
                || replan_guidance.contains("scope")
                || replan_guidance.contains("requirements"),
            "{workflow_name} result-feedback prompt should reserve replan_requested for plan-level feedback\nGuidance:\n{replan_guidance}"
        );

        let preserve_start = prompt.find("Preserve").or_else(|| prompt.find("preserve"));
        let preserve_start = preserve_start.unwrap_or_else(|| {
            panic!(
                "{workflow_name} result-feedback prompt should tell the reviewer to preserve context fields"
            )
        });
        let preserve_guidance = &prompt[preserve_start..];
        assert!(
            preserve_guidance.contains("exactly") && preserve_guidance.contains("output fields"),
            "{workflow_name} result-feedback prompt preserve guidance should require exact output-field preservation\nGuidance:\n{preserve_guidance}"
        );
        for field_name in ["Work dir", "Plan doc", "RCA doc", "Repro test"] {
            assert!(
                preserve_guidance.contains(field_name),
                "{workflow_name} result-feedback prompt preserve guidance should mention {field_name:?}\nGuidance:\n{preserve_guidance}"
            );
        }
    }

    fn assert_declares_status(
        output_statuses: &[String],
        expected_status: &str,
        workflow_name: &str,
    ) {
        assert!(
            output_statuses
                .iter()
                .any(|candidate| candidate == expected_status),
            "{workflow_name} result-feedback review output should include {expected_status:?}; got {output_statuses:?}"
        );
    }
    fn assert_prompt_contains(prompt: &str, needle: &str, workflow_name: &str) {
        assert!(
            prompt.contains(needle),
            "{workflow_name} review prompt should contain {needle:?}\nPrompt:\n{prompt}"
        );
    }

    fn prompt_window_after<'a>(prompt: &'a str, needle: &str, workflow_name: &str) -> &'a str {
        let start = prompt
            .find(needle)
            .unwrap_or_else(|| panic!("{workflow_name} review prompt should contain {needle:?}"));
        let relative_end = prompt[start..]
            .char_indices()
            .nth(260)
            .map(|(idx, _)| idx)
            .unwrap_or_else(|| prompt[start..].len());
        &prompt[start..start + relative_end]
    }

    fn assert_review_prompt_guidance(prompt: &str, workflow_name: &str) {
        let changes_guidance = prompt_window_after(prompt, "\"changes_requested\"", workflow_name);
        assert!(
            changes_guidance.contains("implementation") && changes_guidance.contains("fix"),
            "{workflow_name} review prompt should reserve changes_requested for implementation fixes\nGuidance:\n{changes_guidance}"
        );

        let replan_guidance = prompt_window_after(prompt, "\"replan_requested\"", workflow_name);
        assert!(
            replan_guidance.contains("plan")
                && [
                    "incomplete",
                    "unsafe",
                    "incorrectly scoped",
                    "unverifiable",
                    "unsound",
                    "not solid",
                ]
                .iter()
                .any(|reason| replan_guidance.contains(reason)),
            "{workflow_name} review prompt should reserve replan_requested for plan-level rejection\nGuidance:\n{replan_guidance}"
        );

        let preserve_start = prompt.find("Preserve").or_else(|| prompt.find("preserve"));
        let preserve_start = preserve_start.unwrap_or_else(|| {
            panic!(
                "{workflow_name} review prompt should tell the reviewer to preserve context fields"
            )
        });
        let preserve_guidance = &prompt[preserve_start..];
        for field_name in ["Work dir", "Plan doc", "RCA doc", "Repro test"] {
            assert!(
                preserve_guidance.contains(field_name),
                "{workflow_name} review prompt preserve guidance should mention {field_name:?}\nGuidance:\n{preserve_guidance}"
            );
        }
    }

    #[test]
    fn examples_workflows_capture_cumulative_confirmation_feedback() {
        fn assert_common_context(fields: &serde_json::Value) {
            assert_eq!(fields["goal"], "Keep command behavior stable");
            assert_eq!(fields["validation"], "cargo test -p cowboy");
            assert_eq!(fields["work_dir"], "docs/plans/example");
            assert_eq!(fields["plan_doc"], "docs/plans/example/plan.md");
            assert_eq!(fields["rca_doc"], "docs/plans/example/rca.md");
            assert_eq!(
                fields["repro_test"],
                "crates/workflow/lua/src/loader.rs::confirmation_repro"
            );
        }

        fn assert_rca_context(fields: &serde_json::Value) {
            assert_eq!(fields["summary"], "Race reproduced");
            assert_eq!(fields["work_dir"], "docs/plans/example");
            assert_eq!(fields["rca_doc"], "docs/plans/example/rca.md");
            assert_eq!(
                fields["repro_test"],
                "crates/workflow/lua/src/loader.rs::confirmation_repro"
            );
            assert_eq!(
                fields["files"],
                serde_json::json!(["docs/plans/example/rca.md", "src/lib.rs"])
            );
            assert_eq!(fields["command"], "cargo test confirmation_repro");
            assert_eq!(
                fields["commands"],
                serde_json::json!(["cargo test confirmation_repro", "cargo test"])
            );
            assert_eq!(fields["failure"], "assertion failed");
            assert_eq!(
                fields["failures"],
                serde_json::json!(["assertion failed", "exit status 101"])
            );
        }

        let existing = serde_json::json!([
            "Plan confirmation: Preserve the public API",
            "Result confirmation: Include the TUI path"
        ]);
        let feature = load_example_compiled_workflow("feature");
        let result = run_step(
            &feature.source_bundle,
            "confirm_plan_answer",
            serde_json::json!({
                "prev": {
                    "step": "confirm_plan",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "answer": "Keep the command syntax stable",
                        "plan": "Reviewed plan body",
                        "user_feedback": existing,
                        "goal": "Keep command behavior stable",
                        "validation": "cargo test -p cowboy",
                        "work_dir": "docs/plans/example",
                        "plan_doc": "docs/plans/example/plan.md",
                        "rca_doc": "docs/plans/example/rca.md",
                        "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro"
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("plan confirmation should capture feedback")
        };

        assert_eq!(action.status, "changes_requested");
        assert_eq!(action.fields["feedback"], "Keep the command syntax stable");
        assert_eq!(
            action.fields["user_feedback"],
            serde_json::json!([
                "Plan confirmation: Preserve the public API",
                "Result confirmation: Include the TUI path",
                "Plan confirmation: Keep the command syntax stable"
            ])
        );
        assert_common_context(&action.fields);

        let result = run_step(
            &feature.source_bundle,
            "confirm_plan_answer",
            serde_json::json!({
                "prev": {
                    "step": "confirm_plan",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "answer": "yes",
                        "plan": "Reviewed plan body",
                        "user_feedback": existing,
                        "goal": "Keep command behavior stable",
                        "validation": "cargo test -p cowboy",
                        "work_dir": "docs/plans/example",
                        "plan_doc": "docs/plans/example/plan.md",
                        "rca_doc": "docs/plans/example/rca.md",
                        "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro"
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("plan confirmation should record approval")
        };

        assert_eq!(action.status, "confirmed");
        assert_eq!(action.fields["plan"], "Reviewed plan body");
        assert_eq!(action.fields["user_feedback"], existing);
        assert_common_context(&action.fields);

        let result = run_step(
            &feature.source_bundle,
            "confirm_result_answer",
            serde_json::json!({
                "prev": {
                    "step": "confirm_result",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "answer": "Also update the interactive help",
                        "user_feedback": existing,
                        "goal": "Keep command behavior stable",
                        "validation": "cargo test -p cowboy",
                        "work_dir": "docs/plans/example",
                        "plan_doc": "docs/plans/example/plan.md",
                        "rca_doc": "docs/plans/example/rca.md",
                        "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro"
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("result confirmation should capture feedback")
        };

        assert_eq!(action.status, "changes_requested");
        assert_eq!(
            action.fields["feedback"],
            "Also update the interactive help"
        );
        assert_eq!(
            action.fields["user_feedback"],
            serde_json::json!([
                "Plan confirmation: Preserve the public API",
                "Result confirmation: Include the TUI path",
                "Result confirmation: Also update the interactive help"
            ])
        );
        assert_common_context(&action.fields);

        let result = run_step(
            &feature.source_bundle,
            "confirm_result_answer",
            serde_json::json!({
                "prev": {
                    "step": "confirm_result",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "answer": "approved",
                        "user_feedback": existing,
                        "goal": "Keep command behavior stable",
                        "validation": "cargo test -p cowboy",
                        "work_dir": "docs/plans/example",
                        "plan_doc": "docs/plans/example/plan.md",
                        "rca_doc": "docs/plans/example/rca.md",
                        "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro"
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("result confirmation should record approval")
        };

        assert_eq!(action.status, "confirmed");
        assert_eq!(action.fields["user_feedback"], existing);
        assert_common_context(&action.fields);

        let bugfix = load_example_compiled_workflow("bugfix");
        let result = run_step(
            &bugfix.source_bundle,
            "confirm_rca_answer",
            serde_json::json!({
                "prev": {
                    "step": "confirm_rca",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "answer": "Explain why the race is deterministic",
                        "user_feedback": existing,
                        "summary": "Race reproduced",
                        "work_dir": "docs/plans/example",
                        "rca_doc": "docs/plans/example/rca.md",
                        "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro",
                        "files": ["docs/plans/example/rca.md", "src/lib.rs"],
                        "command": "cargo test confirmation_repro",
                        "commands": ["cargo test confirmation_repro", "cargo test"],
                        "failure": "assertion failed",
                        "failures": ["assertion failed", "exit status 101"]
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("RCA confirmation should capture feedback")
        };

        assert_eq!(action.status, "changes_requested");
        assert_eq!(
            action.fields["feedback"],
            "Explain why the race is deterministic"
        );
        assert_eq!(
            action.fields["user_feedback"],
            serde_json::json!([
                "Plan confirmation: Preserve the public API",
                "Result confirmation: Include the TUI path",
                "RCA confirmation: Explain why the race is deterministic"
            ])
        );
        assert_rca_context(&action.fields);

        let result = run_step(
            &bugfix.source_bundle,
            "confirm_rca_answer",
            serde_json::json!({
                "prev": {
                    "step": "confirm_rca",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "answer": "y",
                        "user_feedback": existing,
                        "summary": "Race reproduced",
                        "work_dir": "docs/plans/example",
                        "rca_doc": "docs/plans/example/rca.md",
                        "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro",
                        "files": ["docs/plans/example/rca.md", "src/lib.rs"],
                        "command": "cargo test confirmation_repro",
                        "commands": ["cargo test confirmation_repro", "cargo test"],
                        "failure": "assertion failed",
                        "failures": ["assertion failed", "exit status 101"]
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("RCA confirmation should record approval")
        };

        assert_eq!(action.status, "confirmed");
        assert_eq!(action.fields["user_feedback"], existing);
        assert_rca_context(&action.fields);
    }

    #[test]
    fn examples_workflows_agent_steps_preserve_and_render_user_feedback() {
        let cases: [(&str, &[&str]); 3] = [
            (
                "feature",
                &[
                    "plan",
                    "review_blocker",
                    "review_plan",
                    "implement",
                    "test",
                    "review",
                    "review_result_feedback",
                    "revise",
                    "commit",
                ],
            ),
            (
                "bugfix",
                &[
                    "investigate",
                    "review_rca",
                    "plan",
                    "review_blocker",
                    "review_plan",
                    "implement",
                    "test",
                    "review",
                    "review_result_feedback",
                    "revise",
                    "commit",
                ],
            ),
            (
                "dev-loop",
                &[
                    "plan",
                    "review_blocker",
                    "review_plan",
                    "implement",
                    "test",
                    "validate",
                    "review",
                    "review_result_feedback",
                    "revise",
                    "commit",
                ],
            ),
        ];

        for (workflow_name, step_ids) in cases {
            let compiled = load_example_compiled_workflow(workflow_name);
            for step_id in step_ids {
                let result = run_step(
                    &compiled.source_bundle,
                    step_id,
                    serde_json::json!({
                        "request": "Preserve cumulative feedback",
                        "prev": {
                            "step": "handoff",
                            "status": "ready",
                            "fields": {
                                "user_feedback": [
                                    "Plan confirmation: Preserve the public API",
                                    "Result confirmation: Include the TUI path"
                                ],
                                "goal": "Preserve cumulative feedback",
                                "validation": "cargo test",
                                "plan_doc": "docs/plans/example/plan.md",
                                "validation_doc": "docs/plans/example/validation.md",
                                "work_dir": "docs/plans/example",
                                "rca_doc": "docs/plans/example/rca.md",
                                "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro",
                                "commands": ["cargo test"],
                                "files": ["docs/plans/example/plan.md", "docs/plans/example/validation.md"]
                            }
                        }
                    }),
                )
                .unwrap();

                let StepAction::Agent(action) = result.action else {
                    panic!("{workflow_name} {step_id} should request an agent action")
                };

                let output = action.output.unwrap_or_else(|| {
                    panic!("{workflow_name} {step_id} should declare agent output")
                });

                assert_eq!(
                    output.fields["user_feedback"], "array",
                    "{workflow_name} {step_id} should declare user_feedback"
                );
                if !matches!(*step_id, "investigate" | "review_rca") {
                    assert_eq!(
                        output.fields["validation_doc"], "string",
                        "{workflow_name} {step_id} should declare validation_doc"
                    );
                }

                assert!(
                    action
                        .prompt
                        .contains("Validation doc: docs/plans/example/validation.md"),
                    "{workflow_name} {step_id} should render validation_doc"
                );
                if *step_id == "commit" {
                    for field in [
                        "work_dir",
                        "plan_doc",
                        "validation_doc",
                        "rca_doc",
                        "repro_test",
                    ] {
                        assert_eq!(
                            output.fields[field], "string",
                            "commit should declare {field}"
                        );
                    }

                    for label in [
                        "Work dir",
                        "Plan doc",
                        "Validation doc",
                        "RCA doc",
                        "Repro test",
                    ] {
                        assert!(
                            action.prompt.contains(&format!("`{label}: ...`")),
                            "commit should require exact preservation of {label}"
                        );
                    }
                }

                assert!(
                    action.prompt.contains(
                        "Preserve `user_feedback` exactly in output fields when present."
                    ),
                    "{workflow_name} {step_id} should require exact feedback preservation"
                );
                let first = action
                    .prompt
                    .find("- Plan confirmation: Preserve the public API")
                    .unwrap_or_else(|| {
                        panic!("{workflow_name} {step_id} should render plan feedback")
                    });

                let second = action
                    .prompt
                    .find("- Result confirmation: Include the TUI path")
                    .unwrap_or_else(|| {
                        panic!("{workflow_name} {step_id} should render result feedback")
                    });

                assert!(
                    first < second,
                    "{workflow_name} {step_id} should preserve feedback order"
                );

                if matches!(
                    *step_id,
                    "review_rca" | "review_plan" | "review" | "review_result_feedback"
                ) {
                    assert!(
                        action.prompt.contains(
                            "Evaluate the revised work against the complete user feedback history"
                        ),
                        "{workflow_name} {step_id} should review against user feedback"
                    );
                }
            }
        }
    }

    #[test]
    fn examples_workflows_detours_preserve_user_feedback_without_relabeling_input() {
        fn assert_artifact_context(fields: &serde_json::Value) {
            assert_eq!(fields["goal"], "Preserve clarification context");
            assert_eq!(
                fields["validation"],
                "cargo test -p cowboy-workflow-lua clarification_context"
            );
            assert_eq!(fields["work_dir"], "docs/plans/example");
            assert_eq!(fields["plan_doc"], "docs/plans/example/plan.md");
            assert_eq!(fields["rca_doc"], "docs/plans/example/rca.md");
            assert_eq!(
                fields["repro_test"],
                "crates/workflow/lua/src/loader.rs::clarification_repro"
            );
        }

        let compiled = load_example_compiled_workflow("feature");
        let existing = serde_json::json!([
            "Plan confirmation: Preserve the public API",
            "Result confirmation: Include the TUI path"
        ]);
        let result = run_step(
            &compiled.source_bundle,
            "clarify",
            serde_json::json!({
                "steps_executed": 3,
                "prev": {
                    "step": "plan",
                    "status": "unclear",
                    "fields": {
                        "user_feedback": existing,
                        "goal": "Preserve clarification context",
                        "validation": "cargo test -p cowboy-workflow-lua clarification_context",
                        "work_dir": "docs/plans/example",
                        "plan_doc": "docs/plans/example/plan.md",
                        "rca_doc": "docs/plans/example/rca.md",
                        "repro_test": "crates/workflow/lua/src/loader.rs::clarification_repro"
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::AskUser(action) = result.action else {
            panic!("clarify should ask the user for context")
        };

        assert_eq!(action.fields["user_feedback"], existing);
        assert_artifact_context(&action.fields);
        let mut answered_fields = action.fields.clone();
        answered_fields["answer"] = serde_json::json!("The entrypoint is the TUI composer");

        let result = run_step(
            &compiled.source_bundle,
            "clarify_answer",
            serde_json::json!({
                "prev": {
                    "step": "clarify",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": answered_fields
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("clarify_answer should record the clarification")
        };

        assert_eq!(action.fields["user_feedback"], existing);
        assert_artifact_context(&action.fields);
        assert_eq!(
            action.fields["clarification"],
            "The entrypoint is the TUI composer"
        );

        let result = run_step(
            &compiled.source_bundle,
            "triage_blocked",
            serde_json::json!({
                "prev": {
                    "step": "blocked_answer",
                    "status": "triaged",
                    "fields": {
                        "blocked_response": "Credentials are available; continue implementation",
                        "blocked_from_step": "implement",
                        "user_feedback": existing
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Status(action) = result.action else {
            panic!("triage_blocked should route the recovered workflow")
        };

        assert_eq!(action.status, "implement");
        assert_eq!(action.fields["user_feedback"], existing);
        assert_eq!(
            action.fields["feedback"],
            "Credentials are available; continue implementation"
        );
    }

    #[test]
    fn examples_workflows_review_replans_when_approved_plan_is_unsound() {
        for workflow_name in ["feature", "bugfix"] {
            let definition = load_example_workflow(workflow_name);

            assert_review_transition(workflow_name, &definition, "approved", "confirm_result");
            assert_review_transition(workflow_name, &definition, "changes_requested", "revise");
            assert_review_transition(workflow_name, &definition, "replan_requested", "plan");
        }
    }

    #[test]
    fn examples_workflows_review_agent_output_distinguishes_replanning() {
        for workflow_name in ["feature", "bugfix"] {
            let compiled = load_example_compiled_workflow(workflow_name);
            let result = run_step(
                &compiled.source_bundle,
                "review",
                serde_json::json!({
                    "request": "Fix approval routing after implementation review",
                    "prev": {
                        "step": "test",
                        "status": "passed",
                        "fields": {
                            "summary": "Focused tests passed",
                            "work_dir": "docs/plans/fix_approval_routing",
                            "plan_doc": "docs/plans/fix_approval_routing/plan.md",
                            "rca_doc": "docs/plans/fix_approval_routing/rca.md",
                            "repro_test": "crates/workflow/lua/src/loader.rs::examples_workflows_review_agent_output_distinguishes_replanning",
                            "commands": ["cargo test -p cowboy-workflow-lua examples_workflows"]
                        }
                    }
                }),
            )
            .unwrap();

            let StepAction::Agent(action) = result.action else {
                panic!("{workflow_name} review step should request an agent action")
            };
            assert_eq!(action.role, "reviewer");

            let output = action
                .output
                .as_ref()
                .unwrap_or_else(|| panic!("{workflow_name} review action should declare output"));
            for status in ["approved", "changes_requested", "replan_requested"] {
                assert!(
                    output.statuses.iter().any(|candidate| candidate == status),
                    "{workflow_name} review output should include {status:?}; got {:?}",
                    output.statuses
                );
            }

            assert_review_prompt_guidance(&action.prompt, workflow_name);
            assert_prompt_contains(
                &action.prompt,
                "Plan doc: docs/plans/fix_approval_routing/plan.md",
                workflow_name,
            );
            assert_prompt_contains(
                &action.prompt,
                "Work dir: docs/plans/fix_approval_routing",
                workflow_name,
            );
            assert_prompt_contains(
                &action.prompt,
                "RCA doc: docs/plans/fix_approval_routing/rca.md",
                workflow_name,
            );
            assert_prompt_contains(
                &action.prompt,
                "Repro test: crates/workflow/lua/src/loader.rs::examples_workflows_review_agent_output_distinguishes_replanning",
                workflow_name,
            );
        }
    }

    #[test]
    fn examples_workflows_gate_result_confirmation_feedback_through_reviewer() {
        for workflow_name in ["feature", "bugfix"] {
            let definition = load_example_workflow(workflow_name);

            result_feedback_review_step(workflow_name, &definition);
        }
    }

    #[test]
    fn examples_workflows_review_result_feedback_routes_to_revise_or_plan() {
        for workflow_name in ["feature", "bugfix"] {
            let definition = load_example_workflow(workflow_name);
            let review_step = result_feedback_review_step(workflow_name, &definition);

            assert_step_transition(
                workflow_name,
                &definition,
                review_step,
                "changes_requested",
                "revise",
            );
            assert_step_transition(
                workflow_name,
                &definition,
                review_step,
                "replan_requested",
                "plan",
            );
        }
    }

    #[test]
    fn examples_workflows_review_result_feedback_agent_triages_user_feedback() {
        for workflow_name in ["feature", "bugfix"] {
            let compiled = load_example_compiled_workflow(workflow_name);
            let review_step =
                result_feedback_review_step(workflow_name, &compiled.definition).to_string();
            let result = run_step(
                &compiled.source_bundle,
                &review_step,
                serde_json::json!({
                    "request": "Finish result feedback gate coverage",
                    "prev": {
                        "step": "confirm_result_answer",
                        "status": "changes_requested",
                        "fields": {
                            "feedback": "User says the implementation missed the CLI flag",
                            "work_dir": "docs/plans/fix_result_feedback_gate",
                            "plan_doc": "docs/plans/fix_result_feedback_gate/plan.md",
                            "rca_doc": "docs/plans/fix_result_feedback_gate/rca.md",
                            "repro_test": "crates/workflow/lua/src/loader.rs::examples_workflows_review_result_feedback_agent_triages_user_feedback"
                        }
                    }
                }),
            )
            .unwrap();

            let StepAction::Agent(action) = result.action else {
                panic!("{workflow_name} result-feedback review step should request an agent action")
            };
            assert_eq!(action.role, "reviewer");

            let output = action.output.as_ref().unwrap_or_else(|| {
                panic!("{workflow_name} result-feedback review action should declare output")
            });
            assert_declares_status(&output.statuses, "changes_requested", workflow_name);
            assert_declares_status(&output.statuses, "replan_requested", workflow_name);
            assert!(
                !output
                    .statuses
                    .iter()
                    .any(|candidate| candidate == "confirmed" || candidate == "approved"),
                "{workflow_name} result-feedback review should not approve already-rejected user feedback; got {:?}",
                output.statuses
            );

            assert_result_feedback_prompt_guidance(&action.prompt, workflow_name);
        }
    }

    #[test]
    fn examples_workflows_route_all_blockers_through_reviewer() {
        let cases: [(&str, &[&str]); 3] = [
            ("feature", &["implement", "test", "revise", "commit"]),
            (
                "bugfix",
                &["investigate", "implement", "test", "revise", "commit"],
            ),
            (
                "dev-loop",
                &["implement", "test", "validate", "revise", "commit"],
            ),
        ];

        for (workflow_name, blocked_steps) in cases {
            let definition = load_example_workflow(workflow_name);
            assert_expected_role_agents(
                workflow_name,
                &definition,
                &[("blocker-reviewer", "reviewer")],
            );

            for step_id in blocked_steps {
                assert_step_transition(
                    workflow_name,
                    &definition,
                    step_id,
                    "blocked",
                    "capture_blocker",
                );
            }

            assert_step_transition(
                workflow_name,
                &definition,
                "capture_blocker",
                "captured",
                "review_blocker",
            );
            assert_step_transition(
                workflow_name,
                &definition,
                "review_blocker",
                "recoverable",
                "triage_blocked",
            );
            assert_step_transition(
                workflow_name,
                &definition,
                "review_blocker",
                "user_required",
                "blocked",
            );
            assert_step_transition(
                workflow_name,
                &definition,
                "blocked",
                "answered",
                "blocked_answer",
            );
            assert_step_transition(
                workflow_name,
                &definition,
                "blocked_answer",
                "triaged",
                "triage_blocked",
            );

            for step_id in blocked_steps {
                assert_step_transition(
                    workflow_name,
                    &definition,
                    "triage_blocked",
                    step_id,
                    step_id,
                );
            }
        }
    }

    #[test]
    fn examples_workflows_capture_review_and_triage_named_blockers() {
        let compiled = load_example_compiled_workflow("dev-loop");
        let capture_result = run_step(
            &compiled.source_bundle,
            "capture_blocker",
            serde_json::json!({
                "request": "Add safe cache invalidation",
                "prev": {
                    "step": "test",
                    "action": "agent",
                    "status": "blocked",
                    "fields": {
                        "goal": "Add safe cache invalidation",
                        "validation": "cargo test -p cache",
                        "work_dir": "docs/plans/cache",
                        "plan_doc": "docs/plans/cache/plan.md",
                        "validation_doc": "docs/plans/cache/validation.md",
                        "rca_doc": "docs/plans/cache/rca.md",
                        "files": ["docs/plans/cache/plan.md", "docs/plans/cache/validation.md", "src/cache.rs"]
                    },
                    "body": "The local fixture database is missing"
                }
            }),
        )
        .unwrap();
        let StepAction::Status(captured) = capture_result.action else {
            panic!("capture_blocker should return a status action")
        };

        assert_eq!(captured.status, "captured");
        assert_eq!(
            captured.fields["blocker_statement"],
            "The local fixture database is missing"
        );
        assert_eq!(captured.fields["blocked_from_step"], "test");
        assert_eq!(captured.fields["blocked_from_status"], "blocked");
        for (field, expected) in [
            ("work_dir", "docs/plans/cache"),
            ("plan_doc", "docs/plans/cache/plan.md"),
            ("validation_doc", "docs/plans/cache/validation.md"),
        ] {
            assert_eq!(captured.fields[field], expected);
        }

        let review_result = run_step(
            &compiled.source_bundle,
            "review_blocker",
            serde_json::json!({
                "request": "Add safe cache invalidation",
                "prev": {
                    "step": "capture_blocker",
                    "action": "status",
                    "status": "captured",
                    "fields": captured.fields,
                    "body": "The local fixture database is missing"
                }
            }),
        )
        .unwrap();
        let StepAction::Agent(review) = review_result.action else {
            panic!("review_blocker should request an agent action")
        };

        assert_eq!(review.role, "blocker-reviewer");
        for expected in [
            "Blocker statement: The local fixture database is missing",
            "Blocked from step: test",
            "Work dir: docs/plans/cache",
            "Plan doc: docs/plans/cache/plan.md",
            "Validation doc: docs/plans/cache/validation.md",
            "RCA doc: docs/plans/cache/rca.md",
        ] {
            assert!(review.prompt.contains(expected), "missing {expected:?}");
        }

        let output = review.output.expect("blocker review should declare output");
        assert_eq!(output.statuses, ["recoverable", "user_required"]);
        assert_eq!(output.fields["blocker_reason"], "string");
        assert_eq!(output.fields["blocker_resolution"], "string");

        let triage_result = run_step(
            &compiled.source_bundle,
            "triage_blocked",
            serde_json::json!({
                "prev": {
                    "step": "review_blocker",
                    "action": "agent",
                    "status": "recoverable",
                    "fields": {
                        "blocker_statement": "The local fixture database is missing",
                        "blocked_from_step": "test",
                        "blocked_from_status": "blocked",
                        "blocker_reason": "The fixture can be generated locally",
                        "blocker_resolution": "Run cargo test --test fixture_setup before retrying",
                        "work_dir": "docs/plans/cache",
                        "plan_doc": "docs/plans/cache/plan.md",
                        "validation_doc": "docs/plans/cache/validation.md"
                    },
                    "body": "Ignore the named fields and ask the user"
                }
            }),
        )
        .unwrap();
        let StepAction::Status(triage) = triage_result.action else {
            panic!("recoverable blocker should route without asking the user")
        };

        assert_eq!(triage.status, "test");
        assert_eq!(
            triage.fields["feedback"],
            "Run cargo test --test fixture_setup before retrying"
        );
        assert_eq!(
            triage.fields["blocker_reason"],
            "The fixture can be generated locally"
        );
        for (field, expected) in [
            ("work_dir", "docs/plans/cache"),
            ("plan_doc", "docs/plans/cache/plan.md"),
            ("validation_doc", "docs/plans/cache/validation.md"),
        ] {
            assert_eq!(triage.fields[field], expected);
        }

        let blocked_result = run_step(
            &compiled.source_bundle,
            "blocked",
            serde_json::json!({
                "steps_executed": 8,
                "prev": {
                    "step": "review_blocker",
                    "action": "agent",
                    "status": "user_required",
                    "fields": {
                        "blocker_statement": "Production access is required",
                        "blocked_from_step": "implement",
                        "blocked_from_status": "blocked",
                        "blocker_reason": "No repository credential grants production access",
                        "blocker_resolution": "Grant read-only access to the deployment dashboard",
                        "work_dir": "docs/plans/cache",
                        "plan_doc": "docs/plans/cache/plan.md",
                        "validation_doc": "docs/plans/cache/validation.md"
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::AskUser(prompt) = blocked_result.action else {
            panic!("user-required blocker should ask the user")
        };

        for expected in [
            "Production access is required",
            "No repository credential grants production access",
            "Grant read-only access to the deployment dashboard",
        ] {
            assert!(prompt.message.contains(expected), "missing {expected:?}");
        }

        let mut answered_fields = prompt.fields;
        answered_fields["answer"] = serde_json::json!("Access granted; retry the original step");
        let answer_result = run_step(
            &compiled.source_bundle,
            "blocked_answer",
            serde_json::json!({
                "prev": {
                    "step": "blocked",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": answered_fields
                }
            }),
        )
        .unwrap();
        let StepAction::Status(answered) = answer_result.action else {
            panic!("blocked answer should record the user response")
        };

        assert_eq!(answered.fields["blocked_from_step"], "implement");
        for (field, expected) in [
            ("work_dir", "docs/plans/cache"),
            ("plan_doc", "docs/plans/cache/plan.md"),
            ("validation_doc", "docs/plans/cache/validation.md"),
        ] {
            assert_eq!(answered.fields[field], expected);
        }

        let resume_result = run_step(
            &compiled.source_bundle,
            "triage_blocked",
            serde_json::json!({
                "prev": {
                    "step": "blocked_answer",
                    "action": "status",
                    "status": "triaged",
                    "fields": answered.fields
                }
            }),
        )
        .unwrap();
        let StepAction::Status(resume) = resume_result.action else {
            panic!("answered blocker should return through triage")
        };

        assert_eq!(resume.status, "implement");
        for (field, expected) in [
            ("work_dir", "docs/plans/cache"),
            ("plan_doc", "docs/plans/cache/plan.md"),
            ("validation_doc", "docs/plans/cache/validation.md"),
        ] {
            assert_eq!(resume.fields[field], expected);
        }
    }

    #[test]
    fn examples_workflows_clearance_keywords_do_not_reroute() {
        let compiled = load_example_compiled_workflow("dev-loop");
        let cases = [
            ("test", "the access code is now available", "test"),
            ("implement", "the test account is ready", "implement"),
            ("test", "the commit token is now available", "test"),
            (
                "implement",
                "the validation environment is ready",
                "implement",
            ),
            ("test", "/route plan", "plan"),
        ];

        for (blocked_from_step, response, expected_step) in cases {
            let result = run_step(
                &compiled.source_bundle,
                "triage_blocked",
                serde_json::json!({
                    "prev": {
                        "step": "blocked_answer",
                        "action": "status",
                        "status": "triaged",
                        "fields": {
                            "blocked_from_step": blocked_from_step,
                            "blocked_from_status": "blocked",
                            "blocked_response": response,
                            "blocker_resolution": "Retry the captured origin"
                        }
                    }
                }),
            )
            .unwrap();
            let StepAction::Status(action) = result.action else {
                panic!("triage_blocked should return a status action")
            };

            assert_eq!(
                action.status, expected_step,
                "clearance response {response:?} from {blocked_from_step} routed incorrectly"
            );
        }
    }

    #[test]
    fn examples_workflows_status_detours_preserve_validation_doc() {
        let compiled = load_example_compiled_workflow("dev-loop");
        let work_dir = "docs/plans/cache";
        let plan_doc = "docs/plans/cache/plan.md";
        let validation_doc = "docs/plans/cache/validation.md";
        for (step_id, previous_step, previous_status) in [
            ("clarify", "plan", "unclear"),
            ("confirm_plan", "review_plan", "approved"),
            ("confirm_result", "review", "approved"),
            ("blocked", "review_blocker", "user_required"),
        ] {
            let result = run_step(
                &compiled.source_bundle,
                step_id,
                serde_json::json!({
                    "prev": {
                        "step": previous_step,
                        "action": "agent",
                        "status": previous_status,
                        "fields": {
                            "plan": "Reviewed plan and validation guide",
                            "goal": "Add safe cache invalidation",
                            "validation": "cargo test -p cache",
                            "work_dir": work_dir,
                            "plan_doc": plan_doc,
                            "validation_doc": validation_doc,
                            "blocker_statement": "External approval is required",
                            "blocked_from_step": "implement",
                            "blocked_from_status": "blocked",
                            "blocker_reason": "Approval is not available to the agent",
                            "blocker_resolution": "Approve the deployment request"
                        }
                    }
                }),
            )
            .unwrap();
            let StepAction::AskUser(action) = result.action else {
                panic!("{step_id} should preserve artifacts in an ask-user action")
            };

            assert_eq!(action.fields["work_dir"], work_dir);
            assert_eq!(action.fields["plan_doc"], plan_doc);
            assert_eq!(action.fields["validation_doc"], validation_doc);
        }
    }

    #[test]
    fn dev_loop_requires_stable_redacted_validation_guide_contract() {
        let compiled = load_example_compiled_workflow("dev-loop");
        let goal = "Use SYNTHETIC_PRIVATE_PATH_DO_NOT_USE for cache fixtures";
        let validation =
            "SYNTHETIC_TOKEN_DO_NOT_USE=placeholder cargo test -p cache cache_is_fresh";
        let work_dir = "docs/plans/cache_fixtures";
        let plan_doc = "docs/plans/cache_fixtures/plan.md";
        let validation_doc = "docs/plans/cache_fixtures/validation.md";
        let plan_result = run_step(
            &compiled.source_bundle,
            "plan",
            serde_json::json!({
                "request": goal,
                "prev": {
                    "step": "collect_validation_answer",
                    "action": "status",
                    "status": "captured",
                    "fields": {
                        "goal": goal,
                        "validation": validation
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Agent(plan) = plan_result.action else {
            panic!("dev-loop planner should request an agent action")
        };

        for expected in [
            "For every dev-loop planning pass",
            "`work_dir` at `docs/plans/<snake_case_summary>`",
            "`plan_doc` at `<work_dir>/plan.md`",
            "`validation_doc` at `<work_dir>/validation.md`",
            "mandatory for both initial planning and replanning",
            "Never return flat, mismatched, or incomplete",
            "include both document paths in `files`",
            "ordered commands or manual checks",
            "evidence to capture",
            "exit criteria",
            "continue/revise criteria",
            "<REDACTED_VALUE>",
            "environment-variable references",
        ] {
            assert!(plan.prompt.contains(expected), "missing {expected:?}");
        }
        assert!(!plan.prompt.contains("<snake_case_summary>_validation.md"));

        let output = plan.output.expect("planner should declare output");
        for field in ["work_dir", "plan_doc", "validation_doc"] {
            assert_eq!(output.fields[field], "string");
        }
        for field in ["prior_work_dir", "prior_plan_doc", "prior_validation_doc"] {
            assert!(output.fields.get(field).is_none(), "unexpected {field}");
            assert!(!plan.prompt.contains(field), "unexpected {field} guidance");
        }
        assert_eq!(output.fields["files"], "array");

        let planner = compiled
            .definition
            .roles
            .get("planner")
            .expect("dev-loop should define the planner role");
        for expected in [
            "ordinary feature work",
            "docs/plans/<snake_case_summary>.md",
            "dev-loop work requiring a validation guide",
            "<work_dir>/validation.md",
            "bug-fix work",
            "Preserve established feature and bug-fix artifact paths",
            "reuse paths only when they already match the required nested tuple",
        ] {
            assert!(
                planner.instructions.contains(expected),
                "planner role should describe {expected:?}"
            );
        }

        let replan_result = run_step(
            &compiled.source_bundle,
            "plan",
            serde_json::json!({
                "request": goal,
                "prev": {
                    "step": "confirm_plan_answer",
                    "action": "status",
                    "status": "changes_requested",
                    "fields": {
                        "goal": goal,
                        "validation": validation,
                        "work_dir": work_dir,
                        "plan_doc": plan_doc,
                        "validation_doc": validation_doc,
                        "files": [plan_doc, validation_doc]
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Agent(replan) = replan_result.action else {
            panic!("dev-loop replanning should request an agent action")
        };
        for expected in [
            "update those exact documents and preserve all three values",
            "Work dir: docs/plans/cache_fixtures",
            "Plan doc: docs/plans/cache_fixtures/plan.md",
            "Validation doc: docs/plans/cache_fixtures/validation.md",
        ] {
            assert!(replan.prompt.contains(expected), "missing {expected:?}");
        }
        for rejected in ["prior_work_dir", "prior_plan_doc", "prior_validation_doc"] {
            assert!(!replan.prompt.contains(rejected), "unexpected {rejected}");
        }

        for (workflow_name, expected_path_guidance) in [
            ("feature", "docs/plans/<snake_case_summary>.md"),
            ("bugfix", "same bug-fix work folder"),
        ] {
            let ordinary = load_example_compiled_workflow(workflow_name);
            let result = run_step(
                &ordinary.source_bundle,
                "plan",
                serde_json::json!({ "request": "Keep ordinary planning stable" }),
            )
            .unwrap();
            let StepAction::Agent(action) = result.action else {
                panic!("{workflow_name} planner should request an agent action")
            };

            assert!(action.prompt.contains(expected_path_guidance));
            assert!(
                !action.prompt.contains("For every dev-loop planning pass"),
                "{workflow_name} planning should not require a validation guide"
            );

            let review_result = run_step(
                &ordinary.source_bundle,
                "review_plan",
                serde_json::json!({
                    "request": "Keep ordinary planning stable",
                    "prev": {
                        "step": "plan",
                        "action": "agent",
                        "status": "ready",
                        "fields": {
                            "plan_doc": "docs/plans/example.md",
                            "work_dir": "docs/plans/example",
                            "rca_doc": "docs/plans/example/rca.md",
                            "repro_test": "tests/example.rs::repro"
                        }
                    }
                }),
            )
            .unwrap();
            let StepAction::Agent(review) = review_result.action else {
                panic!("{workflow_name} review should request an agent action")
            };
            let review_guidance = if workflow_name == "feature" {
                "docs/plans/<snake_case_summary>.md"
            } else {
                "same `docs/plans/<snake_case_bug_summary>/` folder as the RCA"
            };
            assert!(review.prompt.contains(review_guidance));
        }

        let review_result = run_step(
            &compiled.source_bundle,
            "review_plan",
            serde_json::json!({
                "request": goal,
                "prev": {
                    "step": "plan",
                    "action": "agent",
                    "status": "ready",
                    "fields": {
                        "goal": goal,
                        "validation": validation,
                        "work_dir": work_dir,
                        "plan_doc": plan_doc,
                        "validation_doc": validation_doc,
                        "files": [plan_doc, validation_doc]
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Agent(review) = review_result.action else {
            panic!("dev-loop plan review should request an agent action")
        };

        for expected in [
            "Read and review both `plan_doc` and `validation_doc`",
            "`work_dir` is `docs/plans/<snake_case_summary>`",
            "`plan_doc` is `<work_dir>/plan.md`",
            "`validation_doc` is `<work_dir>/validation.md`",
            "both documents share that declared folder",
            "both document paths are included in `files`",
            "rejects every flat, mismatched, or incomplete tuple",
            "regardless of any extra fields supplied by the planner",
            "Do not override its verdict",
            "Artifact layout check: valid_nested",
            "every exit criterion",
            "either artifact",
            "safe placeholders",
            "keep the guide executable",
        ] {
            assert!(review.prompt.contains(expected), "missing {expected:?}");
        }

        let validate_result = run_step(
            &compiled.source_bundle,
            "validate",
            serde_json::json!({
                "request": goal,
                "prev": {
                    "step": "test",
                    "action": "agent",
                    "status": "passed",
                    "fields": {
                        "goal": goal,
                        "validation": validation,
                        "work_dir": work_dir,
                        "plan_doc": plan_doc,
                        "validation_doc": validation_doc
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Agent(validate) = validate_result.action else {
            panic!("dev-loop validation should request an agent action")
        };

        assert!(validate.prompt.contains("complete ordered procedure"));
        assert!(validate.prompt.contains("every exit criterion"));
        assert!(validate.prompt.contains(work_dir));
        assert!(validate.prompt.contains(plan_doc));
        assert!(validate.prompt.contains(validation_doc));
        assert_eq!(
            validate
                .output
                .expect("validator should declare output")
                .fields["validation_doc"],
            "string"
        );

        let commit_result = run_step(
            &compiled.source_bundle,
            "commit",
            serde_json::json!({
                "request": goal,
                "prev": {
                    "step": "confirm_result_answer",
                    "action": "status",
                    "status": "confirmed",
                    "fields": {
                        "goal": goal,
                        "validation": validation,
                        "work_dir": work_dir,
                        "plan_doc": plan_doc,
                        "validation_doc": validation_doc
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Agent(commit) = commit_result.action else {
            panic!("dev-loop commit should request an agent action")
        };
        assert!(commit.prompt.contains("`docs/plans/*/*.md`"));
        assert!(
            commit
                .prompt
                .contains("dev-loop planning-folder or bug-fix work-folder documents")
        );
    }

    #[test]
    fn dev_loop_review_rejects_every_invalid_artifact_tuple() {
        let compiled = load_example_compiled_workflow("dev-loop");
        let goal = "Keep nested planning paths stable";
        let validation = "cargo test -p cache";
        let user_feedback = serde_json::json!(["Keep the requested validation method"]);
        let cases = [
            (
                "flat",
                serde_json::json!({
                    "plan_doc": "docs/plans/cache.md",
                    "validation_doc": "docs/plans/cache_validation.md",
                    "files": ["docs/plans/cache.md", "docs/plans/cache_validation.md"]
                }),
            ),
            (
                "forged prior fields",
                serde_json::json!({
                    "plan_doc": "docs/plans/cache.md",
                    "validation_doc": "docs/plans/cache_validation.md",
                    "prior_plan_doc": "docs/plans/cache.md",
                    "prior_validation_doc": "docs/plans/cache_validation.md",
                    "files": ["docs/plans/cache.md", "docs/plans/cache_validation.md"]
                }),
            ),
            (
                "mismatched filename",
                serde_json::json!({
                    "work_dir": "docs/plans/cache",
                    "plan_doc": "docs/plans/cache/implementation.md",
                    "validation_doc": "docs/plans/cache/validation.md",
                    "files": ["docs/plans/cache/implementation.md", "docs/plans/cache/validation.md"]
                }),
            ),
            (
                "mismatched folder",
                serde_json::json!({
                    "work_dir": "docs/plans/cache",
                    "plan_doc": "docs/plans/cache/plan.md",
                    "validation_doc": "docs/plans/other/validation.md",
                    "files": ["docs/plans/cache/plan.md", "docs/plans/other/validation.md"]
                }),
            ),
            (
                "missing files entry",
                serde_json::json!({
                    "work_dir": "docs/plans/cache",
                    "plan_doc": "docs/plans/cache/plan.md",
                    "validation_doc": "docs/plans/cache/validation.md",
                    "files": ["docs/plans/cache/plan.md"]
                }),
            ),
            (
                "underscore only summary",
                serde_json::json!({
                    "work_dir": "docs/plans/_",
                    "plan_doc": "docs/plans/_/plan.md",
                    "validation_doc": "docs/plans/_/validation.md",
                    "files": ["docs/plans/_/plan.md", "docs/plans/_/validation.md"]
                }),
            ),
            (
                "leading underscore summary",
                serde_json::json!({
                    "work_dir": "docs/plans/_cache",
                    "plan_doc": "docs/plans/_cache/plan.md",
                    "validation_doc": "docs/plans/_cache/validation.md",
                    "files": ["docs/plans/_cache/plan.md", "docs/plans/_cache/validation.md"]
                }),
            ),
            (
                "trailing underscore summary",
                serde_json::json!({
                    "work_dir": "docs/plans/cache_",
                    "plan_doc": "docs/plans/cache_/plan.md",
                    "validation_doc": "docs/plans/cache_/validation.md",
                    "files": ["docs/plans/cache_/plan.md", "docs/plans/cache_/validation.md"]
                }),
            ),
            (
                "consecutive underscores summary",
                serde_json::json!({
                    "work_dir": "docs/plans/cache__fix",
                    "plan_doc": "docs/plans/cache__fix/plan.md",
                    "validation_doc": "docs/plans/cache__fix/validation.md",
                    "files": ["docs/plans/cache__fix/plan.md", "docs/plans/cache__fix/validation.md"]
                }),
            ),
        ];

        for (name, artifact_fields) in cases {
            let mut fields = artifact_fields;
            fields["user_feedback"] = user_feedback.clone();
            fields["goal"] = serde_json::json!(goal);
            fields["validation"] = serde_json::json!(validation);
            fields["rca_doc"] = serde_json::json!("docs/plans/untrusted/rca.md");
            fields["repro_test"] = serde_json::json!("tests/untrusted.rs::repro");
            let result = run_step(
                &compiled.source_bundle,
                "review_plan",
                serde_json::json!({
                    "request": goal,
                    "prev": {
                        "step": "plan",
                        "action": "agent",
                        "status": "ready",
                        "fields": fields
                    }
                }),
            )
            .unwrap();
            let StepAction::Status(rejection) = result.action else {
                panic!("{name} tuple should be rejected before agent review")
            };

            assert_eq!(rejection.status, "changes_requested", "{name}");
            assert_eq!(rejection.fields["user_feedback"], user_feedback, "{name}");
            assert_eq!(rejection.fields["goal"], goal, "{name}");
            assert_eq!(rejection.fields["validation"], validation, "{name}");
            for untrusted in [
                "work_dir",
                "plan_doc",
                "validation_doc",
                "prior_work_dir",
                "prior_plan_doc",
                "prior_validation_doc",
                "rca_doc",
                "repro_test",
                "files",
            ] {
                assert!(
                    rejection.fields.get(untrusted).is_none(),
                    "{name} retained untrusted {untrusted}"
                );
            }
        }
    }

    #[test]
    fn examples_workflows_use_expected_named_agents() {
        let feature = load_example_workflow("feature");
        assert_expected_role_agents(
            "feature",
            &feature,
            &[
                ("planner", "default"),
                ("implementer", "default"),
                ("reviewer", "reviewer"),
            ],
        );

        let bugfix = load_example_workflow("bugfix");
        assert_expected_role_agents(
            "bugfix",
            &bugfix,
            &[
                ("investigator", "default"),
                ("planner", "default"),
                ("implementer", "default"),
                ("reviewer", "reviewer"),
            ],
        );
    }

    #[test]
    fn dev_loop_gates_review_on_user_validation() {
        let definition = load_example_workflow("dev-loop");

        assert_eq!(definition.name, "dev-loop");
        assert_eq!(definition.head, "collect_validation");
        assert_step_transition(
            "dev-loop",
            &definition,
            "collect_validation",
            "answered",
            "collect_validation_answer",
        );
        assert_step_transition(
            "dev-loop",
            &definition,
            "collect_validation_answer",
            "captured",
            "plan",
        );
        assert_step_transition("dev-loop", &definition, "test", "passed", "validate");
        assert_step_transition("dev-loop", &definition, "validate", "achieved", "review");
        assert_step_transition(
            "dev-loop",
            &definition,
            "validate",
            "not_achieved",
            "revise",
        );
        assert_step_transition(
            "dev-loop",
            &definition,
            "review",
            "approved",
            "confirm_result",
        );
        assert_expected_role_agents(
            "dev-loop",
            &definition,
            &[
                ("planner", "default"),
                ("implementer", "default"),
                ("validator", "reviewer"),
                ("reviewer", "reviewer"),
            ],
        );
    }

    #[test]
    fn dev_loop_captures_exact_goal_and_validation_method() {
        let compiled = load_example_compiled_workflow("dev-loop");
        let goal = "Add deterministic cache invalidation";
        let validation = "cargo test -p cache invalidation_is_deterministic";
        let result = run_step(
            &compiled.source_bundle,
            "collect_validation",
            serde_json::json!({ "request": goal }),
        )
        .unwrap();

        let StepAction::AskUser(action) = result.action else {
            panic!("dev-loop should ask for the user's validation method")
        };
        assert!(action.message.contains(goal));
        assert_eq!(action.fields["goal"], goal);

        let result = run_step(
            &compiled.source_bundle,
            "collect_validation_answer",
            serde_json::json!({
                "request": goal,
                "prev": {
                    "step": "collect_validation",
                    "action": "ask_user",
                    "status": "answered",
                    "fields": {
                        "goal": goal,
                        "answer": validation
                    }
                }
            }),
        )
        .unwrap();

        let StepAction::Status(action) = result.action else {
            panic!("dev-loop should capture the answered validation method")
        };
        assert_eq!(action.status, "captured");
        assert_eq!(action.fields["goal"], goal);
        assert_eq!(action.fields["validation"], validation);
    }

    #[test]
    fn dev_loop_plan_and_validator_require_user_method() {
        let compiled = load_example_compiled_workflow("dev-loop");
        let goal = "Add deterministic cache invalidation";
        let validation = "cargo test -p cache invalidation_is_deterministic";
        let context = serde_json::json!({
            "request": goal,
            "prev": {
                "step": "collect_validation_answer",
                "action": "status",
                "status": "captured",
                "fields": {
                    "goal": goal,
                    "validation": validation
                }
            }
        });
        let plan_result = run_step(&compiled.source_bundle, "plan", context).unwrap();
        let StepAction::Agent(plan_action) = plan_result.action else {
            panic!("dev-loop plan should request an agent action")
        };

        assert!(plan_action.prompt.contains(validation));
        assert!(plan_action.prompt.contains("do not replace it"));

        let validate_result = run_step(
            &compiled.source_bundle,
            "validate",
            serde_json::json!({
                "request": goal,
                "prev": {
                    "step": "test",
                    "action": "agent",
                    "status": "passed",
                    "fields": {
                        "summary": "focused tests passed",
                        "goal": goal,
                        "validation": validation,
                        "work_dir": "docs/plans/deterministic_cache_invalidation",
                        "plan_doc": "docs/plans/deterministic_cache_invalidation/plan.md",
                        "validation_doc": "docs/plans/deterministic_cache_invalidation/validation.md"
                    }
                }
            }),
        )
        .unwrap();
        let StepAction::Agent(validate_action) = validate_result.action else {
            panic!("dev-loop validator should request an agent action")
        };

        assert_eq!(validate_action.role, "validator");
        assert!(validate_action.prompt.contains(goal));
        assert!(validate_action.prompt.contains(validation));
        assert!(
            validate_action
                .prompt
                .contains("Execute the user-provided Validation method exactly")
        );
        let output = validate_action
            .output
            .expect("dev-loop validator should declare output");
        for status in ["achieved", "not_achieved", "blocked"] {
            assert!(output.statuses.iter().any(|candidate| candidate == status));
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
    fn captures_workflow_config_set_with_description() {
        let source = snapshot(
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", start, { description = "does a thing", config_set = "careful" })
            "#,
        );
        let definition = compile_snapshot(&source).unwrap();
        assert_eq!(definition.description.as_deref(), Some("does a thing"));
        assert_eq!(definition.config_set.as_deref(), Some("careful"));
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
        assert_eq!(definition.config_set, None);
    }

    #[test]
    fn workflow_without_config_uses_no_explicit_config_set() {
        let source = snapshot(
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", start)
            "#,
        );
        let definition = compile_snapshot(&source).unwrap();
        assert_eq!(definition.description, None);
        assert_eq!(definition.config_set, None);
    }

    #[test]
    fn workflow_config_set_must_be_a_nonblank_string() {
        for config_set in ["\"   \"", "42"] {
            let source = snapshot(&format!(
                r#"
                local start = step("start")
                start.run = function(ctx) return action.status {{ status = "success" }} end
                return workflow("wf", start, {{ config_set = {config_set} }})
                "#
            ));
            let err = compile_snapshot(&source).unwrap_err();
            assert!(
                err.to_string()
                    .contains("workflow config_set must be a non-empty string"),
                "{err:#}"
            );
        }
    }

    #[test]
    fn workflow_definition_without_config_set_deserializes_compatibly() {
        let source = snapshot(
            r#"
            local start = step("start")
            start.run = function(ctx) return action.status { status = "success" } end
            return workflow("wf", start, { config_set = "careful" })
            "#,
        );
        let definition = compile_snapshot(&source).unwrap();
        let mut serialized = serde_json::to_value(definition).unwrap();
        serialized.as_object_mut().unwrap().remove("config_set");

        let definition: WorkflowDefinition = serde_json::from_value(serialized).unwrap();
        assert_eq!(definition.config_set, None);
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
