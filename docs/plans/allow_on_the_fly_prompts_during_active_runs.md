# Plan

Turn the TUI composer into a prompt channel only while a workflow agent action has an explicitly open prompt window. Plain text accepted through that window belongs to the current run: Cowboy persists it in order, sends it to the same active agent session before that action can finalize, exposes it to every later Lua step, and includes it in every later agent prompt. It must never start a second run.

The workflow engine owns this interface. After Lua evaluates the current step to `StepAction::Agent`, the agent executor opens a durable prompt window identified by an opaque `window_id` and the current `run_id`, `step_record_id`, step, and role. A new `AgentPromptWindowOpened` workflow event gives that token to the TUI. `WorkflowRuntime::submit_user_prompt(run_id, window_id, content)` accepts input only while that exact window is open; it does not infer “queued” versus “stored” from `WorkflowRun::status`. If no window is open (startup, a command/status/ask-user/fail action, between agent actions, after sealing, or after the run ends), plain-text submission is rejected and the composer draft remains intact.

Close the final-drain race with a transactional handoff protocol in the run store:

- Opening a window records its baseline prompt sequence before the initial agent turn starts.
- Prompt submission validates the running run and matching open `window_id`, assigns the next sequence, and appends the exact content in one write transaction.
- After each agent turn, the executor calls a compare-and-seal store operation with the highest sequence applied by that turn. In one transaction, the store either returns every newer accepted prompt and keeps the window open, or seals the window when the applied sequence equals the latest accepted sequence.
- A submission serialized before sealing is returned to the executor for another correction turn. A submission serialized after sealing is rejected, so it cannot be accepted into the gap before `apply_step_record` persists a terminal result.
- Cancellation, backend failure, retry, task abortion, and process recovery close or replace the window. The window token prevents a stale or superseded window from accepting input; resuming under the existing run execution lock clears stale window metadata before opening a new token.

ACP v1 permits one `session/prompt` turn at a time and does not define concurrent steering. Cowboy will therefore send correction turns serially over the same client/session at safe turn boundaries. Preserve the executor's current single mutex around the client map and its prompt call; do not describe it as a per-key lock or refactor synchronization as part of this feature.

Use this exact Lua context contract, ordered by `sequence`:

```lua
ctx.user_inputs = {
  {
    sequence = 0,
    kind = "initial",
    content = "the original request",
    submitted_at = "2026-01-02T03:04:05.000Z",
  },
  {
    sequence = 1,
    kind = "follow_up",
    content = "the first accepted on-the-fly prompt",
    submitted_at = "2026-01-02T03:05:06.000Z",
  },
}
```

Sequence `0` is synthesized from `WorkflowRun::original_request`; its timestamp is `WorkflowRun::created_at`. Durable follow-ups start at `1`; their timestamp is captured when the store accepts them. Serialize timestamps as UTC RFC 3339 with millisecond precision and a `Z` suffix. Preserve `ctx.request` as the unchanged original request. `ask_user` answers remain in `ctx.prev.fields.answer` and are deliberately excluded from `ctx.user_inputs`: this collection covers the initial request and on-the-fly prompts named by this feature, not answers to explicit workflow control points.

Validate emptiness with `content.trim().is_empty()`, but persist and forward the original accepted string byte-for-byte, including leading/trailing whitespace and newlines. Do not truncate, normalize, or reconstruct accepted text.

# Changes

- `crates/workflow/core/src/state.rs` and `crates/workflow/core/src/lib.rs`
  - Add serializable `RunUserPrompt` and active-agent prompt-window domain types. A follow-up record contains `sequence: u64`, `content: String`, and `submitted_at: DateTime<Utc>`; the active window contains its opaque token, run/step/record/role identity, baseline/applied sequence, and open/sealed lifecycle data.
  - Keep `WorkflowRun::original_request` as the initial input and keep concurrently appended prompts outside the replace-on-save run snapshot.
- `crates/workflow/core/src/traits.rs` and all `RunStore` adapters/fakes
  - Extend the existing persistence seam with operations to load ordered prompts, open/abort a prompt window, atomically append through a matching open token, and atomically compare-and-seal or return newer prompts.
  - Define typed outcomes for accepted prompt records, pending correction batches, sealed windows, stale tokens, missing/terminal runs, and unavailable agent windows. Do not expose a speculative “queued versus later step” result.
