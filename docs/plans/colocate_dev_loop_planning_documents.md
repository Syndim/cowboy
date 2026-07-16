# Plan

Make the dev-loop folder layout a clean cutover in the revised workflow source. New dev-loop runs must use `docs/plans/<snake_case_summary>/` as `work_dir`, `<work_dir>/plan.md` as `plan_doc`, and `<work_dir>/validation.md` as `validation_doc`; both document paths must be present in `files`. The plan reviewer will deterministically reject every flat or mismatched tuple, regardless of any extra fields supplied by the planner.

Existing in-progress runs do not need compatibility logic in the revised workflow. `WorkflowRuntime` copies the compiled source bundle into `WorkflowRun.workflow_sources` when the run starts (`crates/workflow/engine/src/runtime.rs`), and resume, answer, resolution-option, and resolve paths reconstruct and compile the workflow from that persisted bundle rather than reloading current catalog files. Therefore runs created from the old dev-loop source continue using the old path policy, while runs created from the revised source receive the nested-only policy. Add a runtime regression test for this invariant before relying on it.

Remove the current `prior_work_dir`, `prior_plan_doc`, and `prior_validation_doc` design. Those fields are emitted by the planner action and cannot establish provenance. Replanning under the revised source remains stable because every valid run already carries the nested `work_dir`, `plan_doc`, and `validation_doc` tuple through ordinary workflow fields; review validates the tuple itself, not a planner-authored account of its history.

Keep the cutover scoped to dev-loop. Feature planning remains `docs/plans/<snake_case_summary>.md`, and bug-fix planning remains `<work_dir>/plan.md` beside `<work_dir>/rca.md`.

# Changes

- Update `examples/workflows/steps/plan.lua` to remove all `prior_*` instructions and output fields. For dev-loop planning, require the nested folder tuple for both initial planning and replanning, require both documents in `files`, and make the dev-loop rule take precedence over the shared ordinary-plan path wording.
- Update `examples/workflows/roles/planner.lua` so the role describes the nested-only dev-loop layout without promising compatibility for arbitrary established paths. Preserve the existing feature and bug-fix layout rules.
- Simplify the deterministic dev-loop guard in `examples/workflows/steps/review_plan.lua` to accept only a syntactically valid nested tuple with both files listed. Remove `valid_preserved`, `prior_artifact_fields`, all trust in planner-authored provenance, and recovery output derived from `prior_*`; an invalid result should preserve only trusted workflow context such as `user_feedback`, `goal`, and `validation`, then route back to planning with explicit nested-layout feedback.
- Remove `prior_*` rendering from `examples/workflows/utils/context.lua`; these fields no longer belong to the workflow contract.
- Keep the nested planning-document scope in `examples/workflows/steps/commit.lua` and the documented dev-loop folder layout in `README.md`, adjusting wording only if the clean-cutover implementation makes either inaccurate.
- Update `crates/workflow/lua/src/loader.rs` fixtures and assertions to remove the legacy-preservation contract, enforce the nested-only tuple, and verify normal and blocker detours continue to propagate `work_dir`, `plan_doc`, and `validation_doc` unchanged.
- Add focused coverage in `crates/workflow/engine/src/runtime.rs` proving an in-progress filesystem workflow resumes from `WorkflowRun.workflow_sources` after its on-disk Lua source changes. This test establishes the workflow-controlled boundary that makes the clean cutover safe.

# Tests to be added/updated

- Update the dev-loop planner contract test to assert the nested `work_dir`, `plan_doc`, and `validation_doc` requirements, both paths in `files`, and absence of every `prior_*` output field and instruction.
- Replace the legacy-flat acceptance test with deterministic review cases: a valid nested tuple proceeds to agent review; a flat tuple is rejected; a flat tuple with matching self-asserted `prior_*` values is still rejected; mismatched filenames, folders, or missing `files` entries are rejected; and rejection preserves cumulative `user_feedback` without treating rejected artifact values as trusted recovery state.
- Retain or extend replanning coverage using an already valid nested tuple, proving the same `work_dir`, `plan_doc`, and `validation_doc` values are reused without introducing separate provenance fields.
- Retain feature and bug-fix assertions proving their planner and reviewer path contracts are unchanged.
- Add a runtime snapshot regression that starts a stepwise filesystem workflow, changes the workflow source on disk, resumes the run, and observes behavior from the original persisted source rather than the replacement. Exercise at least the normal resume path that all continuing runs depend on; existing code-path inspection covers answer and resolution reconstruction through the same `snapshot_from_run` helper.
- Update propagation fixtures for dev-loop confirmation, validation, implementation review, commit, and blocker detours to use the nested tuple and assert it survives unchanged.

# How to verify

- Run the focused nested-layout and forged-provenance tests in `cowboy-workflow-lua`, including the case where a flat tuple supplies matching self-asserted `prior_*` fields.
- Run the new focused runtime source-snapshot regression in `cowboy-workflow-engine`.
- Run `cargo test -p cowboy-workflow-lua`.
- Run `cargo test -p cowboy-workflow-engine`.
- Run `cargo clippy -p cowboy-workflow-lua -p cowboy-workflow-engine --all-targets -- -D warnings` and fix every warning.
- Run `cargo fmt -p cowboy-workflow-lua -p cowboy-workflow-engine -- --check`.
- Inspect the resulting workflow contracts to confirm no `prior_*` field remains, all newly compiled dev-loop runs reject flat paths, existing runs are insulated by their persisted workflow source, and feature/bug-fix layouts are unchanged.

# TODO

- [x] Remove planner-authored `prior_*` provenance from the dev-loop planner, reviewer, and context contracts.
- [x] Enforce the nested-only dev-loop artifact tuple during initial planning, replanning, and deterministic review.
- [x] Preserve feature and bug-fix planning layouts while retaining nested dev-loop document commit coverage and accurate README wording.
- [x] Add the runtime regression proving resumed runs execute their persisted workflow-source snapshot after catalog source changes.
- [x] Replace legacy flat-path acceptance coverage with nested replan and forged-`prior_*` rejection regressions.
- [x] Update dev-loop artifact propagation fixtures and assertions for the clean-cutover contract.
- [x] Run focused workflow-Lua and workflow-engine regressions and fix all failures.
- [x] Run both complete crate test suites, Clippy with warnings denied, and formatting checks.
