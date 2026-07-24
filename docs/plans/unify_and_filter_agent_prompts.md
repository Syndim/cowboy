# Plan

Introduce one stage-aware prompt builder for the example workflows so each agent receives a consistent prompt containing only the context required for that step. Keep `cowboy-workflow-agent::build_agent_prompt` as the final prompt assembler for role instructions, cumulative `User Inputs`, task text, and deliverable format; do not duplicate the original request inside workflow-authored task text because the Rust assembler already supplies the complete ordered `RunUserInput` history.

Replace the unconditional `previous_step_context` rendering in `examples/workflows/utils/context.lua` with an explicit prompt specification. Each caller will select the previous-step scalar fields, artifact references, result body, failures, and evidence sources it needs. Optional unselected evidence fields will be omitted; a selected evidence family is valid only when both its command and evidence arrays are present, including the legitimate case where either or both are explicit empty arrays. If exactly one member of a selected pair is absent, reject the context rather than silently omitting the family. Selected values will be type-checked before rendering, and missing or malformed required context will produce the caller's existing non-success workflow result before an `action.agent` is dispatched. Prompts must never contain `<missing-array>`, `<invalid-array:...>`, empty headings, or evidence families owned by later stages.

Preserve `user_feedback` as a separate cumulative raw-user-direction field. The builder may render it once when the current step needs workflow-control feedback that is not part of `ctx.user_inputs`, but it must not merge it with reviewer feedback, evidence issues, body prose, or the automatically appended `User Inputs` section.

The workspace-wide formatter gate currently fails on files outside the prompt-builder implementation, in a shared dirty worktree whose unrelated changes must not be modified. This replan prospectively approves a non-mutating exception before replacement evidence is generated: TODO-08 must compare current and detached clean-HEAD failure sets using the same explicitly pinned Rust 1.89 formatter, directly check every feature-owned Rust file with that toolchain, and reject inconsistent exit-status/parsed-result combinations. It must not run mutating workspace formatting or edit/revert unrelated files.

Stable IDs remain in their existing order. TODO-01, TODO-03, and TODO-04 are reopened because their prior evidence did not prove rejection of one-sided selected evidence families, tuple-key reviewer-assessment uniqueness, or concrete malformed-blocker diagnostics. Their prior observed-result claims must be replaced rather than reused. TODO-07 and TODO-08 must then be rerun against the corrected implementation; TODO-08 appears after TODO-07 in the document but its proof remains part of TODO-07's acceptance procedure.

# Changes

- Refactor `examples/workflows/utils/context.lua` around a declarative builder such as `build_agent_prompt(ctx, spec)` with stable ordering for:
  - the step objective and step-specific instructions;
  - selected previous-step metadata (`step`, `status`, `summary`, `feedback`, goal/validation, artifact paths, blocker fields, files, failures, and body);
  - selected evidence sources (`implementation`, `tester`, `validator`, `reviewer`) and optional reviewer assessments;
  - reusable preservation/review/evidence guidance, included at most once.
- Add reusable validation helpers for scalar, array, evidence-record, command-record, and reviewer-assessment inputs. Do not stringify absent or invalid values into prompts and do not synthesize placeholder strings as evidence values. Return structured field-name/type diagnostics so the caller can take its existing failure, changes-requested, not-achieved, or blocked route without contacting an agent.
- Enforce command/evidence pair completeness for every selected evidence source, regardless of whether the source is required or optional for that stage. Continue accepting explicit empty arrays, but reject commands-without-evidence and evidence-without-commands with diagnostics naming both the missing field and its present partner.
- Validate reviewer-assessment uniqueness exclusively by `(source, subject_kind, subject_id)`. A second assessment with the same tuple must be rejected even when a non-key field such as `subject`, rationale, or verdict differs.
- Remove `request_context(ctx)` from agent task composition. Preserve clarification answers and other `action.ask_user` direction through the selected previous-step fields or `user_feedback`; leave `ctx.request` and `ctx.user_inputs` available to workflow code for non-agent actions such as validation collection.
- Migrate all agent-producing modules under `examples/workflows/steps/` to the shared builder, including planning, RCA investigation/review, implementation/revision, testing, goal validation, plan/implementation/result/blocker review, and commit. Give each step a minimal allow-list:
  - planning and RCA stages receive relevant feedback and artifact inputs, but no implementation/test/validation/reviewer evidence;
  - initial implementation receives the approved planning context; revision additionally receives only the prior implementation evidence it must replace or preserve;
  - testing receives implementation evidence only;
  - validation receives implementation and tester evidence only;
  - implementation review receives implementation/test evidence and validator evidence only when user validation is required;
  - result-feedback and blocker review receive only evidence arrays actually present and required for their decision;
  - commit receives the approved summary and artifact references, not the complete evidence payload.