- `crates/workflow/store/src/tables.rs` and `crates/workflow/store/src/redb_store.rs`
  - Add mutable prompt-history and active-window tables keyed by run id. Assign prompt sequences and validate the run plus token in one redb write transaction.
  - Implement compare-and-seal so append and seal are totally ordered: no prompt accepted before seal can be omitted from the correction loop, and no prompt can be accepted after seal.
  - Preserve legacy databases as empty prompt history/no open window; delete prompt/window indexes with a run. Clear stale window metadata only while holding the existing run execution lock during start/resume recovery.
- `crates/workflow/core/src/engine.rs`
  - Load one ordered prompt snapshot for each dispatch and pass it, together with the original request, to `StepActionProvider::step_action` and `ExecutionContext`, including recoverable retries.
  - Keep `apply_step_record`, transitions, budgets, and run status semantics unchanged; the agent window must already be sealed before an agent `StepRecord` can return for application.
- `crates/workflow/engine/src/runtime.rs` and `crates/workflow/engine/src/events.rs`
  - Add `submit_user_prompt(run_id, window_id, content)`. Use trimming only to reject whitespace-only input, then pass the untouched string to the atomic store append. Do not acquire the run execution lock, advance a step, create a record, consume retry/visit budgets, or alter active-duration accounting.
  - Emit typed prompt-window opened/closed events carrying the current step and opaque token; never include prompt content in lifecycle metadata. Return only durable acceptance (the accepted sequence/record) or a typed rejection.
  - Clear stale windows after acquiring the existing execution guard and before dispatch, and close the current window on cancellation/error cleanup.
- `crates/workflow/engine/src/runner.rs`
  - Materialize the exact `ctx.user_inputs` schema above from `run.original_request` plus the ordered durable prompt records. Keep `ctx.request` unchanged.
  - Ensure later steps and retries reload the full history rather than depending on `ctx.prev` forwarding.
- `crates/workflow/agent/src/prompt.rs` and `crates/workflow/agent/src/executor.rs`
  - Include the complete ordered `ctx.user_inputs` equivalent in every initial agent prompt, regardless of workflow-authored prompt text or role, so every agent step uses all accepted direction.
  - Open the durable window before the initial turn and use its baseline as the highest sequence already present in that base prompt.
  - Give every agent turn a separate response buffer. After the initial turn, call compare-and-seal; when it returns newer prompts, send a correction request on the same session and repeat until sealing succeeds. Parse only the latest turn's complete response as the step result while retaining turns/events from every turn.
  - Build each correction request as `PromptContent` blocks: an instruction block; for each new entry, a metadata label block followed by a raw text block whose value is exactly the stored `content`; and a final response-contract block regenerated from the original `OutputSpec`.
  - Use this correction instruction contract: the entries are new cumulative user direction for the current step; revise work already performed and replace the prior result; return a complete replacement response, not a patch/commentary; and satisfy the original allowed statuses, fields, body expectations, and YAML-frontmatter rules.
  - Keep `StepInput.prompt` as the exact initial composed agent prompt for backward compatibility. Store exact serialized correction `PromptContent` blocks, their applied sequences, role, and window id under `StepInput.context.correction_turns`; record the final applied sequence so replay can prove which inputs influenced the result.
  - Preserve the current client-map mutex across initial and correction prompt calls. Add cleanup guards so all success, failure, cancellation, retry, and dropped-future paths abort or seal the matching window.
