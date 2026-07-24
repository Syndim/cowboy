## Plan

Fix the blocker-review recovery loop described in `docs/plans/blocker_reviewer_recovery_loops_on_missing_evidence/rca.md` at both points that enable it:

1. Make every workflow-agent result that can route directly to `revise` require the structured implementation command/evidence pair that `revise` validates.
2. Prevent `triage_blocked` from retrying `revise` when that same pair is still missing or malformed; route through `implement` so the implementation context can be rebuilt instead of replaying an unchanged invalid record.

Keep the investigator-added regression test `crates/workflow/lua/src/loader.rs::loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop` unchanged as the primary reproduction and make it pass through the product workflow fix.

## Changes

- Update `examples/workflows/steps/review_implementation.lua` so its output contract requires `implementation_commands` and `implementation_evidence`. This closes the reported `review:changes_requested -> revise` omission in the feature, bug-fix, and dev-loop workflows.
- Audit the other direct agent-produced routes into `revise` and apply the same invariant where it is currently unenforced:
  - `examples/workflows/steps/review_result_feedback.lua` for `changes_requested`.
  - `examples/workflows/steps/validate_goal.lua` for `not_achieved`.
  - Preserve the existing stronger contract in `examples/workflows/steps/test.lua`.
- Reuse the evidence validation rules in `examples/workflows/utils/context.lua` to expose a small predicate or diagnostic helper for checking whether a source-specific command/evidence pair is present and structurally valid. Do not duplicate a weaker table-presence check in the triage step.
- Update `examples/workflows/steps/triage_blocked.lua` so a selected `revise` recovery route is accepted only when implementation evidence passes that shared validation. When it does not, route to `implement`, retain the blocker diagnostics and all present structured fields, and explain in `feedback`/body that implementation context must be reconstructed.
- Preserve `user_feedback` exactly, preserve all present evidence arrays with semantic deep equality and original ordering, and leave `review_blocker:user_required -> blocked` behavior unchanged for genuine external dependencies.

## Tests to be added/updated

- Do not rewrite or replace `loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop`; it must pass after `review_implementation.lua` declares both revision-context arrays as required.
- Add `loader::tests::examples_workflows_other_revise_inputs_require_implementation_context` covering the remaining direct agent routes into `revise`. For `review_result_feedback` in feature, bug-fix, and dev-loop and for dev-loop `validate`, assert that the action output declares `user_feedback`, `implementation_commands`, and `implementation_evidence`, requires both implementation arrays, and retains prompt guidance requiring raw `user_feedback` plus semantic-deep-equality evidence preservation.
- Add a focused loader/runtime test that supplies a recoverable blocker captured from `revise` with missing or malformed implementation evidence and asserts that `triage_blocked` routes to `implement`, not `revise`, while preserving raw `user_feedback`, document references, blocker fields, and every valid evidence array.
- Extend the triage coverage with the valid-context case: when the implementation command/evidence pair is valid, recovery may still route to `revise` and must preserve both arrays without reordering or type changes.
- Retain existing coverage proving that `user_required` blockers still ask the user and that recoverable blockers from other retryable steps continue to return to their originating step.

## How to verify

Run the investigator reproduction first:

```bash
cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop -- --exact
```

Run the new direct-route contract and triage regression tests by their exact names. The triage test must invoke `triage_blocked` once per case and directly assert the returned status, so success cannot come from exhausting a visit budget:

```bash
cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_other_revise_inputs_require_implementation_context -- --exact
cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact
```

Then run the Lua workflow crate checks:

```bash
cargo test -p cowboy-workflow-lua
cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings
```

The fix is complete when the original failing reproduction passes; the direct-route contract test proves all named predecessors require and preserve the revision context contract; a single `triage_blocked` evaluation routes missing and malformed context to `implement` with diagnostics while valid context routes to `revise`; and the crate has no test or Clippy failures.

## TODO

