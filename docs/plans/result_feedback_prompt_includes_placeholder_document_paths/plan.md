## Plan

Use the reviewed RCA at `docs/plans/result_feedback_prompt_includes_placeholder_document_paths/rca.md` and the unchanged investigator-added regression test `crates/workflow/lua/src/loader.rs::examples_workflows_review_result_feedback_prompt_uses_concrete_document_paths` as the fix constraints. Correct the shared result-feedback workflow prompt at its source so the reviewer receives only the concrete artifact references already rendered from previous-step fields. Do not change TUI presentation or agent-executor plumbing: `AgentProgressKind::Prompt` and `Client::prompt` receive clones of the same generated prompt, so the literal placeholders are genuinely sent to the backend.

## Changes

- Update `examples/workflows/steps/review_result_feedback.lua` to replace the static instruction containing literal ``Plan doc: ...``, ``Work dir: ...``, ``RCA doc: ...``, and ``Repro test: ...`` references with context-relative wording that tells the reviewer to inspect and preserve artifact references supplied in the previous-step context.
- Leave `examples/workflows/utils/context.lua` unchanged because it already renders the concrete `work_dir`, `plan_doc`, `rca_doc`, and `repro_test` values.
- Preserve the shared feature/bug-fix result-feedback behavior: request and previous-step context, exact `user_feedback` preservation, `changes_requested` versus `replan_requested` routing guidance, and the existing output status/field contract.
- Do not add TUI-specific filtering or modify `crates/workflow/agent` prompt delivery; removing the placeholders at the workflow source keeps the displayed prompt and backend prompt identical.

## Tests to be added/updated

- Do not edit or replace the investigator-added regression test `crates/workflow/lua/src/loader.rs::examples_workflows_review_result_feedback_prompt_uses_concrete_document_paths`. It must turn green by confirming all four concrete artifact references remain present while all four literal placeholder references are absent.
- No additional regression test is needed. Retain the existing `examples_workflows_review_result_feedback_agent_triages_user_feedback` coverage to verify both feature and bug-fix workflows still use the shared reviewer step, preserve concrete context, expose the same output statuses, and retain routing guidance.

## How to verify

1. Run `cargo test -p cowboy-workflow-lua examples_workflows_review_result_feedback_prompt_uses_concrete_document_paths -- --nocapture` and confirm the unchanged repro test passes.
2. Run `cargo test -p cowboy-workflow-lua examples_workflows_review_result_feedback_agent_triages_user_feedback -- --nocapture` and confirm feature and bug-fix result-feedback contracts remain intact.
3. Run `cargo fmt --package cowboy-workflow-lua --check`.
4. Run `cargo clippy -p cowboy-workflow-lua --tests -- -D warnings` and resolve any warnings introduced by the change.

## TODO

- [x] Replace the literal artifact placeholders in `examples/workflows/steps/review_result_feedback.lua` with previous-context-relative reviewer guidance.
- [x] Preserve existing result-feedback context rendering, exact `user_feedback` handling, routing guidance, and output fields for both feature and bug-fix workflows.
- [x] Run the unchanged concrete-document-path regression test and confirm it passes.
- [x] Run the existing feature/bug-fix result-feedback triage test and confirm it passes.
- [x] Run the focused formatting and Clippy checks without warnings.