- Keep `crates/workflow/agent/src/prompt.rs` as the canonical final assembler and document/test the layering invariant: cumulative user inputs are appended exactly once by Rust, while workflow-authored prompts contain only the stage objective and selected workflow context. Add a cross-layer integration test in `cowboy-workflow-engine`, which already depends on both `cowboy-workflow-lua` and `cowboy-workflow-agent`, so the acceptance proof executes an actual example Lua agent step and passes its generated `AgentAction` through `build_agent_prompt`. Also verify visible/backend prompt identity in the existing `cowboy-workflow-agent` executor test surface; no ACP-client or TUI code/test change is planned.
- Update `docs/workflow-authoring.md` to explain the automatic `User Inputs` section, the no-duplication rule for workflow-authored prompts, omission of absent optional context, and deterministic rejection of malformed required context.
- Treat `crates/workflow/agent/src/executor.rs`, `crates/workflow/agent/src/prompt.rs`, `crates/workflow/engine/src/runtime.rs`, `crates/workflow/engine/tests/example_prompt_composition.rs`, and `crates/workflow/lua/src/loader.rs` as the complete feature-owned Rust formatting scope. Check those paths with `rustfmt +1.89.0`. Run both workspace checks with `cargo +1.89.0 fmt` so repository-local or ambient overrides cannot select different formatters. For each workspace result, require status 0 with an empty parsed failure set or status 1 with a nonempty parsed failure set; reject every other status/result combination.

# Tests to be added/updated