- `crates/tui/app/src/app/state.rs`, `crates/tui/app/src/app/input.rs`, and `crates/tui/app/src/app/commands.rs`
  - Replace the boolean submit gate with explicit state derived from three independent facts: whether a background workflow-execution task is running, whether that task has an open agent prompt window, and the latest durable `RunStatusState`. Track background task kind rather than treating the presence of an `active_run_id` as proof of execution; keep the prompt-window token from opened/closed and step/terminal events.
  - Use this submission/command matrix:

    | Workflow execution task | Agent window | Durable run status | Plain text | Slash-command policy |
    | --- | --- | --- | --- | --- |
    | running | open | `Running` | Submit losslessly to the current agent window. | Allow non-conflicting observation/control commands; reject commands that start or mutate workflow execution. |
    | running | closed/absent | any status | Keep the draft; no agent can accept it. | Apply the same execution-conflict policy. |
    | idle | absent | `Running` | Preserve normal idle behavior (a plain request may start a new run). | Allow `/step` and `/resume`; dispatch other commands normally and let runtime status validation remain authoritative. |
    | idle | absent | `WaitingForInput` | Submit through the existing pending-answer fallback. | Allow explicit `/answer` and all other commands; runtime validation remains authoritative. |
    | idle | absent | `Failed` | Preserve normal idle behavior. | Allow `/resolve` (option listing or resolution) and all other commands; runtime validation remains authoritative. |
    | idle | absent | `Completed` or `Cancelled` | Preserve normal idle behavior. | Allow normal command dispatch. |

  - Define “workflow execution task running” from typed background-task metadata, not from active run id or durable `Running` alone. A stepwise run may be durably `Running` with no task executing; waiting and failed runs also retain an active run id while idle.
  - While execution is running, allow `/cancel`, `/help`, `/exit`, `/runs`, `/workflows`, and read-only `/resolve <run-id>` with no status. Reject `/run`, `/step`, `/resume`, `/answer`, `/improve`, and mutating `/resolve <run-id> <status> ...` without dispatch, task creation, or draft clearing. When execution is idle, do not apply this deny list: `/step`, `/resume`, `/answer`, and `/resolve` must remain available in their valid lifecycle states.
  - Classify a submission before mutating the composer. Use a trimmed view only for empty/slash detection. For an on-the-fly prompt, call the runtime with the original draft; clear it and append the original text to history only after durable acceptance. On stale-window, sealed-window, terminal-run, or other rejection, retain the exact draft and show an actionable status.
  - Keep pending `ask_user` answers higher priority than on-the-fly prompts. A leading slash continues to mean a slash command and is never forwarded as agent prompt text.
- `crates/tui/app/src/app/controls/composer.rs` and event projection/rendering
  - Show `Enter sends prompt · Esc cancels` only when a workflow task is executing with an open agent window. During executing-without-agent, keep plain Enter blocked and explain that no agent is accepting prompts. During idle `Running`, `WaitingForInput`, `Failed`, and terminal states, render the existing lifecycle-appropriate idle/answer affordance rather than an active-execution restriction.
  - Preserve the distinct warning state for `WaitingForInput`, cursor/edit/paste behavior, multiline input, and cancellation.
- `docs/workflow-authoring.md` and `docs/architecture.md`
  - Document the exact `ctx.user_inputs` schema, sequence/timestamp rules, whitespace preservation, `ask_user` exclusion, automatic agent-prompt inclusion, durable window handoff, and ACP serial-turn semantics.
  - Document the execution/lifecycle state matrix, including stepwise idle `Running`, `WaitingForInput`, `Failed`, and terminal behavior, plus conflict rejection and draft retention only while execution is actually in progress.

# Tests to be added/updated

- Core/store deterministic handoff tests:
  - exact `RunUserPrompt` JSON/RFC 3339 millisecond representation, sequence `1..N`, stable ordering, and byte-for-byte content round-trip;
  - exact synthesized initial entry at sequence `0` using `run.created_at`;
  - append rejects empty validation at runtime and store rejects missing, terminal, stale-token, sealed-window, and no-window submissions without writing;
  - a prompt appended before compare-and-seal is returned as pending, while an append attempted after seal is rejected;
  - pause specifically after the executor's apparent final drain: append before the seal transaction and prove another correction is required; then seal before append and prove the append is rejected rather than lost before terminal `apply_step_record`;
  - prompt/window tables survive store reopen, work through a second runtime/store handle, load as empty for legacy data, and are removed with the run;
  - stale window cleanup requires the run execution guard and a replacement window token invalidates the old token.
- Core/runner context tests:
  - provider and dispatcher receive the same prompt baseline on initial dispatch and retry;
  - Lua observes the exact schema, sequence, kind, content, timestamps, and order; `ctx.request` remains original;
  - later steps see every accepted follow-up without `ctx.prev` forwarding, while `ask_user` answers remain only in `ctx.prev.fields.answer`.
- Agent prompt/executor tests:
  - every role's base prompt contains sequence `0` plus all prior follow-ups exactly once;
  - prompts accepted during initial and correction turns cause serial same-session correction turns until compare-and-seal succeeds;
  - correction requests contain ordered label/raw blocks, verbatim whitespace, explicit revise/replace instructions, and the original `OutputSpec` frontmatter contract;
  - separate turn buffers prevent concatenated replies; only the latest corrected response is parsed, while all turn records remain captured;
  - `StepInput.prompt` remains the exact initial prompt and `StepInput.context.correction_turns` contains exact correction blocks, sequences, window id, and final applied sequence;
  - no newer prompt produces one turn, and every error/cancellation/drop path closes the active token.