- [x] TODO-01: Require valid implementation command and evidence arrays from every agent output that can route directly into `revise`.
  - Procedure:
    1. Add `implementation_commands` and `implementation_evidence` to the output `required_fields` in `examples/workflows/steps/review_implementation.lua`, `examples/workflows/steps/review_result_feedback.lua`, and `examples/workflows/steps/validate_goal.lua`.
    2. Leave `examples/workflows/steps/test.lua`'s existing stronger required-field contract intact.
    3. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop -- --exact`.
    4. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_other_revise_inputs_require_implementation_context -- --exact`.
    5. In that exact contract test, require these assertions for each applicable action: `output.fields["user_feedback"] == "array"`, `output.fields["implementation_commands"] == "array"`, `output.fields["implementation_evidence"] == "array"`, both implementation field names occur in `output.required_fields`, and the generated prompt contains the existing raw-`user_feedback` and semantic-deep-equality/unchanged-order evidence-preservation guidance.
  - Expected result: both named tests pass; feature, bug-fix, and dev-loop `review`, all three `review_result_feedback` actions, and dev-loop `validate` expose the required implementation pair; and the test fails if any named action drops the raw-user-feedback field, removes either required implementation field, or removes the preservation guidance.
  - Implementer-observed result: Both exact tests passed. The three workflow review actions, all three result-feedback review actions, and dev-loop validation declare both implementation arrays and require them; the focused contract also confirms raw `user_feedback` and semantic-deep-equality preservation guidance.
  - Implementation evidence:
    ```json
    {
      "subject_kind": "todo",
      "subject_id": "TODO-01",
      "subject": "Require valid implementation command and evidence arrays from every agent output that can route directly into `revise`.",
      "source": "implementer",
      "procedure": {
        "kind": "command",
        "steps": [
          "Add `implementation_commands` and `implementation_evidence` to the output `required_fields` in `examples/workflows/steps/review_implementation.lua`, `examples/workflows/steps/review_result_feedback.lua`, and `examples/workflows/steps/validate_goal.lua`.",
          "Leave `examples/workflows/steps/test.lua`'s existing stronger required-field contract intact.",
          "cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop -- --exact",
          "cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_other_revise_inputs_require_implementation_context -- --exact",
          "In that exact contract test, require these assertions for each applicable action: `output.fields[\"user_feedback\"] == \"array\"`, `output.fields[\"implementation_commands\"] == \"array\"`, `output.fields[\"implementation_evidence\"] == \"array\"`, both implementation field names occur in `output.required_fields`, and the generated prompt contains the existing raw-`user_feedback` and semantic-deep-equality/unchanged-order evidence-preservation guidance."
        ]
      },
      "expected_result": "both named tests pass; feature, bug-fix, and dev-loop `review`, all three `review_result_feedback` actions, and dev-loop `validate` expose the required implementation pair; and the test fails if any named action drops the raw-user-feedback field, removes either required implementation field, or removes the preservation guidance.",
      "observed_result": "Both exact tests passed. The three workflow review actions, all three result-feedback review actions, and dev-loop validation declare both implementation arrays and require them; the focused contract also confirms raw `user_feedback` and semantic-deep-equality preservation guidance.",
      "applicability": "applicable",
      "match": "matched",
      "comparisons": []
    }
    ```

- [x] TODO-02: Prevent blocker triage from retrying `revise` with unchanged missing or malformed implementation context.
  - Procedure:
    1. Add a reusable validation helper in `examples/workflows/utils/context.lua` that applies the existing paired-array, command-record, and evidence-record validation rules to a named evidence source.
    2. In `examples/workflows/steps/triage_blocked.lua`, validate implementation context after selecting the recovery destination; if `revise` was selected and validation fails, select `implement` and include the validation diagnostics in the recovery feedback/body.
    3. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact`.
    4. In that exact test, invoke `triage_blocked` once for each of these inputs: both implementation arrays missing, only one paired array present, a non-array implementation field, a malformed command/evidence record, and a valid pair.
    5. Assert every invalid case returns `action.status == "implement"` on that single invocation; `feedback` and body name the failed implementation field/path; exact raw `user_feedback`, blocker metadata, artifact paths, and every unrelated valid evidence array equal the input JSON values; and the valid case returns `action.status == "revise"` with exact implementation-array equality and order.
  - Expected result: the focused test passes and deterministically proves that missing, unpaired, wrong-type, and malformed implementation context route immediately to `implement` with field-specific diagnostics, while valid context routes to `revise` and all preserved fields remain semantically equal.
  - Implementer-observed result: The focused triage test passed. Missing, unpaired, wrong-type, and malformed implementation context returned `implement` after one triage evaluation with field-specific diagnostics; valid context returned `revise`, and raw user feedback, blocker/document fields, and unrelated evidence arrays remained equal.
  - Implementation evidence:
    ```json
    {
      "subject_kind": "todo",
      "subject_id": "TODO-02",
      "subject": "Prevent blocker triage from retrying `revise` with unchanged missing or malformed implementation context.",
      "source": "implementer",
      "procedure": {
        "kind": "command",
        "steps": [
          "Add a reusable validation helper in `examples/workflows/utils/context.lua` that applies the existing paired-array, command-record, and evidence-record validation rules to a named evidence source.",
          "In `examples/workflows/steps/triage_blocked.lua`, validate implementation context after selecting the recovery destination; if `revise` was selected and validation fails, select `implement` and include the validation diagnostics in the recovery feedback/body.",
          "cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact",
          "In that exact test, invoke `triage_blocked` once for each of these inputs: both implementation arrays missing, only one paired array present, a non-array implementation field, a malformed command/evidence record, and a valid pair.",
          "Assert every invalid case returns `action.status == \"implement\"` on that single invocation; `feedback` and body name the failed implementation field/path; exact raw `user_feedback`, blocker metadata, artifact paths, and every unrelated valid evidence array equal the input JSON values; and the valid case returns `action.status == \"revise\"` with exact implementation-array equality and order."
        ]
      },
      "expected_result": "the focused test passes and deterministically proves that missing, unpaired, wrong-type, and malformed implementation context route immediately to `implement` with field-specific diagnostics, while valid context routes to `revise` and all preserved fields remain semantically equal.",
      "observed_result": "The focused triage test passed. Missing, unpaired, wrong-type, and malformed implementation context returned `implement` after one triage evaluation with field-specific diagnostics; valid context returned `revise`, and raw user feedback, blocker/document fields, and unrelated evidence arrays remained equal.",
      "applicability": "applicable",
      "match": "matched",
      "comparisons": []
    }
    ```

- [x] TODO-03: Add contract and recovery regression coverage without modifying the investigator reproduction.
  - Procedure:
    1. In `crates/workflow/lua/src/loader.rs`, add `examples_workflows_other_revise_inputs_require_implementation_context` to assert the required implementation arrays on `review_result_feedback` and dev-loop `validate` outputs.
    2. Add `examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context` with missing, malformed, and valid implementation-context cases; assert destination status, diagnostics, exact `user_feedback`, blocker/document fields, semantic array equality, and stable array order.
    3. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_other_revise_inputs_require_implementation_context -- --exact`.
    4. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact`.
  - Expected result: both new focused tests pass, the invalid cases route to `implement`, the valid case routes to `revise`, and the existing investigator-added test remains present and unchanged.
  - Implementer-observed result: Both new exact tests passed. The recovery cases prove immediate `implement` routing for invalid context and `revise` for valid context. The pre-implementation diff snapshot captured before product-code edits is persisted at `docs/plans/blocker_reviewer_recovery_loops_on_missing_evidence/investigator_repro_preimplementation.patch`, and the current investigator-added reproduction matches that captured test body exactly.
  - Implementation evidence:
    ```json
    {
      "subject_kind": "todo",
      "subject_id": "TODO-03",
      "subject": "Add contract and recovery regression coverage without modifying the investigator reproduction.",
      "source": "implementer",
      "procedure": {
        "kind": "command",
        "steps": [
          "In `crates/workflow/lua/src/loader.rs`, add `examples_workflows_other_revise_inputs_require_implementation_context` to assert the required implementation arrays on `review_result_feedback` and dev-loop `validate` outputs.",
          "Add `examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context` with missing, malformed, and valid implementation-context cases; assert destination status, diagnostics, exact `user_feedback`, blocker/document fields, semantic array equality, and stable array order.",
          "cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_other_revise_inputs_require_implementation_context -- --exact",
          "cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact"
        ]
      },
      "expected_result": "both new focused tests pass, the invalid cases route to `implement`, the valid case routes to `revise`, and the existing investigator-added test remains present and unchanged.",
      "observed_result": "Both new exact tests passed. The recovery cases prove immediate `implement` routing for invalid context and `revise` for valid context. The pre-implementation diff snapshot captured before product-code edits is persisted at `docs/plans/blocker_reviewer_recovery_loops_on_missing_evidence/investigator_repro_preimplementation.patch`, and the current investigator-added reproduction matches that captured test body exactly.",
      "applicability": "applicable",
      "match": "matched",
      "comparisons": []
    }
    ```

- [x] TODO-04: Verify the complete Lua workflow behavior and lint cleanliness.
  - Procedure:
    1. Run `cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact`.
    2. Require the focused test to construct one `triage_blocked` action per invalid case, call `run_step` exactly once for that case, and assert `triage.status == "implement"` immediately; do not implement the test with a retry loop, runner visit counter, or an assertion on an `exceeded max visits` failure.
    3. Run `cargo test -p cowboy-workflow-lua`.
    4. Run `cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings`.
  - Expected result: the exact focused test proves first-triage routing to `implement` before any repeated visit can occur, the complete `cowboy-workflow-lua` suite passes, and Clippy reports no warnings.
  - Implementer-observed result: The exact focused test passed with one `run_step` call per case and immediate `implement` assertions for invalid context. The complete crate suite passed 61 tests, and Clippy completed with no warnings.
  - Implementation evidence:
    ```json
    {
      "subject_kind": "todo",
      "subject_id": "TODO-04",
      "subject": "Verify the complete Lua workflow behavior and lint cleanliness.",
      "source": "implementer",
      "procedure": {
        "kind": "command",
        "steps": [
          "cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_triage_does_not_retry_revise_with_invalid_implementation_context -- --exact",
          "Require the focused test to construct one `triage_blocked` action per invalid case, call `run_step` exactly once for that case, and assert `triage.status == \"implement\"` immediately; do not implement the test with a retry loop, runner visit counter, or an assertion on an `exceeded max visits` failure.",
          "cargo test -p cowboy-workflow-lua",
          "cargo clippy -p cowboy-workflow-lua --all-targets -- -D warnings"
        ]
      },
      "expected_result": "the exact focused test proves first-triage routing to `implement` before any repeated visit can occur, the complete `cowboy-workflow-lua` suite passes, and Clippy reports no warnings.",
      "observed_result": "The exact focused test passed with one `run_step` call per case and immediate `implement` assertions for invalid context. The complete crate suite passed 61 tests, and Clippy completed with no warnings.",
      "applicability": "applicable",
      "match": "matched",
      "comparisons": []
    }
    ```