- Replace `loader::tests::examples_workflows_render_typed_evidence_and_invalid_comparisons` in package `cowboy-workflow-lua` with `loader::tests::examples_workflows_prompt_builder_omits_absent_and_rejects_invalid_context`. The new test must prove unselected optional arrays are omitted, selected paired empty arrays remain valid, and malformed required context returns a non-agent action with safe field-level diagnostics. It must also exercise an optionally selected family in `review_result_feedback` in both one-sided states: commands present/evidence absent and evidence present/commands absent. Both states must return `changes_requested` without dispatching an agent and must identify the absent field and its present partner.
- Update `loader::tests::examples_workflows_render_source_labeled_evidence_in_stable_order` in package `cowboy-workflow-lua` to exercise explicit source selection and retain stable ordering for valid selected arrays without rendering unselected or absent sources.
- Keep `loader::tests::examples_workflows_allow_manual_only_evidence_without_commands` in package `cowboy-workflow-lua`, but assert that valid empty command arrays render only when the current stage selected that source and the evidence contract permits them.
- Add `loader::tests::examples_workflows_prompt_matrix_uses_only_stage_context` in package `cowboy-workflow-lua`. Execute every feature, bug-fix, and dev-loop agent step with one superset fixture and assert the exact allowed headings, forbidden headings, placeholder absence, and single occurrence of reusable guidance. Give `ctx.request` and every `ctx.user_inputs[*].content` unique sentinels and assert none occur in the workflow-authored `AgentAction.prompt`; also reject a workflow-authored `## User Inputs` heading or cumulative-user-input boilerplate.
- Add `prompt::tests::prompt_includes_each_user_input_exactly_once` in package `cowboy-workflow-agent` to prove the initial request and each accepted follow-up appear once in the final prompt after example workflow task text stops embedding `ctx.request`.
- Rename or extend the executor coverage as `executor::tests::progress_and_backend_receive_identical_prompt` in package `cowboy-workflow-agent`. Capture the emitted `AgentProgressKind::Prompt` value and the mock client's received prompt and assert exact equality plus absence of placeholder/invalid markers.
- Add integration target `crates/workflow/engine/tests/example_prompt_composition.rs` with test `actual_example_lua_prompt_includes_each_user_input_once` in package `cowboy-workflow-engine`. Load the real `examples/workflows/workflows/feature.lua` source through `cowboy_workflow_lua::load`, execute its `plan` step through `cowboy_workflow_lua::run_step` using distinct initial-request and follow-up sentinels, resolve the action's compiled role, and call `cowboy_workflow_agent::build_agent_prompt` with the same ordered `RunUserInput` values. Assert the Lua task contains neither sentinel nor a workflow-authored `User Inputs` section, while the final Rust prompt contains each sentinel exactly once.
- Retain `loader::tests::examples_workflows_agent_steps_preserve_and_render_user_feedback`, `loader::tests::examples_workflows_preserve_evidence_through_blocker_detours`, and `loader::tests::examples_workflows_status_detours_preserve_validation_doc` in package `cowboy-workflow-lua`, adapting fixtures only as needed so `user_feedback` remains byte-for-byte unchanged and required artifact/evidence values survive the same routes.
- Strengthen `loader::tests::examples_workflows_reviewer_assesses_evidence_before_rerun` so its duplicate fixture preserves the same `(source, subject_kind, subject_id)` as the original assessment while changing `subject` to a different non-key value. Require rejection before agent dispatch with the tuple-key uniqueness diagnostic.
- Strengthen `loader::tests::examples_workflows_capture_review_and_triage_named_blockers` so malformed reviewer-assessment context asserts exact `blocker_reason` and `blocker_resolution` values containing the concrete `reviewer_assessments[5]` duplicate and original `reviewer_assessments[1]` paths, then assert the user-facing blocked prompt carries those exact actionable diagnostics.
- Do not add tests or changes for unrelated formatter-failing files. Verify formatting through direct checks of all feature-owned Rust files, `git diff --check`, and the non-mutating clean-HEAD failure-set comparison.

# How to verify

1. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_builder_omits_absent_and_rejects_invalid_context -- --exact --nocapture`; confirm paired empty arrays are accepted and each one-sided optionally selected evidence fixture returns a non-agent `changes_requested` action naming the absent field and present partner.
2. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_reviewer_assesses_evidence_before_rerun -- --exact --nocapture`; confirm assessments with an identical tuple key and different `subject` values are rejected.
3. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_capture_review_and_triage_named_blockers -- --exact --nocapture`; confirm exact field-specific `blocker_reason` and `blocker_resolution` values reach the blocked user prompt.
4. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_matrix_uses_only_stage_context -- --exact --nocapture`.
5. Run `cargo test -p cowboy-workflow-engine --test example_prompt_composition actual_example_lua_prompt_includes_each_user_input_once -- --exact --nocapture`; confirm the actual feature-workflow `plan` action omits the request/follow-up sentinels and workflow-authored `User Inputs` content, while the Rust-assembled final prompt contains each sentinel exactly once.
6. Run `cargo test -p cowboy-workflow-agent executor::tests::progress_and_backend_receive_identical_prompt -- --exact --nocapture`; confirm the captured progress prompt exactly equals the mock backend prompt and contains no `<missing-array>` or `<invalid-array:...>` marker.
7. Run `cargo test -p cowboy-workflow-agent prompt::tests::prompt_includes_each_user_input_exactly_once -- --exact --nocapture`.
8. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_render_source_labeled_evidence_in_stable_order -- --exact --nocapture` and `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_allow_manual_only_evidence_without_commands -- --exact --nocapture`.
9. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_agent_steps_preserve_and_render_user_feedback -- --exact --nocapture`, `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_preserve_evidence_through_blocker_detours -- --exact --nocapture`, and `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_status_detours_preserve_validation_doc -- --exact --nocapture`.
10. Run `rustfmt +1.89.0 --edition 2024 --check crates/workflow/agent/src/executor.rs crates/workflow/agent/src/prompt.rs crates/workflow/engine/src/runtime.rs crates/workflow/engine/tests/example_prompt_composition.rs crates/workflow/lua/src/loader.rs`.
11. Execute TODO-08's non-mutating clean-HEAD formatter comparison and require its comparison command to exit 0.
12. Run `git --no-pager diff --check`.
13. Run `cargo test -p cowboy-workflow-lua`.
14. Run `cargo test -p cowboy-workflow-agent`.
15. Run `cargo test -p cowboy-workflow-engine`.
16. Run `cargo clippy -p cowboy-workflow-lua -p cowboy-workflow-agent -p cowboy-workflow-engine --all-targets -- -D warnings`.

