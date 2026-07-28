# Plan: Escalate repeated no-result retry prompts

## Plan

Implement the bug fix described in the reviewed and user-approved
[`rca.md`](./rca.md). The RCA shows that `ExecutionContext.attempt` reaches
`AgentExecutor::execute_agent`, but the executor converts it to the boolean
`attempt > 1` and calls `build_retry_nudge` without the numeric attempt. As a
result, repeated `NoWorkflowResult` failures receive the same prompt on attempt
2 and attempt 17.

Keep the existing retry accounting, recoverable error classification, reused
agent session, original task, cumulative user inputs, output contract, and
side-effect-safety instructions unchanged. Pass the current numeric attempt into
the retry prompt builder and use it only to escalate the no-result branch:

- On the first retry (`attempt == 2`), retain the current instruction to inspect
  existing work, continue or complete only unfinished work, avoid repeating
  completed side effects, and return a complete workflow result.
- On later no-result retries (`attempt > 2`), identify the actual attempt number
  and use materially stronger wording: returning a parseable workflow result is
  now the highest priority; do not restart the task or repeat completed edits,
  commands, commits, or other side effects; avoid broad new investigation or
  implementation; inspect the existing state, perform only work strictly
  necessary to finish, and immediately emit the required YAML-frontmatter plus
  Markdown result.
- For malformed-frontmatter and other retry reasons, preserve the existing
  correction behavior and wording. The numeric attempt must not route those
  failures through the no-result escalation path.

The investigator-added regression test
`crates/workflow/agent/src/executor.rs::repeated_no_result_retry_prompt_uses_attempt_to_escalate`
is an input to the fix. Do not rewrite, replace, weaken, or remove it.

## Changes

- `crates/workflow/agent/src/executor.rs`
  - Pass `context.attempt` through the existing retry-prompt construction call
    instead of using it only for the `attempt > 1` gate.
  - Do not change session loading/reuse, prompt-window behavior, retry
    accounting, parsing, or error conversion.

- `crates/workflow/agent/src/prompt.rs`
  - Extend `build_retry_nudge` to accept the current attempt number.
  - Keep `is_no_result_reason` as the discriminator between a reply with no
    workflow result and malformed/invalid workflow-result frontmatter.
  - Preserve the current first-retry no-result nudge for `attempt == 2`.
  - Add a later-retry no-result nudge for `attempt > 2` that includes
    `attempt {N}` and explicitly prioritizes immediate result emission while
    retaining the instruction not to repeat completed side effects.
  - Continue appending `build_output_instruction` so allowed statuses, declared
    fields, required fields, blocked-status policy, and YAML formatting remain
    in every retry prompt.
  - Update the helper documentation to describe the attempt-aware escalation
    without changing the malformed-frontmatter branch.

No changes are expected in `crates/workflow/core`,
`crates/workflow/engine`, retry limits, retry counters, or persisted workflow
state.

## Tests to be added/updated

- Keep the investigator-added executor regression test
  `repeated_no_result_retry_prompt_uses_attempt_to_escalate` unchanged. After
  the product fix, it must prove that attempt 17 differs from attempt 2 and
  contains `attempt 17`.
- Update existing `build_retry_nudge` unit-test call sites in
  `crates/workflow/agent/src/prompt.rs` for the new attempt parameter without
  weakening their assertions.
- Add prompt-level coverage that directly locks the escalation policy:
  - attempt 2 with the no-result reason retains the current
    inspect/continue/complete and do-not-repeat-side-effects guidance;
  - a later attempt with the same reason contains its exact attempt number,
    contains the stronger immediate-result priority, and still contains the
    side-effect-safety and YAML/output-contract requirements;
  - a later attempt with a malformed-frontmatter reason continues to use the
    existing re-emission instruction and does not use the repeated no-result
    wording.

## How to verify

Run from the repository root.

1. Run the unchanged investigator regression test:

   ```bash
   cargo test -p cowboy-workflow-agent repeated_no_result_retry_prompt_uses_attempt_to_escalate -- --nocapture
   ```

   Expected result: one test passes; attempt 17 receives a different prompt
   from attempt 2 and the prompt identifies `attempt 17`.

2. Run the retry-prompt unit tests:

   ```bash
   cargo test -p cowboy-workflow-agent retry_nudge
   cargo test -p cowboy-workflow-agent retry_prompt
   ```

   Expected result: all matching tests pass, including first-retry no-result,
   repeated no-result, malformed-frontmatter, output-contract, and
   side-effect-safety assertions.

3. Run the complete affected crate suite:

   ```bash
   cargo test -p cowboy-workflow-agent
   ```

   Expected result: the crate test suite completes with zero failures.

4. Check formatting and lint the affected crate:

   ```bash
   cargo fmt --all -- --check
   cargo clippy -p cowboy-workflow-agent --all-targets -- -D warnings
   ```

   Expected result: both commands exit successfully with no formatting
   differences, compiler warnings, or Clippy warnings.

## TODO

