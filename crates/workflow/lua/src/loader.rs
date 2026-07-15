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
                                "plan_doc": "docs/plans/example.md",
                                "work_dir": "docs/plans/example",
                                "rca_doc": "docs/plans/example/rca.md",
                                "repro_test": "crates/workflow/lua/src/loader.rs::confirmation_repro",
                                "commands": ["cargo test"]
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
                if *step_id == "commit" {
                    for field in ["work_dir", "plan_doc", "rca_doc", "repro_test"] {
                        assert_eq!(
                            output.fields[field], "string",
                            "commit should declare {field}"
                        );
                    }

                    for label in ["Work dir", "Plan doc", "RCA doc", "Repro test"] {
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
                        "plan_doc": "docs/plans/deterministic_cache_invalidation.md"
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