# TODO

- [x] TODO-01: Define the stage-aware prompt specification, stable section ordering, selected-field/evidence allow-lists, and safe required-context validation in `examples/workflows/utils/context.lua`.
  - Procedure:
    1. Update `build_agent_prompt` validation so every selected evidence source requires its command/evidence fields as a pair even when the source selection is optional.
    2. Extend `loader::tests::examples_workflows_prompt_builder_omits_absent_and_rejects_invalid_context` with three `review_result_feedback` fixtures: both implementation arrays explicitly empty, `implementation_commands: []` with `implementation_evidence` absent, and `implementation_evidence: []` with `implementation_commands` absent.
    3. Require the paired-empty fixture to produce an agent prompt containing both `array(empty)` headings. Require each one-sided fixture to produce `StepAction::Status` with `changes_requested`, no agent dispatch, and one deterministic failure naming the absent field and its present partner.
    4. Extend `loader::tests::examples_workflows_capture_review_and_triage_named_blockers` with duplicate assessments whose fifth record keeps the first record's `(source, subject_kind, subject_id)` but changes `subject`. Assert exact equality for `blocker_reason` and `blocker_resolution` using the field-specific duplicate diagnostic, and assert the blocked user prompt contains both exact values.
    5. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_builder_omits_absent_and_rejects_invalid_context -- --exact --nocapture` and `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_capture_review_and_triage_named_blockers -- --exact --nocapture`.
  - Expected result: Valid selected context renders in stable order; unselected optional values are omitted; paired real empty arrays render as valid empty arrays; either one-sided selected family is rejected without dispatch and identifies the missing field plus its present partner; malformed required values return deterministic field-level diagnostics; and no prompt contains `<missing-array>` or `<invalid-array:...>`. For the duplicate blocker fixture, `blocker_reason` equals `Agent dispatch was skipped because required workflow context was invalid: reviewer_assessments[5]: expected unique (source, subject_kind, subject_id), got duplicate of reviewer_assessments[1]`, `blocker_resolution` equals `Correct the malformed workflow context and retry the blocked step. Required corrections: reviewer_assessments[5]: expected unique (source, subject_kind, subject_id), got duplicate of reviewer_assessments[1]`, and both values reach the user-facing blocked prompt.
  - Implementer observed result: Both focused tests passed. Explicit empty implementation command/evidence arrays rendered as two `array(empty)` headings. Each one-sided optional implementation family returned `changes_requested` without agent dispatch and produced exactly one failure naming the missing field and its present partner. The blocker regression preserved the named blocker/routing fields and matched the required `reviewer_assessments[5]` duplicate-of-`reviewer_assessments[1]` reason and resolution exactly through the blocked user prompt.

- [x] TODO-02: Remove duplicate original-request rendering from workflow-authored agent prompts while preserving clarification answers and cumulative raw `user_feedback`.
  - Procedure: Add `crates/workflow/engine/tests/example_prompt_composition.rs::actual_example_lua_prompt_includes_each_user_input_once`, then run `cargo test -p cowboy-workflow-engine --test example_prompt_composition actual_example_lua_prompt_includes_each_user_input_once -- --exact --nocapture` followed by `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_agent_steps_preserve_and_render_user_feedback -- --exact --nocapture`.
  - Expected result: The actual feature-workflow `plan` task contains neither the initial-request nor follow-up sentinel and contains no workflow-authored `User Inputs` section; the Rust-assembled final prompt contains each sentinel exactly once, while clarification and `user_feedback` retain their original content and order in selected workflow-context sections.
  - Implementer observed result: Both focused tests passed. The real feature `plan` action omitted both user-input sentinels and `User Inputs` boilerplate, the Rust final prompt contained each sentinel once, and every agent-step fixture preserved scalar and structured raw `user_feedback` entries in order. Structured entries render their scalar fields deterministically and never produce a Lua `table: 0x...` address.

- [x] TODO-03: Migrate planning, RCA, implementation, revision, test, validation, review, blocker-review, result-feedback, and commit agent steps to the unified prompt builder with minimal stage-specific context.
  - Procedure:
    1. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_matrix_uses_only_stage_context -- --exact --nocapture` to exercise every feature, bug-fix, and dev-loop agent-producing stage.
    2. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_builder_omits_absent_and_rejects_invalid_context -- --exact --nocapture` to prove optionally selected one-sided evidence cannot disappear at migrated result-feedback stages.
    3. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_capture_review_and_triage_named_blockers -- --exact --nocapture` and inspect the exact `blocker_reason`, `blocker_resolution`, and blocked-prompt assertions defined in TODO-01.
  - Expected result: Every agent step uses the shared builder, includes all context required by its contract, omits unrelated and future-stage evidence, contains none of the unique `ctx.request` or `ctx.user_inputs` sentinels, contains no workflow-authored `User Inputs` heading/boilerplate, and includes each reusable guidance block no more than once. Migrated stages reject a selected source when either half of its command/evidence pair is absent. The migrated blocker-review stage preserves the named blocker/routing fields and emits the exact field-specific duplicate-assessment `blocker_reason` and `blocker_resolution` defined in TODO-01 rather than a generic or empty prompt.
  - Implementer observed result: All three focused tests passed. The feature, bug-fix, and dev-loop prompt matrix retained the stage allow-lists and exact-once guidance without request/input leakage. Result-feedback rejected either one-sided selected implementation family, and blocker review preserved its named routing fields while carrying the exact duplicate-assessment diagnostics into the user-facing blocked prompt.

