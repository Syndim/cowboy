## Plan

Base the fix on `docs/plans/blocker_answer_not_preserved_as_user_feedback/rca.md` and the investigator-added regression test `crates/workflow/lua/src/loader.rs::loader::tests::bugfix_blocker_answer_becomes_cumulative_user_feedback`. Keep that repro test as an input to the fix; do not rewrite or replace it.

Fix the blocker-answer handoff in the example Lua workflow. When the `blocked_answer` step receives the answered `ask_user` record, it must preserve all existing fields, store the raw answer in `blocked_response`, and append that same raw answer to `user_feedback` exactly once for this answered blocker response. The value appended to `user_feedback` must be the unprefixed user answer string, preserving the previous `user_feedback` entries exactly and in order.

Keep `triage_blocked` responsible for selecting the retry route and for exposing `feedback` as the immediate retry instruction. Do not use `feedback` as the durable user-direction channel, do not add blocker reviewer analysis to `user_feedback`, and do not change workflow routing, prompt text, evidence arrays, path fields, or blocker metadata semantics.

## Changes

- In `examples/workflows/steps/blocked.lua`, import the shared workflow context utilities and use the existing `copy_user_feedback` behavior, or an equivalent local copy, in the answered `ask_user` branch.
- In that same answered branch, after verifying the answer is non-empty, set `fields.user_feedback` to the copied prior user-feedback array plus the raw `tostring(answer)` value, then continue setting `fields.blocked_response` and returning the existing `triaged` status action.
- Leave the initial `action.ask_user` branch unchanged except for any import needed by the answered branch; it should continue carrying the current fields forward while waiting for the user.
- Leave `examples/workflows/steps/triage_blocked.lua` routing behavior intact: it should continue to choose `/route <step>` or the captured blocked-from step, preserve evidence/path/blocker fields, emit `feedback = recovery`, and copy the already-updated `user_feedback`.
- Do not edit `crates/workflow/lua/src/loader.rs::loader::tests::bugfix_blocker_answer_becomes_cumulative_user_feedback` except if product-code compilation forces non-behavioral test harness maintenance unrelated to the assertion.

## Tests to be added/updated

- Keep `loader::tests::bugfix_blocker_answer_becomes_cumulative_user_feedback` unchanged and make it pass through product workflow changes.
- Keep the existing blocker-detour evidence preservation test unchanged and passing so the fix does not drop source-specific command/evidence arrays or blocker metadata.
- No additional regression test is required unless implementation uncovers an uncovered edge case; the investigator-added test already exercises the reported data-loss path through `blocked_answer` and `triage_blocked`.

## How to verify

1. Confirm the existing repro test fails before the fix and passes after the fix:
   `cargo test -p cowboy-workflow-lua bugfix_blocker_answer_becomes_cumulative_user_feedback -- --nocapture`
2. Confirm blocker detours still preserve evidence arrays and metadata:
   `cargo test -p cowboy-workflow-lua examples_workflows_preserve_evidence_through_blocker_detours -- --nocapture`
3. Run the changed crate's tests:
   `cargo test -p cowboy-workflow-lua`
4. Run Clippy for the changed crate and fail on diagnostics:
   `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`

## TODO

