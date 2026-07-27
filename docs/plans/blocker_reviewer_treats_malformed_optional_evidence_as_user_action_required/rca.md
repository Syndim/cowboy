## Bug behavior

When a workflow enters blocker review with a valid blocker statement but a one-sided optional evidence pair, the blocker-reviewer agent is not dispatched. The workflow classifies the malformed internal context as `user_required` and displays an ask-user prompt saying the blocker reviewer determined that user action is required.

The reported `dev-loop` run had `implementation_evidence` present while `implementation_commands` was missing. Instead of reviewing the actual active-build blocker, the UI asked the user to correct workflow-owned structured context.

## Root cause

`examples/workflows/steps/review_blocker.lua` selects implementation evidence as optional blocker-review context, but the shared prompt builder rejects a selected evidence source whenever only one member of its command/evidence pair is present. When that validation returns no prompt, `review_blocker.lua` hardcodes the invalid-context fallback status to `user_required`.

That classification is incorrect for this failure. The malformed pair is internal persisted workflow state, not a user-only decision, credential, permission, external resource, or manual action. The `user_required` status then routes directly to the ask-user step, so the UI attributes the fallback to the blocker reviewer even though the reviewer agent never ran.

## Root cause evidence

The following flow reconstructs the reported failure. Runtime-specific process identifiers, network addresses, artifact names, and project details are omitted or generalized.

1. The supplied report first shows a real operational blocker:

   ```text
   Blocker:
   A project-local build process remains active and the expected artifact does not exist yet.
   ```

   This is the blocker that `capture_blocker` is meant to preserve for review. `examples/workflows/steps/capture_blocker.lua:4-17` copies all previous fields, writes the previous body to `blocker_statement`, and records the originating step and status.

2. The report then shows that no blocker-review decision was produced:

   ```text
   Reviewer analysis:
   Agent dispatch was skipped because required workflow context was invalid:
   prev.fields.implementation_commands: expected array paired with present
   prev.fields.implementation_evidence, got missing
   ```

   `examples/workflows/steps/review_blocker.lua:13-20` requires only `blocker_statement`, but also selects implementation, tester, validator, and reviewer evidence as optional prompt context. In `examples/workflows/utils/context.lua:684-705`, every selected source is validated as a pair. Lines 690-695 detect that `implementation_evidence` exists while `implementation_commands` is absent and add the exact diagnostic shown above. `build_agent_prompt` returns `nil` when any such error exists at `examples/workflows/utils/context.lua:729-735`, proving the blocker-reviewer agent was skipped.

3. The local fallback converts that internal validation error into the external-user classification:

   ```text
   Required user action:
   Correct the malformed workflow context and retry the blocked step.
   ```

   `examples/workflows/steps/review_blocker.lua:29` calls `invalid_context_action(ctx, "user_required", errors)`. `examples/workflows/utils/context.lua:785-818` constructs the quoted reason and correction text, then returns the caller-provided status unchanged. No reviewer agent participates in this decision.

4. The graph turns the incorrect status into the observed prompt:

   ```text
   The blocker reviewer determined that user action is required.
   ```

   `examples/workflows/workflows/dev-loop.lua:74-78` routes `review_blocker:user_required` to `blocked`. `examples/workflows/steps/blocked.lua:32-52` builds the ask-user message beginning with the quoted sentence. Thus a one-sided optional evidence pair deterministically advances from prompt validation failure, to hardcoded `user_required`, to the misleading user-facing blocked state.

## Reproduction steps

1. Load the example `dev-loop` workflow.
2. Run `review_blocker` with a captured blocker containing `blocker_statement`, `blocked_from_step`, and `blocked_from_status`.
3. Include `implementation_evidence: []` but omit `implementation_commands`, matching the reported one-sided pair.
4. Observe that the prompt builder rejects the pair and `review_blocker` returns a status action instead of dispatching the blocker-reviewer agent.
5. Observe that the returned status is `user_required`, which the workflow routes to the ask-user `blocked` step.
6. Run the focused regression test below.

## Regression test

- Test file path: `crates/workflow/lua/src/loader.rs`
- Test name: `loader::tests::dev_loop_malformed_blocker_context_is_not_user_required`
- Command: `cargo test -p cowboy-workflow-lua loader::tests::dev_loop_malformed_blocker_context_is_not_user_required -- --exact --nocapture`
- Expected failure before the fix: the assertion expects internal malformed workflow context to take the `recoverable` path, but the current implementation returns `user_required`.

## Current failing result

```text
running 1 test

thread 'loader::tests::dev_loop_malformed_blocker_context_is_not_user_required' panicked at crates/workflow/lua/src/loader.rs:1878:9:
assertion `left == right` failed: malformed workflow context is internal state, not a user-only prerequisite
  left: "user_required"
 right: "recoverable"
test loader::tests::dev_loop_malformed_blocker_context_is_not_user_required ... FAILED

failures:
    loader::tests::dev_loop_malformed_blocker_context_is_not_user_required

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 63 filtered out

error: test failed, to rerun pass `-p cowboy-workflow-lua --lib`
```

## Fix constraints

- Do not classify malformed workflow-owned context as `user_required`; reserve that status for a specific unavailable user decision, credential, permission, external resource, or manual action.
- Preserve the original named blocker and `blocked_from_step` / `blocked_from_status` while recovering from context validation failures.
- Preserve `user_feedback` exactly when present. Do not add blocker-reviewer, agent, validation, or fallback-generated text to it.
- Preserve every valid command/evidence array with semantic deep equality and unchanged order; do not invent the missing paired array or silently convert malformed data into valid-looking evidence.
- Keep genuine external blockers routed to the existing ask-user step.
- Avoid a recovery route that immediately re-enters the same step with unchanged context known to fail validation.
- Reconcile the existing malformed-context assertions in `crates/workflow/lua/src/loader.rs` that currently codify `user_required` as expected behavior.
- Product code must remain unchanged during this investigation; only the focused failing test and this RCA are added.