- [x] TODO-04: Preserve valid source-specific evidence semantics while stopping placeholder and malformed-value rendering.
  - Procedure:
    1. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_render_source_labeled_evidence_in_stable_order -- --exact --nocapture` and `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_allow_manual_only_evidence_without_commands -- --exact --nocapture`.
    2. In `loader::tests::examples_workflows_prompt_builder_omits_absent_and_rejects_invalid_context`, assert acceptance of explicit paired empty arrays and rejection of both one-sided optionally selected implementation-family states before agent dispatch.
    3. In `loader::tests::examples_workflows_reviewer_assesses_evidence_before_rerun`, create the duplicate by cloning the first assessment, changing only `subject` to `Different non-key subject text`, and appending it. Assert the appended record still has the original `source`, `subject_kind`, and `subject_id`, then require a non-agent `changes_requested` result whose failures include `reviewer_assessments[5]: expected unique (source, subject_kind, subject_id), got duplicate of reviewer_assessments[1]`.
    4. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_prompt_builder_omits_absent_and_rejects_invalid_context -- --exact --nocapture` and `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_reviewer_assesses_evidence_before_rerun -- --exact --nocapture`.
  - Expected result: Selected valid arrays retain semantic values and order; paired empty arrays remain valid; either absent half of a selected family is rejected instead of silently discarding the present half; malformed or duplicate evidence prevents dispatch; reviewer-assessment uniqueness is demonstrably based on `(source, subject_kind, subject_id)` rather than whole-record equality; and unselected evidence sources produce no prompt text.
  - Implementer observed result: All four focused commands passed. Selected evidence retained stable source order, manual-only evidence accepted its explicit empty command array, paired empty arrays remained valid, and both one-sided selected-family states were rejected before dispatch. The duplicate assessment changed only `subject`, retained the original tuple key, and produced the exact `reviewer_assessments[5]` duplicate-of-`reviewer_assessments[1]` failure.