- Runtime integration tests:
  - use barriers to hold a scripted terminal agent after its final check, submit through another `WorkflowRuntime` clone before seal, and prove the same session receives the correction before the terminal record persists;
  - reverse the barrier order so seal wins, then prove submission is rejected and no accepted prompt is stranded;
  - subsequent Lua and agent steps receive initial and follow-up inputs exactly once and in order;
  - whitespace-only submission rejects, while nonempty leading/trailing whitespace and multiline content round-trip unchanged;
  - prompt acceptance changes no step/visit/retry counters and survives reopening the runtime/store.
- TUI input/command/state/render tests:
  - open-window Enter submits the original draft, records history only after acceptance, clears it, renders a prompt card, and creates no second background run;
  - an executing task with a stale/sealed/no-agent window leaves draft/history untouched and shows the correct composer state;
  - pending `ask_user` still routes plain text through `answer_run` before active-prompt handling;
  - while execution is in progress, `/cancel`, `/help`, `/exit`, `/runs`, `/workflows`, and read-only `/resolve <run-id>` remain available;
  - while execution is in progress, `/run`, `/step`, `/resume`, `/answer`, `/improve`, and mutating `/resolve` are rejected without dispatch, background-task creation, or draft loss;
  - after a stepwise task returns a durably `Running` run to idle, `/step` and `/resume` dispatch successfully;
  - while idle in `WaitingForInput`, both plain-answer fallback and explicit `/answer` dispatch successfully;
  - while idle in `Failed`, read-only and mutating `/resolve` dispatch successfully;
  - completed/cancelled idle states retain normal new-request and slash-command behavior;
  - active-window copy advertises prompt submission, executing-no-agent copy blocks plain Enter, and idle lifecycle states do not show an execution-only restriction; cursor, edits, paste, modified Enter, history, and Esc retain their behavior.

# How to verify

- `cargo fmt --all -- --check`
- Run focused redb/core tests for exact schema, verbatim content, token validation, compare-and-seal ordering, stale cleanup, and the final-drain/terminal-finalization race.
- Run focused agent tests for correction content blocks, separate response buffers, same-session serial turns, final output parsing, and cleanup paths.
- Run `cargo test -p cowboy-workflow-engine --features test-support append_at_ -- --nocapture` for deterministic hooks on both sides of the seal transaction and submission through a second runtime/store handle.
- Run focused Cowboy TUI state, input, command, composer, event, and render tests for every row in the execution/window/durable-status matrix, including positive idle `/step`, `/resume`, `/answer`, and `/resolve` cases and executing-task conflict rejection.
- `cargo test -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-agent -p cowboy-workflow-actions -p cowboy-workflow-engine`
- `cargo test -p cowboy`
- `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-agent -p cowboy-workflow-actions -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`
- Manual TUI smoke test with a workflow whose agent step remains active:
  - submit two corrections during initial/correction turns and confirm both affect the current agent's replacement result before it finalizes;
  - press Enter immediately as the agent finishes and confirm the prompt is either accepted and applied or rejected with the exact draft retained—never accepted and lost;
  - confirm later steps receive the complete ordered history;
  - confirm plain text is blocked only while a task executes without an agent window; then verify idle stepwise `/step`/`/resume`, waiting plain answer and `/answer`, failed `/resolve`, normal terminal commands, and cancellation/window cleanup.

# Verification evidence

