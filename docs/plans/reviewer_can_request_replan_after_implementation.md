# Plan

Allow the post-implementation reviewer in the example feature and bug-fix workflows to send work back to planning when the implementation correctly followed an unsound plan. Today `examples/workflows/workflows/feature.lua:41-42` and `examples/workflows/workflows/bugfix.lua:54-55` route every reviewer rejection through `revise`, so the implementer is asked to patch code even when the defect is in the approved plan. Add a distinct reviewer status for plan-level rejection and route it to the existing `plan` step.

Keep this as a workflow-file change, not an engine change. The workflow runtime already supports arbitrary status-to-step transitions, and `examples/workflows/steps/plan.lua` already consumes previous feedback through `context.previous_step_context(ctx, "Previous feedback:")`. The missing pieces are an explicit post-implementation review status, graph routing from that status to `plan`, and prompt copy that preserves the existing plan/work-dir/RCA/repro-test fields when the planner is being asked to re-plan.

# Changes

- Update `examples/workflows/steps/review_implementation.lua`.
  - Add a third output status such as `replan_requested` alongside `approved` and `changes_requested`.
  - Tell the reviewer to use `changes_requested` for implementation defects that can be fixed within the approved plan.
  - Tell the reviewer to use `replan_requested` when the implementation follows the plan but the plan is incomplete, unsafe, incorrectly scoped, unverifiable, or otherwise not solid.
  - Require actionable feedback for the planner when `replan_requested` is returned.
  - Continue preserving `work_dir`, `plan_doc`, `rca_doc`, and `repro_test` fields exactly when present.
- Update `examples/workflows/workflows/feature.lua`.
  - Add `review:on("replan_requested", plan)`.
  - Leave `review:on("changes_requested", revise)` intact for implementation-only fixes.
- Update `examples/workflows/workflows/bugfix.lua`.
  - Add `review:on("replan_requested", plan)`.
  - Leave `review:on("changes_requested", revise)` intact for implementation-only fixes.
  - Preserve the bug-fix flow’s RCA and repro-test handoff through the existing output fields.
- Update `examples/workflows/steps/plan.lua` so re-planning edits the existing plan context instead of accidentally starting a separate plan path.
  - If previous feedback includes `Plan doc: ...`, instruct the planner to update that existing plan document and return the same `plan_doc` value.
  - For bug fixes, keep the existing rule that `Work dir: ...`, `RCA doc: ...`, and `Repro test: ...` are preserved exactly and that the repro test remains an unchanged input to the fix.
  - Keep the existing initial-plan behavior for ordinary non-bug-fix work with no prior `Plan doc: ...`.
- Add a reviewer gate for user result-confirmation feedback after implementation review.
  - Add `examples/workflows/steps/review_result_feedback.lua` to ask the reviewer whether the user's feedback is an implementation revision or a plan-level re-plan request.
  - Route `confirm_result_answer` `changes_requested` through that reviewer step in feature and bug-fix workflows.
  - Route reviewer `changes_requested` output to `revise` and `replan_requested` output to `plan`, preserving `Work dir`, `Plan doc`, `RCA doc`, and `Repro test` values.

- Do not change `revise.lua`, `implement.lua`, `test.lua`, or engine/runtime code unless tests reveal the workflow source cannot express this transition. The existing status routing and previous-step context mechanisms are sufficient.

# Tests to be added/updated

- Add focused Lua workflow tests in `crates/workflow/lua/src/loader.rs` for the example workflows.
  - Load `examples/workflows/workflows/feature.lua` and assert the `review` step routes `replan_requested` to `plan`, `changes_requested` to `revise`, and `approved` to `confirm_result`.
  - Load `examples/workflows/workflows/bugfix.lua` and assert the `review` step routes `replan_requested` to `plan`, `changes_requested` to `revise`, and `approved` to `confirm_result`.
- Add or extend a Lua runtime test that executes the example `review` step from the loaded source bundle and asserts the returned `StepAction::Agent` output status list includes `approved`, `changes_requested`, and `replan_requested`.
- In the same runtime coverage, assert the generated review prompt contains guidance distinguishing implementation fixes from plan-level rejections and still mentions preserving `Plan doc`, `Work dir`, `RCA doc`, and `Repro test` values.
- Add focused Lua workflow tests for the result-feedback reviewer gate: graph routing from `confirm_result_answer`, reviewer-step routes to `revise`/`plan`, and prompt/output contract for preserving user feedback and path fields.

# How to verify

- Run `cargo test -p cowboy-workflow-lua examples_workflows` to verify the example feature/bug-fix workflow graphs and review-step output contract.
- Run `cargo test -p cowboy-workflow-lua` if the focused test filter does not include all new assertions.
- Optional manual workflow-source smoke check:
  - load the `feature` example workflow through the catalog or Lua loader;
  - confirm the `review` step has three outbound routes: `approved -> confirm_result`, `changes_requested -> revise`, and `replan_requested -> plan`;
  - repeat for the `bugfix` example workflow.

# TODO

- [x] Add a post-implementation reviewer output status for plan-level rejection.
- [x] Update the reviewer prompt to distinguish implementation fixes from re-plan requests.
- [x] Route feature workflow review `replan_requested` results back to `plan`.
- [x] Route bug-fix workflow review `replan_requested` results back to `plan`.
- [x] Update the planner prompt to reuse an existing `Plan doc: ...` path during re-planning.
- [x] Add workflow graph tests for the new review-to-plan route in feature and bug-fix workflows.
- [x] Add review-step action/prompt tests for the new `replan_requested` output contract.
- [x] Run the focused `cowboy-workflow-lua` tests.
- [x] Add a reviewer step for user result-confirmation feedback.
- [x] Route feature and bug-fix result feedback through the reviewer step.
- [x] Add graph and prompt tests for the result-feedback reviewer step.
- [x] Run focused `cowboy-workflow-lua` tests after result-feedback reviewer changes.