- [x] TODO-05: Verify the Rust final prompt assembler remains the single source of role, cumulative user inputs, task, deliverable format, and blocked-status policy sections.
  - Procedure: Add the engine integration test `actual_example_lua_prompt_includes_each_user_input_once`, add `prompt::tests::prompt_includes_each_user_input_exactly_once`, and rename or extend the executor handoff test as `executor::tests::progress_and_backend_receive_identical_prompt`; run `cargo test -p cowboy-workflow-engine --test example_prompt_composition actual_example_lua_prompt_includes_each_user_input_once -- --exact --nocapture`, `cargo test -p cowboy-workflow-agent prompt::tests::prompt_includes_each_user_input_exactly_once -- --exact --nocapture`, and `cargo test -p cowboy-workflow-agent executor::tests::progress_and_backend_receive_identical_prompt -- --exact --nocapture`.
  - Expected result: The real Lua-to-Rust composition has one copy of every user input, each final prompt has one copy of every top-level section, workflow task text is not rewritten by the transport, and the TUI-visible prompt remains identical to the backend prompt.
  - Implementer observed result: All three composition and handoff tests passed. Each user input and top-level section occurred exactly once, the visible progress prompt equaled the mock backend prompt byte-for-byte, and neither prompt contained missing/invalid array markers.

- [x] TODO-06: Update workflow authoring documentation with the unified builder and prompt-content rules.
  - Procedure:
    1. Update `docs/workflow-authoring.md` with the final unified-builder contract and example.
    2. Run `git --no-pager diff --check -- docs/workflow-authoring.md`.
    3. Manually compare the documented helper signature, selected-field/evidence behavior, and invalid-context behavior with `examples/workflows/utils/context.lua`.
  - Expected result: The documentation accurately states which layer supplies user inputs, how optional and invalid context is handled, and how workflow authors select only valuable stage-specific data.
  - Implementer observed result: The documentation diff check passed, and manual comparison confirmed the documented `build_agent_prompt(ctx, spec)` signature, field/evidence selection, selected command/evidence pair completeness (including optional sources and paired empty arrays), partner-aware invalid-context path, and guidance names match `examples/workflows/utils/context.lua`.

- [x] TODO-07: Run the complete affected test, formatting, and Clippy verification set and resolve every regression or warning.
  - Procedure:
    1. Run `cargo test -p cowboy-workflow-lua`.
    2. Run `cargo test -p cowboy-workflow-agent`.
    3. Run `cargo test -p cowboy-workflow-engine`.
    4. Execute TODO-08's complete ordered non-mutating formatter procedure.
    5. Run `cargo clippy -p cowboy-workflow-lua -p cowboy-workflow-agent -p cowboy-workflow-engine --all-targets -- -D warnings`.
  - Expected result: All focused and crate-level tests pass; Rust 1.89 directly formats every feature-owned Rust file successfully; both pinned workspace formatter executions have consistent status/parsed-set results; the current failure set contains no feature-owned path and no path absent from the clean-HEAD failure set; `git diff --check` passes; Clippy reports no warnings; and captured prompts contain no duplicate user direction, placeholder arrays, invalid-value renderings, or unrelated evidence sections.
  - Implementer observed result: Lua passed 58 tests; agent passed 57 library and 2 binary tests; engine passed 130 library and 1 integration test. TODO-08's complete Rust 1.89 procedure passed with `current_status=1`, `clean_status=1`, and empty current-only and feature-owned failure sets; `git --no-pager diff --check` exited 0. Affected-crate Clippy completed with `-D warnings`.

