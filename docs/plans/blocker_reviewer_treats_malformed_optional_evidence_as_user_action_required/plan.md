# Plan

Implement the confirmed fix from
`docs/plans/blocker_reviewer_treats_malformed_optional_evidence_as_user_action_required/rca.md`.
Malformed workflow-owned blocker-review context must remain a deterministic local
recovery condition, not a `user_required` blocker. Keep the investigator-added
`crates/workflow/lua/src/loader.rs::loader::tests::dev_loop_malformed_blocker_context_is_not_user_required`
test unchanged as the primary reproduction input.

Preserve strict command/evidence validation. Do not synthesize missing paired
arrays, discard malformed values, or convert invalid data into valid-looking
evidence. Blocker review will classify prompt-validation failure as
`recoverable`; blocker triage will route invalid implementation context to
`implement` before entering `test`, `validate`, or `revise`, which all require a
valid implementation command/evidence pair.

# Changes

- Update `examples/workflows/steps/review_blocker.lua` so only the local
  invalid-prompt fallback classification changes from `user_required` to
  `recoverable`.
- Update `examples/workflows/steps/triage_blocked.lua` to validate implementation
  context before routing to `test`, `validate`, or `revise`. Invalid context must
  route to `implement` with formatted reconstruction diagnostics.
- Leave the strict validators in `examples/workflows/utils/context.lua`
  unchanged. Preserve every incoming field that the existing blocker recovery
  contract carries, including absent fields remaining absent.
- Reconcile existing loader assertions that encode the old malformed-context
  behavior. Keep genuine blocker-reviewer `user_required` results routed to the
  existing ask-user step.

# Tests to be added/updated

- Do not edit or replace
  `loader::tests::dev_loop_malformed_blocker_context_is_not_user_required`. Its
  existing `StepAction::Status` and `recoverable` assertions remain the primary
  regression proof.
- Extend
  `loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context`
  into an exact matrix covering origins `test`, `validate`, and `revise`; both
  arrays missing; each one-sided pair direction; a wrong command-array type; a
  command record missing `procedure_index`; and an evidence record missing
  `observed_result`. Add valid controls for all three origins.
- Update
  `loader::tests::examples_workflows_capture_review_and_triage_named_blockers`
  to prove malformed reviewer context returns `recoverable`, preserves its
  blocker metadata, raw `user_feedback`, diagnostics, and evidence fields, and
  then deterministically triages to `implement`.
- Retain the same named-blocker test's genuine external-access case, which must
  still produce an ask-user action containing the blocker statement, reviewer
  reason, and required user action.
- Retain
  `loader::tests::examples_workflows_route_all_blockers_through_reviewer` as the
  graph proof that `review_blocker:recoverable` routes to `triage_blocked` while
  `review_blocker:user_required` routes to `blocked`.

# How to verify

1. Run
   `cargo test -p cowboy-workflow-lua loader::tests::dev_loop_malformed_blocker_context_is_not_user_required -- --exact --nocapture`.
   The unchanged reproduction must return `StepAction::Status` with status
   `recoverable`.
2. Run
   `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact --nocapture`.
   Every declared invalid case for every evidence-dependent origin must route to
   `implement`, and every valid control must retain its original origin.
3. Run
   `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_capture_review_and_triage_named_blockers -- --exact --nocapture`
   and
   `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_route_all_blockers_through_reviewer -- --exact --nocapture`.
   Malformed internal context must follow recoverable triage, while a genuine
   external prerequisite must still follow the ask-user route.
4. Run `cargo test -p cowboy-workflow-lua`, `cargo fmt --all -- --check`, and
   `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`. Every
   command must exit successfully.

# TODO

- [x] TODO-01: Apply the surgical `recoverable` fallback change and verify the validator and investigator test remain unchanged.
  - Procedure:
    1. Inspect the current worktree and investigator-added loader changes with
       `git --no-pager status --short && git --no-pager diff --stat && git --no-pager diff -- crates/workflow/lua/src/loader.rs`.
    2. In `examples/workflows/steps/review_blocker.lua`, change the status argument
       of the existing `context.invalid_context_action` call from
       `"user_required"` to `"recoverable"`; make no other production change for
       this TODO.
    3. Do not edit or replace
       `loader::tests::dev_loop_malformed_blocker_context_is_not_user_required`.
    4. After isolating the repository-wide failure, rerun
       `git --no-pager status --short && git --no-pager diff --stat && git --no-pager diff -- crates/workflow/lua/src/loader.rs`.
    5. Run
       `git diff --exit-code -- examples/workflows/utils/context.lua` and require
       exit code 0, proving the shared evidence validation implementation was not
       weakened.
    6. Run
       `git diff --unified=0 -- examples/workflows/steps/review_blocker.lua` and
       inspect that the only changed executable line in this file is the fallback
       status argument described in step 2.
    7. Run
       `cargo test -p cowboy-workflow-lua loader::tests::dev_loop_malformed_blocker_context_is_not_user_required -- --exact --nocapture`.
  - Expected result: The validator file has no diff; the blocker-review step diff
    contains only the fallback classification change; and the unchanged
    reproduction passes by receiving `StepAction::Status` with status
    `recoverable`, proving the malformed context does not dispatch an agent or
    enter the `user_required` path.
  - Implementer observed result: `examples/workflows/utils/context.lua` had no
    diff before or after the corrective engine-test update. The zero-context diff
    for `review_blocker.lua` contained only the fallback argument change from
    `user_required` to `recoverable`. The unchanged investigator test passed and
    observed deterministic `StepAction::Status` recovery with status
    `recoverable`.

