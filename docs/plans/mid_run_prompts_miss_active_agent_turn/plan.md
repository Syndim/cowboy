# Plan

Use the confirmed root-cause analysis in `docs/plans/mid_run_prompts_miss_active_agent_turn/rca.md` and the investigator-added regression `crates/workflow/engine/src/runtime.rs::accepted_prompt_cancels_active_turn_before_replacement` as the implementation contract. Keep that regression unchanged and fix the production prompt lifecycle beneath it.

After `WorkflowRuntime::submit_user_prompt` durably accepts a prompt, publish its durable sequence to the matching in-process prompt-window control. The active agent turn should observe that sequence, send one ACP `session/cancel` notification for the current `session/prompt`, wait for that original request to return `stopReason: cancelled`, then let the existing compare-and-seal loop load the authoritative prompt batch and issue the next serial `session/prompt` on the same session. The durable store remains the source of truth; the in-process control only shortens the handoff to the next turn.

Follow the protocol requirements in [ACP prompt-turn cancellation](https://agentclientprotocol.com/protocol/v1/prompt-turn#cancellation): once cancellation is sent, every permission request still pending or received before the original prompt completes must receive a `cancelled` outcome, while session updates continue to be consumed until the prompt returns `stopReason: cancelled`.

Make cancellation sequence-aware rather than queueing one cancellation token per submission. Each prompt turn should wait only for a durable sequence newer than the `applied_sequence` captured when that turn starts. This coalesces multiple accepted prompts into the next correction batch, lets prompts accepted during a correction cancel that correction and trigger another replacement, and prevents a late signal for an already-applied prompt from cancelling the replacement turn. If the active turn completes before cancellation is sent, preserve the current post-turn compare-and-seal fallback.

# Changes

- `crates/agent/client/src/traits.rs` and `crates/agent/client/src/lib.rs`
  - Add a provider-neutral, awaitable prompt-turn cancellation input to the `Client::prompt` interface. It should express only “cancel this in-flight turn”; durable run ids, window ids, and prompt sequences remain hidden from backend adapters.
  - Provide a disabled/no-cancellation value for selector, topic-generation, summarization, test, and diagnostic callers that do not have an interactive prompt window.
  - Update every `Client` implementation and callsite identified through the trait seam; do not add a parallel ACP-only prompt interface.
- `crates/agent/acp/src/messages.rs` and `crates/agent/acp/src/client.rs`
  - Add the JSON-RPC notification envelope and camel-case `sessionId` parameters required for `session/cancel`. The notification must have no request id and must not use `session/close`.
  - While `prompt_turn` is waiting in the existing ACP receive loop, select between the next transport message and the turn-cancellation input. On cancellation, send exactly one `session/cancel` notification for the active session and record that cancellation has been sent.
  - Before cancellation, retain the existing permission-selection behavior. After cancellation is sent, respond to every already-buffered or subsequently received `session/request_permission` with `PermissionDecision::Cancelled`; never select an allow option for those requests. Add a direct `PermissionOutcome::cancelled()` constructor if needed so this protocol outcome is explicit rather than reconstructed at callsites.
  - Continue accepting session/tool updates and answering permission requests until the matching original `session/prompt` response arrives with `stopReason: cancelled`. Do not start the replacement prompt while the cancelled turn still has outstanding protocol traffic.
  - Return `StopReason::Cancelled` without the normal long trailing-event drain or automatic `Continue` behavior, so the executor can promptly start the replacement turn. Keep the ACP client connected and retain the same session id.
  - Surface notification/transport failures through the existing prompt error path; do not convert them into session closure or discard the durably accepted prompt.
- `crates/workflow/agent/src/executor.rs` and `crates/workflow/agent/src/lib.rs`
  - Add a cloneable, process-local prompt-turn control registry keyed by run id and opaque window id. Register a sequence watch before emitting `PromptWindowOpened`, initialize it from the window baseline, and unregister it whenever `PromptWindowGuard` closes or drops.
  - Have each initial or correction `run_prompt_turn` derive its cancellation input from the current `applied_sequence`. Publishing a greater accepted sequence wakes the active turn once; compare-and-seal still loads exact prompt content and FIFO ordering from `RunStore`.
  - Keep collecting already-emitted tool, thought, response, and turn records from the cancelled turn, but continue to parse only the latest complete replacement response into the final `StepOutput`.
  - Preserve cleanup on backend errors, retry, explicit run cancellation, and dropped futures so stale controls cannot address a later window.
- `crates/workflow/engine/src/runtime.rs` and `crates/workflow/engine/src/workflow.rs`
  - Store the control registry in `WorkflowRuntime` so runtime clones used by the executing task and TUI submission path share the same registry; pass it into each `AgentExecutor` created by `run_existing_with_events`.
  - Keep `submit_user_prompt` ordering strict: validate non-empty content, append through `RunStore`, and only after `AppendUserPromptOutcome::Accepted` publish that prompt's durable sequence to the matching control. Rejections must not signal cancellation.
  - Treat a missing/already-closed control as the expected completion race: return the durable `Accepted` result and rely on compare-and-seal to deliver the prompt after the completed turn. Do not change prompt bytes, timestamps, counters, window-token validation, or public submission outcomes.
  - Pass the disabled cancellation input from workflow selection, request-topic generation, and summarization, and update the engine's scripted `Client` fake for the revised interface without changing its behavior.
- `docs/architecture.md`
  - Replace the statement that ACP has no active-turn steering mechanism with the implemented lifecycle: durable acceptance, `session/cancel` of the current turn, cancelled response, then a same-session serial replacement prompt. Retain the compare-and-seal race and persistence guarantees.

# Tests to be added/updated

- Keep `crates/workflow/engine/src/runtime.rs::accepted_prompt_cancels_active_turn_before_replacement` unchanged. It must turn from red to green by observing `session/cancel` before the replacement, exact accepted prompt bytes, and reuse of `session-1`; complete-traffic assertions belong in the focused ACP test rather than weakening or expanding this investigator-owned regression.
- Add an ACP client test named `cancelled_prompt_sends_session_cancel_notification_and_reuses_session` using a controlled transport. Deliver a permission request after the client sends cancellation, then return the original prompt's `cancelled` response and permit a later prompt. Inspect the complete outgoing stream and assert: exactly one `session/cancel` notification without an `id`; the post-cancel permission response has outcome `cancelled` and never selects an allow option; no message uses `session/close`; no automatic `Continue` is sent; and the later `session/prompt` uses the original session id.
- Add workflow-agent coverage such as `prompt_cancellation_sequences_coalesce_without_cancelling_replacement`. Exercise multiple accepted sequences around the compare-and-seal handoff and prove that already-applied signals do not cancel the replacement while a newer sequence does.
- Add or extend prompt-window guard coverage for success, backend error, and dropped execution so registry entries are removed together with the durable window cleanup.
- Update all fake `Client` implementations and direct prompt callers for the new cancellation input. Preserve the assertions in `correction_turns_use_verbatim_blocks_and_replace_the_initial_response`, especially verbatim prompt blocks, FIFO sequences, and latest-response replacement.

# How to verify

1. Run the unchanged end-to-end regression:

   ```bash
   cargo test -p cowboy-workflow-engine accepted_prompt_cancels_active_turn_before_replacement -- --nocapture
   ```

   It must show the active-turn control message as `session/cancel`, the original prompt response as cancelled, and the replacement `session/prompt` on the same session with the exact accepted text.

2. Run the focused ACP and workflow-agent cancellation tests added above, then the existing correction contract:

   ```bash
   cargo test -p cowboy-agent-acp cancelled_prompt_sends_session_cancel_notification_and_reuses_session -- --nocapture
   cargo test -p cowboy-workflow-agent prompt_cancellation_sequences_coalesce_without_cancelling_replacement -- --nocapture
   cargo test -p cowboy-workflow-agent correction_turns_use_verbatim_blocks_and_replace_the_initial_response -- --nocapture
   ```

   The ACP test must inspect the complete outgoing message stream, not only the first control message: one id-less `session/cancel`, a cancelled permission response with no allow selection, no `session/close` anywhere, and a later same-session `session/prompt`.

3. Check formatting and all touched Rust interfaces for warnings:

   ```bash
   cargo fmt --all -- --check
   cargo clippy -p cowboy-agent-client -p cowboy-agent-acp -p cowboy-workflow-agent -p cowboy-workflow-engine --tests -- -D warnings
   ```

# TODO

- [x] Extend the provider-neutral `Client::prompt` interface with awaitable turn cancellation and migrate every implementation and caller.
- [x] Implement ACP `session/cancel` notification serialization, receive-loop selection, cancelled-turn completion, and same-session reuse without issuing `session/close`.
- [x] Track whether ACP turn cancellation was sent; thereafter answer every pending or subsequently received permission request with `PermissionDecision::Cancelled`, never allow it, and keep consuming protocol traffic until the original prompt returns `stopReason: cancelled`.
- [x] Implement the sequence-aware workflow-agent prompt-turn control registry and prompt-window guard cleanup.
- [x] Wire the shared registry through `WorkflowRuntime`, publish only durably accepted prompt sequences, and preserve completion-race fallback behavior.
- [x] Keep the investigator regression unchanged while updating client fakes and existing correction tests for the revised interface.
- [x] Add focused ACP coverage for the full cancel/permission/replacement message stream, plus workflow-agent sequence/coalescing and registry-cleanup regressions.
- [x] Update `docs/architecture.md` to describe turn cancellation followed by same-session replacement.
- [x] Run the focused regression, compatibility test, formatter, and Clippy commands listed above and resolve all failures or warnings.