- [x] TODO-08: Normalize the workspace rustfmt baseline required by the approved TODO-07 formatter gate without changing behavior.
  - Procedure:
    1. Run `cargo +1.89.0 fmt --version` and `rustfmt +1.89.0 --version`; both commands must succeed.
    2. Run `rustfmt +1.89.0 --edition 2024 --check crates/workflow/agent/src/executor.rs crates/workflow/agent/src/prompt.rs crates/workflow/engine/src/runtime.rs crates/workflow/engine/tests/example_prompt_composition.rs crates/workflow/lua/src/loader.rs`.
    3. In one Bash shell, run the following read-only comparison:

       ```bash
       set -euo pipefail
       tmp_dir="$(mktemp -d)"
       clean_dir="$tmp_dir/clean"
       cleanup() {
         git worktree remove --force "$clean_dir" >/dev/null 2>&1 || true
         rm -f "$tmp_dir/current.txt" "$tmp_dir/clean.txt" \
           "$tmp_dir/current-paths.txt" "$tmp_dir/clean-paths.txt" \
           "$tmp_dir/current-only.txt" "$tmp_dir/feature-paths.txt" \
           "$tmp_dir/feature-failures.txt"
         rmdir "$tmp_dir" >/dev/null 2>&1 || true
       }
       trap cleanup EXIT

       cargo +1.89.0 fmt --version >/dev/null
       rustfmt +1.89.0 --version >/dev/null

       set +e
       cargo +1.89.0 fmt --all -- --check >"$tmp_dir/current.txt" 2>&1
       current_status=$?
       set -e

       git worktree add --detach "$clean_dir" HEAD >/dev/null
       set +e
       (cd "$clean_dir" && cargo +1.89.0 fmt --all -- --check) \
         >"$tmp_dir/clean.txt" 2>&1
       clean_status=$?
       set -e

       sed -n 's/^Diff in //p' "$tmp_dir/current.txt" \
         | sed -E 's/:[0-9]+:$//' \
         | sed "s#^$(pwd)/##" \
         | sort -u >"$tmp_dir/current-paths.txt"
       sed -n 's/^Diff in //p' "$tmp_dir/clean.txt" \
         | sed -E 's/:[0-9]+:$//' \
         | sed "s#^$clean_dir/##" \
         | sort -u >"$tmp_dir/clean-paths.txt"
       comm -23 "$tmp_dir/current-paths.txt" "$tmp_dir/clean-paths.txt" \
         >"$tmp_dir/current-only.txt"

       printf '%s\n' \
         crates/workflow/agent/src/executor.rs \
         crates/workflow/agent/src/prompt.rs \
         crates/workflow/engine/src/runtime.rs \
         crates/workflow/engine/tests/example_prompt_composition.rs \
         crates/workflow/lua/src/loader.rs \
         >"$tmp_dir/feature-paths.txt"
       grep -Fxf "$tmp_dir/feature-paths.txt" "$tmp_dir/current-paths.txt" \
         >"$tmp_dir/feature-failures.txt" || true

       validate_result() {
         label="$1"
         status="$2"
         paths="$3"
         output="$4"
         if [ "$status" -eq 0 ] && [ ! -s "$paths" ]; then
           return
         fi
         if [ "$status" -eq 1 ] && [ -s "$paths" ]; then
           return
         fi
         printf '%s formatter result is inconsistent: status=%s\n' \
           "$label" "$status" >&2
         cat "$output" >&2
         exit 1
       }

       validate_result current "$current_status" \
         "$tmp_dir/current-paths.txt" "$tmp_dir/current.txt"
       validate_result clean "$clean_status" \
         "$tmp_dir/clean-paths.txt" "$tmp_dir/clean.txt"

       printf 'current_status=%s\nclean_status=%s\n' "$current_status" "$clean_status"
       printf '%s\n' 'Current-only formatter failures:'
       cat "$tmp_dir/current-only.txt"
       printf '%s\n' 'Feature-owned formatter failures:'
       cat "$tmp_dir/feature-failures.txt"
       test ! -s "$tmp_dir/current-only.txt"
       test ! -s "$tmp_dir/feature-failures.txt"
       ```

    4. Run `git --no-pager diff --check`.
  - Expected result: Rust 1.89's Cargo fmt and rustfmt components are available; no repository file is modified; direct pinned rustfmt checking passes for all five feature-owned Rust files; each workspace formatter result is either status 0 with an empty parsed set or status 1 with a nonempty parsed set; the comparison exits 0 because the current set has no path absent from clean HEAD and no feature-owned path; the temporary worktree is removed; and `git diff --check` reports no whitespace errors.
  - Implementer observed result: Both version commands reported rustfmt 1.8.0 and exited 0. Direct checking passed for all five feature-owned Rust files. The executable read-only comparison exited 0 with `current_status=1`, `clean_status=1`, and empty current-only and feature-owned outputs; its trap removed the detached worktree. `git --no-pager diff --check` also exited 0.