- [x] TODO-02: Cover 18 malformed origin/fixture combinations and three valid controls, including exact field preservation.
  - Procedure:
    1. In `examples/workflows/steps/triage_blocked.lua`, apply
       `context.validate_evidence_source(fields, "implementation", true)` whenever
       the selected recovery target is `test`, `validate`, or `revise`. On
       validation failure, set the target to `implement` and append the complete
       `context.format_validation_errors(errors)` output to recovery feedback.
    2. In
       `loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context`,
       execute every combination of origins `test`, `validate`, and `revise` with
       these six fixtures: both implementation arrays absent;
       `implementation_commands` absent with `implementation_evidence` present;
       `implementation_evidence` absent with `implementation_commands` present;
       `implementation_commands` set to a non-array value; the first command
       record missing `procedure_index`; and the first evidence record missing
       `observed_result`.
    3. For every invalid combination, assert that the result is
       `StepAction::Status`, the status is `implement`, both `fields.feedback` and
       the action body contain every expected diagnostic field path, and
       `fields.user_feedback` exactly equals the input value.
    4. For every invalid combination, compare `Value::get` on input and output for
       `blocker_statement`, `blocked_from_step`, `blocked_from_status`,
       `blocker_reason`, `blocker_resolution`, `goal`, `validation`, `work_dir`,
       `plan_doc`, `validation_doc`, `rca_doc`, `repro_test`, `files`, all eight
       source command/evidence arrays, and `reviewer_assessments`. This must prove
       deep equality for present values and continued absence for missing values.
    5. Add valid controls for origins `test`, `validate`, and `revise` using
       `sample_evidence_fields()`. Assert each result retains its origin as the
       status and preserves every field named in step 4.
    6. Run
       `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact --nocapture`.
  - Expected result: All 18 invalid origin/fixture combinations route to
    `implement`, expose the exact validation paths in feedback and body, preserve
    raw feedback and every present or absent carried field without synthesis, and
    all three valid controls route to their original `test`, `validate`, or
    `revise` target with unchanged fields.
  - Implementer observed result: The focused matrix passed all six malformed
    fixtures for each of `test`, `validate`, and `revise`. All 18 invalid cases
    routed to `implement`, included every expected diagnostic path in feedback and
    body, preserved raw `user_feedback`, and retained exact `Value::get` equality
    for every carried field. The three valid controls retained their original
    statuses and fields.

- [x] TODO-03: Verify deterministic malformed-context recovery, genuine external ask-user behavior, and graph routing.
  - Procedure:
    1. In
       `loader::tests::examples_workflows_capture_review_and_triage_named_blockers`,
       retain the valid recoverable case and assert it returns status `test` with
       its blocker resolution and planning paths unchanged.
    2. Retain the genuine external-access case and assert it produces
       `StepAction::AskUser`; assert the prompt contains the exact blocker
       statement, blocker reason, and blocker resolution supplied by the
       blocker-reviewer result.
    3. For the malformed reviewer-assessment case, preserve an input snapshot and
       include a nonempty raw `user_feedback` array. Assert `review_blocker`
       returns `StepAction::Status` with status `recoverable`; assert exact
       equality for `blocker_statement`, `blocked_from_step`,
       `blocked_from_status`, and `user_feedback`; assert the exact formatted
       duplicate-assessment diagnostic in both `blocker_reason` and
       `blocker_resolution`; and use `assert_evidence_fields_equal` against the
       input snapshot.
    4. Pass that malformed result to `triage_blocked` with status `recoverable`.
       Assert the result is `StepAction::Status` with status `implement`; assert
       exact equality for the blocker metadata, raw `user_feedback`, diagnostic
       reason/resolution, and all evidence fields against the snapshot. Do not
       invoke the `blocked` step for this malformed case.
    5. Run
       `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_capture_review_and_triage_named_blockers -- --exact --nocapture`.
    6. Run
       `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_route_all_blockers_through_reviewer -- --exact --nocapture`
       and require its transition assertions to show `recoverable ->
       triage_blocked` and `user_required -> blocked` for feature, bug-fix, and
       dev-loop workflows.
  - Expected result: The focused named-blocker test proves malformed internal
    context preserves all declared state and reaches deterministic `implement`
    recovery without constructing an ask-user action; the genuine external case
    still constructs the expected ask-user prompt; and the graph test proves the
    two statuses retain their distinct routes in all three workflows.
  - Implementer observed result: The named-blocker test passed with malformed
    reviewer context preserving blocker metadata, raw `user_feedback`, exact
    diagnostics, and all evidence fields through `recoverable` review and
    deterministic `implement` triage. The external-access case still produced the
    expected ask-user prompt. The graph test passed for feature, bug-fix, and
    dev-loop routes.

- [x] TODO-04: Run the crate suite, formatting check, and warning-denying Clippy.
  - Procedure:
    1. Run
       `cargo test -p cowboy-workflow-engine runtime::tests::example_triage_blocked_reconstructs_implementation_when_revise_evidence_is_missing -- --exact --nocapture`.
    2. Run `cargo test -p cowboy-workflow-lua`.
    3. Run `cargo fmt --all -- --check`.
    4. Run
       `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`.
    5. Run `cargo test`.
  - Expected result: The focused engine integration test exits with status 0, all
    64 `cowboy-workflow-lua` tests pass, the formatting check exits with status 0
    and no diff, warning-denying Clippy exits with status 0 and no diagnostics,
    and the current repository-wide test suite exits with status 0.
  - Implementer observed result: The focused engine integration test passed, all
    64 `cowboy-workflow-lua` tests passed, `cargo fmt --all -- --check` exited 0
    with no diff, warning-denying Clippy exited 0 with no diagnostics, and the
    current repository-wide `cargo test` exited 0.
