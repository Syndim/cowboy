## Bug behavior

A feature workflow can enter a repeated `revise -> capture_blocker -> review_blocker -> triage_blocked -> revise` cycle after implementation review requests changes. In the reported persisted run, `capture_blocker`, `review_blocker`, and `triage_blocked` each executed 18 times, while `revise` reached 20 visits. The run then failed with:

```text
invalid action: step "revise" exceeded max visits (20)
```

The blocker reviewer correctly classified the blocker as locally recoverable, but its recovery route returned to `revise` with the same incomplete structured context, so no iteration could clear the blocker.

## Root cause

The implementation-review agent declares the evidence arrays that a downstream revision requires, but does not mark them as required output fields. Therefore, an otherwise valid `changes_requested` response can be accepted without `implementation_commands` and `implementation_evidence`.

The `revise` step requires those arrays when building its prompt. Missing arrays make prompt construction return an invalid-context `blocked` status. The blocker detour copies only fields that are present; it cannot reconstruct the omitted arrays. `triage_blocked` then routes a recoverable blocker back to its originating `revise` step without checking that the required revision context was restored. The next `revise` invocation receives the same missing arrays and blocks identically.

## Root cause evidence

The persisted records from the reported run show the complete failure flow:

1. Record 22:

   ```text
   step=review status=changes_requested
   fields=[feedback, goal, plan_doc, reviewer_assessments, reviewer_commands, reviewer_evidence, user_feedback, validation]
   ```

   The reviewer requested a revision but omitted `implementation_commands` and `implementation_evidence`. This output was accepted because `examples/workflows/steps/review_implementation.lua:70-73` declares those fields but supplies no `required_fields` list. The agent executor validates presence only for fields listed in `required_fields`.

2. Record 23:

   ```text
   step=revise status=blocked
   prev.fields.implementation_commands: expected array, got missing
   prev.fields.implementation_evidence: expected array, got missing
   ```

   `examples/workflows/steps/revise.lua:18` requires implementation evidence while composing the revision prompt. `examples/workflows/utils/context.lua:549-559` reports missing paired evidence arrays, `build_agent_prompt` returns no prompt at line 589, and `revise.lua:25` converts that validation failure into a `blocked` status.

3. Record 24:

   ```text
   step=capture_blocker status=captured
   blocker_statement=Agent dispatch skipped because selected workflow context was missing or malformed.
   ```

   The feature graph routes `revise:blocked` to `capture_blocker` at `examples/workflows/workflows/feature.lua:57`. The capture step preserves the current fields, which still do not contain either implementation array.

4. Record 25:

   ```text
   step=review_blocker status=recoverable
   blocker_reason=The selected workflow context can be reconstructed locally...
   ```

   The blocker reviewer correctly determines that no user credential, permission, decision, or external resource is needed. Its instructions are prose in `blocker_resolution`; they do not restore the missing structured arrays.

5. Record 26:

   ```text
   step=triage_blocked status=revise
   ```

   `examples/workflows/steps/triage_blocked.lua:34` chooses the captured originating step, and lines 40-58 copy only evidence fields already present. The feature graph routes `triage_blocked:revise` directly to `revise` at `examples/workflows/workflows/feature.lua:68`.

6. Record 27:

   ```text
   step=revise status=blocked
   prev.fields.implementation_commands: expected array, got missing
   prev.fields.implementation_evidence: expected array, got missing
   ```

   This is the same validation failure as record 23. Records 28-90 repeat the blocker-review and recovery route until the visit budget terminates the run. The durable visit counts were `capture_blocker=18`, `review_blocker=18`, `triage_blocked=18`, and `revise=20`.

## Reproduction steps

1. Load the `feature` example workflow.
2. Execute `review` with valid implementation and tester evidence.
3. Return `changes_requested` while omitting `implementation_commands` and `implementation_evidence`. The current review output contract accepts this response because those fields are optional.
4. Execute `revise`. It returns `blocked` with missing-array diagnostics.
5. Route the result through `capture_blocker`, a `recoverable` blocker review, and `triage_blocked`.
6. Execute `revise` again. It returns the same `blocked` result because the recovery path preserved the omission and routed back without restoring valid revision context.
7. Repeating the route eventually fails on the configured per-step visit limit.

The focused regression test reproduces the enabling contract defect directly by asserting that review outputs routed to `revise` must not be accepted without the revision evidence fields.

## Regression test

- Test file: `crates/workflow/lua/src/loader.rs`
- Test name: `loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop`
- Command:

  ```bash
  cargo test -p cowboy-workflow-lua loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop -- --exact
  ```

- Expected failure before the fix: the reviewer output contract has an empty `required_fields` list, so the assertion that `implementation_commands` is required fails.

## Current failing result

```text
running 1 test
test loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop ... FAILED

thread 'loader::tests::examples_workflows_review_output_cannot_drop_revision_context_into_blocker_loop' panicked at crates/workflow/lua/src/loader.rs:787:17:
feature reviewer output must require implementation_commands because a changes_requested result routes directly to revise

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 58 filtered out
error: test failed, to rerun pass `-p cowboy-workflow-lua --lib`
```

## Fix constraints

- Preserve `user_feedback` exactly and do not synthesize reviewer or blocker feedback into it.
- A review result that routes to `revise` must carry the structured implementation context that `revise` validates; prose recovery instructions are not a substitute for required arrays.
- Recoverable blocker routing must not re-enter a step with unchanged context that is already known to fail that step's validation.
- Preserve evidence array contents, semantic types, and ordering across review and blocker detours.
- Keep the existing user-required blocker behavior for genuine missing credentials, permissions, decisions, external resources, or manual actions.
- Do not rely on the visit limit as loop prevention; it is only a terminal safety budget.
- The investigation changes only tests and documentation. Product code remains unchanged.