- [x] TODO-01: Append the raw blocker answer to cumulative user feedback in the answered blocker step.
  - Procedure: update `examples/workflows/steps/blocked.lua` so the `ctx.prev.action == "ask_user"` answered branch copies the incoming `user_feedback` array, appends the non-empty raw `tostring(answer)` value exactly once, keeps `fields.blocked_response = tostring(answer)`, and then run `cargo test -p cowboy-workflow-lua bugfix_blocker_answer_becomes_cumulative_user_feedback -- --nocapture`.
  - Expected result: the command exits 0, `triage_blocked` still returns status `revise`, `feedback` still equals the raw blocker answer, and `user_feedback` equals the prior entries followed by the raw blocker answer.
  - Observed result: `examples/workflows/steps/blocked.lua` now copies existing `user_feedback`, appends the raw non-empty answer exactly once, and stores the same raw answer in `blocked_response`; `cargo test -p cowboy-workflow-lua bugfix_blocker_answer_becomes_cumulative_user_feedback -- --nocapture` exited 0 with 1 focused test passed.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-01","subject":"Append the raw blocker answer to cumulative user feedback in the answered blocker step.","source":"implementer","procedure":{"kind":"command","steps":["Update `examples/workflows/steps/blocked.lua` so the `ctx.prev.action == \"ask_user\"` answered branch copies the incoming `user_feedback` array, appends the non-empty raw `tostring(answer)` value exactly once, keeps `fields.blocked_response = tostring(answer)`, and then run `cargo test -p cowboy-workflow-lua bugfix_blocker_answer_becomes_cumulative_user_feedback -- --nocapture`."]},"expected_result":"The command exits 0, `triage_blocked` still returns status `revise`, `feedback` still equals the raw blocker answer, and `user_feedback` equals the prior entries followed by the raw blocker answer.","observed_result":"`examples/workflows/steps/blocked.lua` now copies existing `user_feedback`, appends the raw non-empty answer exactly once, and stores the same raw answer in `blocked_response`; `cargo test -p cowboy-workflow-lua bugfix_blocker_answer_becomes_cumulative_user_feedback -- --nocapture` exited 0 with 1 focused test passed.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-02: Preserve blocker detour routing, metadata, and evidence fields after the feedback fix.
  - Procedure: inspect `examples/workflows/steps/triage_blocked.lua` and any touched blocker-step logic to ensure only the answered blocker step mutates `user_feedback`, then run `cargo test -p cowboy-workflow-lua examples_workflows_preserve_evidence_through_blocker_detours -- --nocapture`.
  - Expected result: the command exits 0 and proves the blocker detour still preserves implementation/tester/validator/reviewer evidence arrays, path fields, blocker fields, selected retry step, and immediate `feedback` recovery text.
  - Observed result: `triage_blocked.lua` remains responsible for routing and immediate `feedback`, while `blocked.lua` only mutates `user_feedback` in the answered blocker branch; `cargo test -p cowboy-workflow-lua examples_workflows_preserve_evidence_through_blocker_detours -- --nocapture` exited 0 with 1 focused test passed.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-02","subject":"Preserve blocker detour routing, metadata, and evidence fields after the feedback fix.","source":"implementer","procedure":{"kind":"command","steps":["Inspect `examples/workflows/steps/triage_blocked.lua` and any touched blocker-step logic to ensure only the answered blocker step mutates `user_feedback`, then run `cargo test -p cowboy-workflow-lua examples_workflows_preserve_evidence_through_blocker_detours -- --nocapture`."]},"expected_result":"The command exits 0 and proves the blocker detour still preserves implementation/tester/validator/reviewer evidence arrays, path fields, blocker fields, selected retry step, and immediate `feedback` recovery text.","observed_result":"`triage_blocked.lua` remains responsible for routing and immediate `feedback`, while `blocked.lua` only mutates `user_feedback` in the answered blocker branch; `cargo test -p cowboy-workflow-lua examples_workflows_preserve_evidence_through_blocker_detours -- --nocapture` exited 0 with 1 focused test passed.","applicability":"applicable","match":"matched","comparisons":[]}

- [x] TODO-03: Run focused crate verification for the Lua workflow change.
  - Procedure: from the repository root, run `cargo test -p cowboy-workflow-lua`, then run `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`.
  - Expected result: both commands exit 0 with no Rust compiler warnings and no Clippy diagnostics.
  - Observed result: `cargo test -p cowboy-workflow-lua` exited 0 with 55 tests passed across 3 suites; `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings` exited 0 with no diagnostics.
  - Implementation evidence: {"subject_kind":"todo","subject_id":"TODO-03","subject":"Run focused crate verification for the Lua workflow change.","source":"implementer","procedure":{"kind":"command","steps":["Run `cargo test -p cowboy-workflow-lua` from the repository root.","Run `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings` from the repository root."]},"expected_result":"Both commands exit 0 with no Rust compiler warnings and no Clippy diagnostics.","observed_result":"`cargo test -p cowboy-workflow-lua` exited 0 with 55 tests passed across 3 suites; `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings` exited 0 with no diagnostics.","applicability":"applicable","match":"matched","comparisons":[]}