- `cargo test -p cowboy-workflow-store prompt_append_and_compare_and_seal_are_totally_ordered` — passed; the accepted timestamp is generated inside the write transaction and bounded by the store call.
- `cargo test -p cowboy-workflow-agent --bin execute-agent existing_run_context_uses_durable_request_timestamp_and_follow_ups` — passed with durable sequence-zero request/timestamp data and a nonzero follow-up baseline.
- `cargo test -p cowboy-workflow-engine --features test-support append_at_ -- --nocapture` — 2 passed. Test-support-only hooks pause immediately before and after compare-and-seal. The pre-seal test submits through a second `WorkflowRuntime`/redb handle, forces a same-session correction, and proves subsequent Lua and agent steps each receive the initial request and follow-up exactly once. The post-seal test receives exact `SealedWindow` while the run remains `Running` before `StepRecord` application.
- `cargo test -p cowboy valid_idle_lifecycle_states_dispatch_step_resume_answers_and_terminal_requests` and `cargo test -p cowboy slash_resolve_forwards_typed_fields_and_renders_commands` — passed for valid idle `/step`, `/resume`, plain answer, `/answer`, failed `/resolve`, and completed/cancelled new-request dispatch.
- `cargo test -p cowboy idle_requests_answers_and_allowed_slash_history_remain_trimmed`, `active_agent_prompt_is_persisted_verbatim_without_starting_another_task`, and `rejected_active_prompt_retains_exact_draft_and_history` — passed. Idle request, pending-answer, and allowed slash history use trimmed text; accepted active-agent prompts and rejected drafts retain exact surrounding whitespace.
- `cargo test -p cowboy synchronous_idle_dispatch_error_clears_and_records_trimmed_submission` — passed. A padded missing-run `/resolve` renders the synchronous error, clears the composer, and records only the trimmed command in history.
- `cargo test -p cowboy-workflow-agent correction_turns_use_verbatim_blocks_and_replace_the_initial_response` — passed with ids `record-turn-1` through `record-turn-3` and `prev` values `[None, record-turn-1, record-turn-2]` across the initial response and two corrections.
- Default affected backend suites — 208 passed, 3 ignored. Engine suite with `test-support` — 115 passed. Cowboy TUI suite — 222 passed, 2 ignored.
- `cargo fmt --all -- --check`, default warnings-denied Clippy including `cowboy-workflow-actions`, and warnings-denied engine Clippy with `test-support` passed.
- Manual PTY TUI agent smoke accepted `correction one` during the initial turn and `correction two` during the first correction turn on one ACP session. The final replacement was `second correction result`, and a later step rendered `later=correction one|correction two`.
- A timed manual finish-boundary submission landed after sealing: `finish-boundary-edge-2` was absent from input history and the correction event stream, no second agent turn ran, and the window-closed event was persisted. A separate five-second command-action run retained `blocked draft`, omitted it from history, cancelled successfully, and `/exit` returned 0.
- Manual idle-lifecycle TUI runs advanced seeded `Running` runs with `/step` and `/resume`, completed a TUI-created waiting run through plain text, completed another through `/answer`, completed a failed run through `/resolve`, and started/completed a new terminal-state request; the final terminal session exited with code 0.

# TODO

- [x] Add exact run-user-prompt and active-agent-window domain types, serialization, and public exports.
- [x] Extend `RunStore` and every adapter with ordered prompt history plus open, append, compare-and-seal, abort, and stale-window cleanup operations.
- [x] Implement redb prompt/window tables, transactional token validation and sequencing, legacy-empty behavior, reopen support, and run deletion cleanup.
- [x] Pass one prompt baseline through core provider/dispatch/retry context without changing workflow budgets or transitions.
- [x] Add runtime prompt submission and prompt-window lifecycle events with durable-acceptance-only results.
- [x] Implement execution-guard stale-window cleanup and cancellation/error/drop cleanup.
- [x] Materialize and document the exact `ctx.user_inputs` schema while preserving `ctx.request` and excluding `ask_user` answers.
- [x] Include complete ordered user input in every initial agent prompt.
- [x] Implement transactional compare-and-seal correction loops on the same session with separate response buffers.
- [x] Build correction turns from instruction, ordered label/verbatim-content blocks, and regenerated original output requirements.
- [x] Preserve initial `StepInput.prompt` and persist exact correction-turn blocks plus applied sequence metadata in `StepInput.context`.
- [x] Replace the TUI boolean gate with typed background-execution, prompt-window, and durable-lifecycle submission modes.
- [x] Preserve original drafts/history transactionally across acceptance and rejection, including whitespace.
- [x] Enforce the execution-state command conflict policy without restricting valid idle `/step`, `/resume`, `/answer`, or `/resolve` flows.
- [x] Update composer/event/status rendering for open-agent, executing-no-agent, idle-running, waiting, failed, terminal, rejected, and cancelled states.
- [x] Update workflow-authoring and architecture documentation for the context and handoff contracts.
- [x] Add all deterministic store, core, runner, agent, runtime, and TUI behavioral tests listed above.
- [x] Run formatting, focused race tests, affected-crate suites, manual TUI verification, and Clippy; fix every failure and warning.
