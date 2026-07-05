## Bug behavior

The reported process was still alive as PID 87922:

```text
ps -p 87922 -o pid=,ppid=,stat=,etime=,command=
87922 77285 S+   24:40 ../cowboy/target/debug/cowboy
```

`sample 87922 3` showed the main thread in `cowboy::app::run_loop` waiting in `crossterm::event::poll`, with Tokio worker threads parked. That means the process was not CPU-spinning; it had returned to the TUI input loop after a background workflow-selection failure.

The process log grounds the user-visible stuck interval. The TUI started a workflow request at `2026-07-04T23:31:29Z`. The agent-backed workflow selector sent a one-shot selection prompt to OMP. OMP repeatedly returned only housekeeping updates plus `end_turn`, with no visible `agent_message_chunk` text. Cowboy responded by sending automatic `Continue` prompts at least five times for the same selector attempt, then retried the selector prompt and repeated the same empty continuation chain. The log ends the selection at `2026-07-04T23:49:17Z` with:

```text
workflow selector: no valid selection after retries attempts=2 reply=<empty>
```

So the observed stuck behavior is a multi-minute apparent hang during workflow selection when the ACP backend repeatedly acknowledges turns without text.

## Root cause

`crates/agent/acp/src/client.rs` treats an ACP prompt turn that ends with `StopReason::EndTurn` and no visible text as recoverable. `Client::prompt` sends `Continue` while `PromptTurnOutcome::should_continue()` is true, up to `MAX_CONTINUATIONS = 5`.

That recovery path is valid for one empty acknowledgement followed by a text-producing continuation, and existing tests cover that case. The bug appears when every continuation is also empty. After exhausting the continuation loop, `Client::prompt` still returns `Ok(StopReason::EndTurn)` instead of surfacing the repeated empty response as an error. The caller receives a successful prompt with no text.

`AgentWorkflowSelector::select` then parses the empty collected text, retries the JSON-selection prompt, and triggers another full ACP empty-continuation loop. In the live PID 87922 run, each empty turn also waited for trailing events before continuing, so two selector attempts consumed minutes before the selector finally failed.

## Reproduction steps

1. Use an ACP backend or mock transport that responds to `session/prompt` with `{"stopReason":"end_turn"}` and no `agent_message_chunk` text for the original prompt and each automatic `Continue` prompt.
2. Call `cowboy_agent_acp::Client::prompt` with any prompt content.
3. Observe that Cowboy sends repeated `Continue` prompts even though none produce text.
4. Observe that after the continuation budget is exhausted, the call returns `Ok(EndTurn)` with no text instead of an error.
5. In the full TUI path, the workflow selector treats that blank successful response as invalid JSON, retries the selector prompt, and repeats the empty continuation chain.

## Regression test

- Test file path: `crates/agent/acp/src/client.rs`
- Test name: `client::tests::test_prompt_errors_after_repeated_empty_end_turns`
- Command: `cargo test -p cowboy-agent-acp test_prompt_errors_after_repeated_empty_end_turns`
- Expected failure before the fix: the new test panics because `Client::prompt` returns `Ok(EndTurn)` after repeated empty ACP `end_turn` responses instead of returning an error.

## Current failing result

```text
cargo test -p cowboy-agent-acp test_prompt_errors_after_repeated_empty_end_turns

running 1 test
failures:

---- client::tests::test_prompt_errors_after_repeated_empty_end_turns stdout ----

thread 'client::tests::test_prompt_errors_after_repeated_empty_end_turns' (251363) panicked at crates/agent/acp/src/client.rs:1111:9:
repeated empty ACP end_turn responses should fail instead of returning success: Ok(EndTurn)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    client::tests::test_prompt_errors_after_repeated_empty_end_turns

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 36 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy-agent-acp --lib`
```

## Fix constraints

- Do not remove the valid recovery path where one empty `end_turn` or tool/progress-only turn is followed by a continuation that produces text.
- Preserve `test_prompt_captures_text_streamed_after_response` so text streamed shortly after the JSON-RPC response is still delivered and does not trigger a spurious `Continue`.
- Preserve `test_prompt_continues_empty_end_turn_without_progress` so a single empty acknowledgement can still be followed by one useful continuation.
- Repeated empty continuations must not be reported as a successful prompt. Surface a clear error or otherwise stop the selector from entering another multi-minute blank retry chain.
- Keep the fix in the ACP client / selector boundary; do not put ACP transport policy into the TUI crate or workflow core.
