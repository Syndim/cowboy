## Bug behavior

When an agent reply contains no parseable workflow result, Cowboy retries the
step with a corrective prompt. If the reply keeps failing in the same way, every
later retry receives the same corrective prompt as the first retry.

The reported run reached:

```text
attempt 17/18
recoverable action failure: agent reply did not contain a workflow result
```

Despite the high attempt number, the prompt sent on attempt 17 contains no
attempt number and no stronger instruction than attempt 2. Cowboy therefore
spends each remaining retry on another full agent turn without adapting the
prompt to the repeated failure.

## Root cause

The retry attempt number is carried through the core runner in
`ExecutionContext.attempt`, but the workflow-agent executor reduces it to the
boolean condition `context.attempt > 1`. It then calls `build_retry_nudge` with
only the action and previous error text.

`build_retry_nudge` has no attempt parameter, so for a fixed action and the
repeated reason `agent reply did not contain a workflow result`, it deterministically
returns the same text on attempt 2, attempt 17, and every retry between them.
The available signal needed to escalate a repeated no-result prompt is discarded
at the executor-to-prompt-builder boundary.

## Root cause evidence

The supplied log has no backend reply transcript, so the flow is grounded in the
reported retry event, the exact source path that creates it, and the deterministic
regression reproduction.

1. The reported line `attempt 17/18` with
   `recoverable action failure: agent reply did not contain a workflow result`
   proves the current step has repeatedly returned the same recoverable
   no-result failure.
2. `crates/workflow/agent/src/executor.rs`, in `AgentExecutor::execute_agent`,
   passes the visible reply to `parse_frontmatter_output`. A reply with no
   frontmatter is mapped from `Error::MissingFrontmatter` to
   `Error::NoWorkflowResult` before being returned.
3. `crates/workflow/agent/src/error.rs`, in `Error::recoverable`, classifies
   `NoWorkflowResult` as recoverable. Its conversion to
   `WorkflowError::RecoverableAction` produces the reported reason text.
4. `crates/workflow/engine/src/runner.rs`, in `WorkflowRunner::retry_step`,
   increments the retry count, emits `StepRetrying` with the numeric `attempt`,
   and calls `retry_current_step` with both that attempt and the previous error.
   The runner therefore has and forwards the information that this is attempt
   17.
5. `crates/workflow/core/src/engine.rs`, in `retry_current_step` and
   `dispatch_current_step`, stores those values in
   `ExecutionContext { attempt, retry_reason, ... }`. The attempt count is still
   intact when dispatch reaches the agent executor.
6. `crates/workflow/agent/src/executor.rs`, in
   `AgentExecutor::execute_agent`, checks only `context.attempt > 1` and invokes
   `build_retry_nudge(&action, context.retry_reason.as_deref())`. The numeric
   value is not passed.
7. `crates/workflow/agent/src/prompt.rs`, in `build_retry_nudge`, accepts only
   `action` and `reason`. For the no-result reason it always builds the same
   "Inspect the existing work..." instruction.
8. The regression test constructs two otherwise identical execution contexts,
   one with `attempt = 2` and one with `attempt = 17`. The captured prompts are
   byte-for-byte identical. The failing assertion prints the same role, task,
   user input, error reason, and retry instruction on both sides. This directly
   demonstrates that the attempt signal is discarded rather than used to adapt
   the prompt.

## Reproduction steps

1. Build two identical agent execution contexts with the retry reason
   `recoverable action failure: agent reply did not contain a workflow result`.
2. Set the first context to attempt 2 and the second to attempt 17.
3. Execute each context through the real `AgentExecutor` prompt-construction
   path using the existing fake client and a valid final response.
4. Read the prompt persisted in each resulting `StepRecord`.
5. Observe that the attempt-2 and attempt-17 prompts are identical and neither
   identifies attempt 17.

Run the focused automated reproduction described below.

## Regression test

- Test file: `crates/workflow/agent/src/executor.rs`
- Test name:
  `executor::tests::repeated_no_result_retry_prompt_uses_attempt_to_escalate`
- Command:
  `cargo test -p cowboy-workflow-agent repeated_no_result_retry_prompt_uses_attempt_to_escalate -- --nocapture`
- Expected failure before the fix: the assertion requiring the attempt-17
  prompt to differ from the attempt-2 prompt fails because both prompts are
  identical. After that is corrected, the test also requires the escalated
  prompt to identify `attempt 17`.

## Current failing result

The command exits with status 101:

```text
running 1 test

thread 'executor::tests::repeated_no_result_retry_prompt_uses_attempt_to_escalate' panicked:
assertion `left != right` failed: a seventeenth attempt must escalate the
no-result recovery prompt instead of resending the same instruction as attempt
two

left:  "... ## Retry\n\nYour previous turn did not produce a parseable workflow
result (... did not contain a workflow result).\n\nInspect the existing work and
conversation state. Continue or complete any unfinished work ..."
right: "... ## Retry\n\nYour previous turn did not produce a parseable workflow
result (... did not contain a workflow result).\n\nInspect the existing work and
conversation state. Continue or complete any unfinished work ..."

test result: FAILED. 0 passed; 1 failed; 0 ignored; 74 filtered out
```

## Fix constraints

- Preserve the current no-result classification and recoverable retry behavior.
- Preserve session reuse and the instruction not to repeat completed side
  effects.
- Use the existing numeric attempt signal when constructing no-result retry
  prompts; do not alter retry accounting or retry-budget semantics.
- Later repeated no-result attempts must receive an adapted prompt rather than
  the byte-for-byte first-retry prompt, and the prompt must identify the actual
  attempt number.
- Keep the original task, cumulative user inputs, allowed statuses, output
  fields, and YAML-frontmatter contract in the replacement prompt.
- Do not weaken or conflate the separate malformed-frontmatter retry path.
- Do not modify the investigator-added regression test while implementing the
  product fix.
