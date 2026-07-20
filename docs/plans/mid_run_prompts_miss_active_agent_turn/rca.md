## Bug behavior

Cowboy accepts and durably stores a prompt while an agent step is executing, but it does not notify the active ACP turn. The agent therefore continues without the new direction and may finish additional actions before Cowboy sends the stored text.

Current behavior is post-turn correction:

```text
initial session/prompt runs to completion
  -> Cowboy reads accepted prompts
  -> Cowboy sends a new session/prompt on the same session
  -> agent revises and replaces the completed result
```

The approved behavior is **turn cancellation followed by a replacement turn**:

```text
user prompt is durably accepted
  -> Cowboy sends ACP session/cancel with the active session id
  -> only the ongoing session/prompt turn is aborted
  -> active session/prompt returns stopReason: cancelled
  -> the ACP session remains open with the same session id
  -> Cowboy reads the accepted prompt
  -> Cowboy sends it in the next serial session/prompt on that same session
```

Despite its name, ACP `session/cancel` does **not** close or destroy the session. The session id is only the address identifying which session's current prompt turn to cancel. ACP explicitly defines this notification as “cancel an ongoing prompt turn.” Session destruction is a different operation, `session/close`. After cancellation returns, ACP permits another `session/prompt` on the same session, preserving its conversation context.

This distinction matches the requested behavior: cancel the current **turn**, not the session. The regression uses the same `session-1` for the cancelled prompt and the replacement prompt, and it never issues `session/close`.

## Root cause

`WorkflowRuntime::submit_user_prompt` only appends the prompt through `RunStore::append_user_prompt`. The active `AgentExecutor` is not notified, so it cannot cancel the prompt turn.

`AgentExecutor::execute_agent` awaits `run_prompt_turn`, which owns the mutable provider-neutral `Client` and its ACP receive loop until the matching `session/prompt` response arrives. Only after that await returns does the executor call `compare_and_seal_agent_prompt_window` and send pending prompts as correction turns.

The ACP implementation already recognizes `StopReason::Cancelled`, and standard ACP defines the `session/cancel` **notification** for cancelling an ongoing prompt turn. This notification carries a `sessionId` but does not close that session and has no independent JSON-RPC response; completion is acknowledged when the original `session/prompt` returns `stopReason: cancelled`. Cowboy does not expose or invoke this turn-cancellation operation when a durable prompt is accepted. This missing acceptance-to-turn-cancellation path is the defect.

The ACP operations are separate:

| Operation | Scope | Session remains usable? |
| --- | --- | --- |
| `session/cancel` notification | Current ongoing prompt turn and its unfinished model/tool operations | Yes |
| Original `session/prompt` response with `stopReason: cancelled` | Confirms that turn ended | Yes |
| `session/close` request | Session and associated resources | No |

Reusing `session/prompt` directly is not the fix: the configured Copilot ACP handler aborts the active operation before processing another prompt. The explicit `session/cancel` notification makes that lifecycle intentional and allows Cowboy to wait for cancellation before sending the replacement request.

## Reproduction steps

1. Start a real `WorkflowRuntime` using the production ACP client against a deterministic fake ACP server.
2. Let the initial `session/prompt` report a tool action and its completion while keeping the prompt turn open.
3. Wait for Cowboy's open prompt-window event and completed tool-action event.
4. Submit `steer by cancelling the active turn` through `WorkflowRuntime::submit_user_prompt`; verify durable acceptance.
5. The fake server records the first JSON-RPC message received while the original prompt remains active.
6. Without the fix, no cancellation arrives. After a bounded delay, the fake finishes the original turn, and Cowboy's existing post-turn correction request becomes the first recorded message.
7. The fake returns a corrected replacement response. The test verifies that the final stored result is `corrected` and parses the replacement JSON-RPC request to verify its raw text block equals the accepted prompt exactly.
8. The final assertions check that the first active-turn control message was the `session/cancel` notification for `session-1`, that no `session/close` was sent, and that the replacement `session/prompt` also targets `session-1`.

## Regression test

- Test file: `crates/workflow/engine/src/runtime.rs`
- Test name: `runtime::tests::accepted_prompt_cancels_active_turn_before_replacement`
- Command: `cargo test -p cowboy-workflow-engine accepted_prompt_cancels_active_turn_before_replacement -- --nocapture`
- Expected failure before the fix: the exact accepted prompt is eventually sent and produces the corrected final result, but the first message received during the active turn is another `session/prompt` after timeout rather than `session/cancel`. The assertion fails with `accepted prompt did not cancel the active ACP turn`.

## Current failing result

The focused command fails deterministically in approximately 1.6 seconds:

```text
running 1 test
thread 'runtime::tests::accepted_prompt_cancels_active_turn_before_replacement' panicked:
assertion `left == right` failed: accepted prompt did not cancel the active ACP turn
  left: String("session/prompt")
 right: "session/cancel"
test runtime::tests::accepted_prompt_cancels_active_turn_before_replacement ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 113 filtered out
```

Before reaching the failing assertion, the test proves the existing fallback still forwards the exact accepted content and persists the replacement response. The failure is specifically the missing ACP cancellation, not prompt loss or malformed correction delivery.

## Fix constraints

- Keep the investigator-added regression test unchanged; fix product code rather than weakening the lifecycle assertion.
- After durable acceptance, send the ACP `session/cancel` notification for the matching open window's active session. This cancels its current prompt turn, not the session itself.
- Preserve exact prompt bytes, FIFO sequence ordering, durable timestamps, opaque window-token validation, and compare-and-seal loss prevention.
- Keep the ACP session open and send the replacement prompt with the same session id after the cancelled turn returns.
- Coalesce prompts accepted before replacement handoff according to their durable sequence and keep checking for later accepted prompts until the window seals.
- Preserve the existing post-turn correction fallback when the active turn wins the race and completes before cancellation is sent.
- Keep cleanup correct across cancellation errors, backend failure, retry, explicit run cancellation, and dropped futures.
- Preserve cumulative prompt availability for later Lua and agent steps.
- Preserve `user_feedback` output fields exactly when present; do not synthesize reviewer or agent feedback into them.
