## Plan

Use the approved RCA at `docs/plans/cowboy_hangs_on_empty_acp_selector_turns/rca.md` and the investigator-added repro test `crates/agent/acp/src/client.rs::client::tests::test_prompt_errors_after_repeated_empty_end_turns` as the fix contract. The bug is in `crates/agent/acp/src/client.rs`: `Client::prompt` recognizes repeated empty `end_turn` turns as continuable, but when `MAX_CONTINUATIONS` is exhausted it returns `Ok(StopReason::EndTurn)` to callers with no collected text. That lets `AgentWorkflowSelector::select` treat the blank response as a successful-but-invalid selector reply and retry the whole selector prompt.

Fix the ACP client continuation loop so an exhausted continuation budget with a still-empty/agent-progress `EndTurn` is a prompt error, not a successful stop reason. Keep the existing recovery behavior for a single empty acknowledgement followed by text, preserve trailing text drain semantics, and keep workflow/TUI crates free of ACP transport policy.

## Changes

- Update `Client::prompt` in `crates/agent/acp/src/client.rs`.
  - Keep `MAX_CONTINUATIONS` and the automatic `Continue` behavior for `PromptTurnOutcome::should_continue()` while attempts remain.
  - Split the current combined condition `!outcome.should_continue() || attempt == MAX_CONTINUATIONS` into two outcomes:
    - return `Ok(outcome.stop_reason)` immediately when `outcome.should_continue()` is false, preserving text, permission-exchange, and non-`end_turn` stop reasons;
    - when `outcome.should_continue()` is still true on `attempt == MAX_CONTINUATIONS`, return an `anyhow` error that names repeated empty ACP `end_turn` responses and includes the continuation budget/session context.
  - Leave `prompt_turn`, `PromptTurnActivity`, and `drain_trailing_events` behavior intact unless the narrow tests show the loop cannot distinguish trailing text from empty turns.
  - Remove or make unreachable the final fallback `Ok(StopReason::EndTurn)` so the exhausted-empty path cannot silently succeed.
- Do not add selector-specific blank-response handling in `crates/workflow/engine/src/workflow.rs` unless the ACP-client error does not propagate through `AgentWorkflowSelector::select`; the repository-grounded source of truth is the ACP client boundary.
- Do not modify TUI runtime logic; AGENTS.md assigns ACP backend behavior to `cowboy-agent-acp` and workflow selection orchestration to `cowboy-workflow-engine`.

## Tests to be added/updated

- Keep the existing investigator-added regression test unchanged as the primary red/green signal: `crates/agent/acp/src/client.rs::client::tests::test_prompt_errors_after_repeated_empty_end_turns`.
- Preserve and run the existing guard tests in `crates/agent/acp/src/client.rs`:
  - `client::tests::test_prompt_continues_empty_end_turn_without_progress` must still pass, proving one empty turn can recover when a continuation produces text.
  - `client::tests::test_prompt_captures_text_streamed_after_response` must still pass, proving late streamed text is drained and does not trigger a spurious `Continue`.
  - `client::tests::test_prompt_agent_error`, `client::tests::test_prompt_max_tokens_stop`, and `client::tests::test_prompt_connection_closed` should continue to pass, proving existing error and non-`end_turn` stop behavior is preserved.
- Add no replacement for the repro test. Only tighten its assertion message if the final error string becomes part of the intended contract.

## How to verify

Run the narrow regression first:

```bash
cargo test -p cowboy-agent-acp test_prompt_errors_after_repeated_empty_end_turns
```

Run the neighboring ACP prompt-behavior guard tests:

```bash
cargo test -p cowboy-agent-acp test_prompt_captures_text_streamed_after_response test_prompt_continues_empty_end_turn_without_progress test_prompt_agent_error test_prompt_max_tokens_stop test_prompt_connection_closed
```

If Cargo rejects multiple bare test filters in one command, run the same five test names as separate `cargo test -p cowboy-agent-acp <test_name>` invocations.

Optionally run the ACP crate test suite after the narrow checks pass:

```bash
cargo test -p cowboy-agent-acp
```

Expected fixed behavior: repeated empty ACP `end_turn` responses produce an error from `Client::prompt`; `AgentWorkflowSelector::select` receives that error via `.map_err(...)` and does not retry a second blank selector attempt as though the prompt succeeded.

## TODO

- [x] Reproduce the current red signal with `cargo test -p cowboy-agent-acp test_prompt_errors_after_repeated_empty_end_turns` before changing implementation code.
- [x] Change `Client::prompt` so non-continuable outcomes return `Ok(outcome.stop_reason)` immediately.
- [x] Change `Client::prompt` so exhausted continuable outcomes return a clear `anyhow` error instead of `Ok(StopReason::EndTurn)`.
- [x] Keep automatic `Continue` prompts unchanged for attempts before `MAX_CONTINUATIONS`.
- [x] Preserve `prompt_turn` trailing-event drain behavior so late `agent_message_chunk` events still count as text.
- [x] Run the investigator-added regression test and confirm it passes.
- [x] Run the neighboring ACP prompt-behavior guard tests listed in this plan.
- [x] If any guard test fails, adjust only the ACP prompt-loop decision logic until repeated-empty turns error while one-empty-turn recovery and late-text handling still pass.