- [x] TODO-01: Thread the numeric retry attempt from `AgentExecutor::execute_agent` into `build_retry_nudge` without changing retry eligibility, session reuse, retry accounting, or the base prompt.
  - Procedure: Change the retry-nudge call in
    `crates/workflow/agent/src/executor.rs` to pass `context.attempt`; inspect the
    surrounding diff with
    `git --no-pager diff -- crates/workflow/agent/src/executor.rs` and confirm
    the existing `context.attempt > 1` gate, `base_prompt`, role/task/user-input
    construction, and execution flow remain intact.
  - Expected result: the prompt builder receives the exact attempt value used
    by the runner event, while no core/engine retry-budget or session-management
    code changes.
  - Observed result: `execute_agent` now passes `context.attempt` directly to
    `build_retry_nudge`; the inspected diff retains the existing
    `context.attempt > 1` gate, `base_prompt`, and surrounding execution flow,
    and contains no core, engine, retry-budget, or session-management changes.

- [x] TODO-02: Make `build_retry_nudge` preserve the current attempt-2 no-result guidance and emit a stronger, attempt-numbered no-result instruction for every attempt greater than 2.
  - Procedure: In `crates/workflow/agent/src/prompt.rs`, add the attempt
    parameter and branch inside `is_no_result_reason(reason)`: retain the
    existing first-retry text for attempt 2; for later attempts include the
    literal current number as `attempt {N}`, make immediate production of the
    parseable workflow result the highest priority, prohibit restarting or
    repeating completed side effects, limit further work to what is strictly
    necessary, and retain the opening/closing `---`, `status`, Markdown body,
    and `build_output_instruction` requirements.
  - Expected result: otherwise-identical no-result prompts for attempts 2 and
    17 are materially different; the attempt-17 prompt contains `attempt 17`,
    stronger immediate-result guidance, side-effect protection, and the full
    workflow output contract.
  - Observed result: `build_retry_nudge` accepts the numeric attempt, preserves
    the prior attempt-2 text verbatim, and emits an attempt-numbered escalation
    for later no-result retries that prioritizes immediate parseable output,
    prohibits restarting or repeating completed side effects, limits further
    work, and still appends the complete output contract.

- [x] TODO-03: Preserve the malformed-frontmatter retry path and add prompt-level tests for the attempt-aware no-result policy without modifying the investigator regression test.
  - Procedure: Update existing `build_retry_nudge` test calls for the new
    signature; add assertions covering attempt 2, a later no-result attempt, and
    a later malformed-frontmatter attempt. Before and after implementation,
    compare the investigator test with
    `git --no-pager diff -- crates/workflow/agent/src/executor.rs` and do not
    edit the test named
    `repeated_no_result_retry_prompt_uses_attempt_to_escalate`.
  - Expected result: prompt tests prove escalation occurs only for repeated
    no-result failures; malformed-frontmatter retries retain their existing
    re-emission wording; the investigator-added test body remains unchanged.
  - Observed result: prompt tests cover the unchanged attempt-2 guidance, the
    attempt-17 escalation and output contract, and an attempt-17 malformed
    frontmatter reason that retains the existing re-emission wording without
    no-result escalation. The durable pre-implementation session event
    `call_FV55nRSmGJ1Igea5NgXKpj9n` records the executor diff containing the
    investigator-added test before product changes; extracting that recorded
    function and comparing it with the current source produces an empty diff,
    proving the test body remains byte-for-byte unchanged.

- [x] TODO-04: Run the unchanged regression test and all retry-prompt tests.
  - Procedure: Run
    `cargo test -p cowboy-workflow-agent repeated_no_result_retry_prompt_uses_attempt_to_escalate -- --nocapture`,
    then `cargo test -p cowboy-workflow-agent retry_nudge`, then
    `cargo test -p cowboy-workflow-agent retry_prompt`.
  - Expected result: every command exits with status 0; the regression reports
    one passing test, and the prompt-focused filters report zero failures.
  - Observed result: the unchanged regression test passed 1/1; the
    `retry_nudge` filter passed 6/6; and the `retry_prompt` filter passed 2/2,
    all with exit status 0.

- [x] TODO-05: Run the affected crate suite, formatting check, and warning-denying Clippy check, then confirm the change is limited to the workflow-agent retry prompt path.
  - Procedure: Run `cargo test -p cowboy-workflow-agent`,
    `cargo fmt --all -- --check`, and
    `cargo clippy -p cowboy-workflow-agent --all-targets -- -D warnings`; then
    inspect `git --no-pager diff --stat` and
    `git --no-pager diff -- crates/workflow/agent/src/executor.rs crates/workflow/agent/src/prompt.rs`.
  - Expected result: all commands exit successfully with zero test failures,
    formatting differences, or warnings; the product diff is confined to
    attempt propagation, retry-prompt escalation, and its tests, with no changes
    to retry budgets, counters, persisted state, or workflow routing.
  - Observed result: the current-state crate suite passed 79 tests across the
    library and test app with zero failures; the approved formatting check and
    warning-denying Clippy check both exited successfully. Final diff inspection
    shows product changes only in `executor.rs` attempt propagation and
    `prompt.rs` retry escalation/tests, with no retry-budget, counter,
    persisted-state, or workflow-routing changes.
