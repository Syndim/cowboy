# Fix plan: watchdog-cancelled turns consume the whole retry budget

Root cause analysis: [`rca.md`](./rca.md) (reviewed and approved by the user).

Investigator-added regression test (an **input** to this fix; do not rewrite or
replace it):

- `crates/agent/acp/src/client.rs`
- `client::tests::watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating`

## Plan

### Why 20 retries could never work

Per the RCA, all 20 charged retries on step `implement` were turns that Cowboy's
own ACP watchdog cancelled after 100 s of stream inactivity while the agent
legitimately waited on slow external work. The backend acknowledges
`session/cancel` by **ending the turn normally** (`stopReason: "end_turn"` plus a
trailing `Info: Operation cancelled by user` chunk) rather than by reporting
`stopReason: "cancelled"`.

`Client::prompt_turn` (`crates/agent/acp/src/client.rs`) only enters soft
recovery when the acknowledgement is literally `cancelled`:

```rust
let stop_reason = result.stop_reason.unwrap_or(StopReason::EndTurn);
if matches!(stop_reason, StopReason::Cancelled) { /* soft recover */ }
break 'monitor stop_reason;
```

So the truncated turn is returned as an ordinary completed turn, the executor
maps the missing frontmatter to the recoverable `Error::NoWorkflowResult`,
`WorkflowRunner::retry_step` charges the run-wide and per-step counters, and the
retry re-enters the same unavoidable wait. The budget drains at the watchdog
period (~114–120 s per attempt) and can never converge.

### Answering the review question: long-running vs stuck

> When watchdog triggered, is it possible for us to understand whether the agent
> is doing some long-running job or it's stucked?

Yes — partially today, and reliably after this change. The distinction has to be
made from two independent signals, because neither alone is sufficient.

**Signal A — does the backend still answer? (authoritative, already available.)**
The watchdog already sends `session/cancel` and waits `cancel_timeout_seconds`.
A backend that answers the cancel at all — with `cancelled`, `end_turn`, or
anything else — is demonstrably alive and responsive; only its *turn* was
truncated. A backend that never answers within the grace window is genuinely
stuck, and that case already escalates correctly to
`hard_recover_and_continue` (force-terminate, restart with `--resume`,
re-initialize, continue). This is exactly the boundary the current code gets
right for `cancelled` and wrong for `end_turn`; TODO-01 fixes that asymmetry, so
"answered the cancel" reliably means "alive, was busy" and "did not answer"
means "stuck".

**Signal B — what was the agent doing when the deadline expired? (available but
currently discarded.)** `parse_session_update_payload`
(`crates/agent/acp/src/messages.rs`) already parses `tool_call` and
`tool_call_update` events including their `toolCallId`, `title`, `kind`, and
`status`. `PromptTurnActivity` (`crates/agent/acp/src/client.rs`) collapses all
of that into a four-state enum (`Empty` / `AgentProgress` /
`PermissionExchange` / `Text`) used only to decide whether to auto-continue. The
information needed to say *"the last thing observed was tool call `<title>`
(kind `execute`) still `in_progress`, 100 s ago"* is therefore already on the
wire and simply thrown away.

An agent blocked on a long external command shows a tool call that reached
`in_progress` and never reached `completed`/`failed`; a stuck or wedged backend
typically shows either no tool call at all, or a completed tool call followed by
silence. That is a strong, cheap heuristic, and it is the difference between an
operator seeing a bare `agent_watchdog_timeout` line and seeing *why* the turn
stalled. TODO-11 captures this last-activity context and TODO-12 surfaces it on
the watchdog log lines and in the soft-recovery record.

### What the watchdog actually does when a tool call is in flight

> From your plan doc I'm not very clear when watchdog times out and finding that
> the agent is waiting on the tool to return, what will the watchdog do?

Stating the current behavior plainly, because it is the weak point: **today, and
with only TODO-01..12, the watchdog cancels the turn regardless.** Detecting an
in-flight tool call would change only the log line, not the action. That is not
good enough:

- Cancelling a turn that is parked inside a tool call kills that tool call. The
  remote build, publish, or device install the agent was waiting on is abandoned
  mid-flight.
- The `Continue` continuation then makes the agent *restart* the wait, which is
  precisely the ~114-120 s loop the RCA measured. Soft recovery (TODO-01) stops
  that loop from draining the retry budget, but it does not stop the loop.
- So the step could still spin indefinitely without progressing, only now
  silently rather than by failing at 20/20.

This plan therefore adds the missing action, with the ownership boundary the
user set: **Cowboy's watchdog does not police tool calls at all.** A tool call
that hangs is the agent's problem to detect and abort — the agent is the only
party that knows what the tool is doing, how long it should take, and whether
killing it is safe. Cowboy's watchdog exists to detect a *dead conversation*,
not a slow tool. So when the watchdog fires and a tool call is in flight, it
simply restarts itself and waits again.

The full deadline-expiry decision becomes:

| State at inactivity deadline | Watchdog action |
| --- | --- |
| A tool call is in flight (last `tool_call` / `tool_call_update` for that id has status `pending` or `in_progress`, no terminal `completed`/`failed`) | **Do not cancel. Restart the watchdog.** Log `agent_watchdog_tool_wait` and re-arm the inactivity deadline for another `response_timeout_seconds`. Repeat for as long as the tool stays in flight. Deciding that the tool itself is stuck, and killing it, is the agent's responsibility, not Cowboy's. |
| No tool call in flight (never started, or the last one already reached `completed`/`failed`) | Unchanged from today: `session/cancel` → grace window → soft or hard recovery. This is the "conversation is stuck" case the watchdog is actually for. |
| Cancel sent, backend answers within the grace window (any stop reason) | Soft recovery: `agent_watchdog_soft_recovered`, `Continue` on the same session, **no retry charged** (TODO-01, the core fix). |
| Cancel sent, backend never answers within the grace window | Hard recovery, unchanged: force-terminate, restart with `--resume=<session-id>`, re-initialize, `Continue`. |

Any parsed ACP activity — including a `tool_call_update` carrying progress —
already resets the inactivity deadline, so the re-arm only engages for a tool
that is genuinely silent while running.

No new configuration field is introduced. There is deliberately **no** tool-wait
ceiling, no per-tool timeout table, and no adaptive timeout: a bounded wait would
put Cowboy back in the business of judging whether someone else's tool call is
too slow, which is exactly the judgement being handed to the agent. The re-arm
is unbounded by design, and its cost is bounded in practice because a tool call
that never terminates keeps the ACP connection healthy and observable — every
re-arm emits an `agent_watchdog_tool_wait` line carrying the tool's id, title,
kind, status, and total waited seconds (TODO-12), so an operator can see exactly
what is being waited on and intervene. See "Accepted consequences" below.

Deliberate scope limit: the restart keys **only** on ACP-reported tool-call
status, which is data already on the wire. This plan does not add process
inspection or heuristics over tool titles.

### Accepted consequences of an unbounded tool wait

Stated explicitly so this is a decision rather than an oversight:

- A backend that reports a tool call as `in_progress` and then wedges forever
  will be waited on forever by the watchdog. Cowboy will not cancel it. The run
  stays parked until the agent aborts the tool, the user cancels the run, or the
  process is killed.
- This is the accepted trade. The previous alternative — cancelling a live tool
  call on a fixed timer — destroyed real work (remote builds, installs,
  long-running searches) and, per the RCA, produced the ~114-120 s cancel loop
  that consumed the entire retry budget. Losing real work on a timer is worse
  than waiting on a tool that the agent should be managing.
- The wait is loud, not silent: `agent_watchdog_tool_wait` is emitted at WARN on
  every re-arm with the tool identity and the cumulative wait, so a genuinely
  stuck tool is diagnosable from the log without re-running at DEBUG.
- User-initiated cancellation is unaffected and remains the escape hatch:
  `WaitOutcome::ExternalCancellation` is a separate arm and the
  `cancellation.try_cancelled()` check still runs on every loop iteration, so
  Ctrl+C / `/cancel` still terminates the turn immediately during a tool wait.

### Fix strategy — one seam, minimum blast radius

1. Make the client's own knowledge that it just sent `session/cancel` decide the
   outcome, instead of trusting the backend's choice of stop reason. Any
   terminal prompt response arriving inside the watchdog cancel-grace window is
   by definition the acknowledgement of a Cowboy-initiated cancel, so it must
   reach the existing `agent_watchdog_soft_recovered` continuation path on the
   same session.
2. Preserve every other branch of the escalation ladder exactly: external
   (user-initiated) cancellation still terminates as `Cancelled` and is never
   auto-continued; cancel-grace timeout, RPC errors, and unusable streams still
   escalate to `hard_recover_and_continue`; a normal `end_turn` with no
   preceding watchdog cancel is still returned as `EndTurn` with its reply and
   trailing-event drain intact.
3. Add last-activity capture and per-turn soft-recovery observability so a slow
   step is diagnosable rather than silent.
4. Act on that capture: while a tool call is genuinely in flight, restart the
   watchdog instead of cancelling, so an agent waiting on slow external work is
   left alone rather than interrupted every `response_timeout_seconds`. Whether
   a stuck tool should be killed is the agent's decision, so no ceiling, no new
   config field, and no tool-level timeout is added.
5. Keep the documented watchdog contract (README + docs + the exactness test in
   `crates/tui/app/src/config.rs`) in sync with both the new acknowledgement
   rule and the tool-wait restart rule.

Explicit non-goals, per the RCA fix constraints:

- Do not enlarge, reset, or otherwise touch retry budgets or config sets.
- Do not change the `NoWorkflowResult` classification, its recoverability, or
  the retry-prompt escalation added by earlier work; they remain correct for
  genuinely completed replies that omit a workflow result.
- Do not modify the investigator-added regression test.
- Do not change the `Client` trait's provider neutrality.
- Do not change the default value of `response_timeout_seconds`,
  `cancel_timeout_seconds`, or `recovery_operation_timeout_seconds`, and do not
  add any new watchdog configuration field. The tool-wait restart is additive
  and engages only while ACP reports a tool call in flight.
- Do not add any Cowboy-side timeout, ceiling, or kill path for tool calls.
  Aborting a stuck tool call is the agent's responsibility.

Why no separate "watchdog-truncated failure" surface is needed: once the
cancelled turn is soft-recovered on the same session, the caller never observes
a failure at all, so nothing is charged to the retry budget. Accumulated turn
text is preserved across the recovery, and `find_frontmatter_open`
(`crates/workflow/agent/src/frontmatter.rs`) already scans past leading
narration and returns the **first** frontmatter block, so a genuine result that
races the cancel is still parsed correctly (covered today by
`parses_frontmatter_after_agent_preamble`).

## Changes

### 1. `crates/agent/acp/src/client.rs` — watchdog cancel-grace acknowledgement

In `prompt_turn`, inside the cancel-grace loop's
`Message::Response { id: resp_id, .. } if resp_id == id` arm:

- Replace the `matches!(stop_reason, StopReason::Cancelled)` gate with a gate on
  "this response acknowledges the watchdog cancel we just sent", which is true
  for **every** stop reason observed in the grace window.
- Keep the external-cancellation precedence first: when
  `external_cancellation_sent` is set, `break 'monitor StopReason::Cancelled`
  regardless of the acknowledged stop reason (today this is only reached for a
  `cancelled` acknowledgement; it must now also cover `end_turn` and the other
  variants).
- Otherwise log `agent_watchdog_soft_recovered` including the observed
  `stop_reason`, the per-turn recovery count, and the captured last-activity
  context, then `dispatch_watchdog_continuation(session_id)` and
  `continue 'monitor`, exactly as the current `cancelled` path does.
- Leave the RPC-error branch (`if let Some(err) = error`) unchanged: it still
  escalates through `hard_recover_and_continue` and sets
  `replacement_continuation_active`.

Add a `watchdog_soft_recoveries: u32` local to `prompt_turn`, incremented on
each soft recovery and emitted as a field on the `agent_watchdog_soft_recovered`
warning. No bound is introduced: a genuinely dead backend never answers the
cancel, so the fixed cancel-grace deadline still escalates to hard recovery;
soft recovery only repeats while the backend is alive and answering, which is
the case this fix must keep alive.

### 2. `crates/agent/acp/src/client.rs` — last-activity capture

Add a small private struct (for example `LastObservedActivity`) alongside
`PromptTurnActivity`, holding the most recent interesting event seen in the
turn: event kind (reuse `event_kind`), and for `Event::ToolCall` /
`Event::ToolCallUpdate` the `tool_call_id`, `title` (tool calls only), `kind`,
and `status`. Update it from the same places that already call
`activity.observe_event(&update)` — both in the main monitor loop and in the
cancel-grace loop — so no new parsing or protocol surface is added.

This type stays private to the ACP crate; nothing is added to the
provider-neutral `Client` trait or to `cowboy-agent-client`.

### 3. `crates/agent/acp/src/client.rs` — watchdog diagnostics

Extend the existing watchdog log lines with the captured context so an operator
can classify a stall without a DEBUG-level rerun:

- `agent_watchdog_timeout` gains `last_event_kind`, `last_tool_call_title`,
  `last_tool_call_kind`, `last_tool_call_status`, and
  `seconds_since_last_activity`.
- `agent_watchdog_soft_recovered` gains `stop_reason`, `soft_recoveries`, and
  the same last-activity fields.

An in-flight tool call (`status` `in_progress` / `pending`) at timeout indicates
a long-running job; no tool call, or a completed one followed by silence,
indicates a likely stuck backend. Record that interpretation as a short comment
next to the struct so the fields are not mistaken for decoration.

### 3a. `crates/agent/acp/src/client.rs` — tool-wait watchdog restart

This is the action half of §2/§3, and the direct answer to "what will the
watchdog do when the agent is waiting on a tool": **it restarts itself and keeps
waiting.**

Give `LastObservedActivity` (§2) a `tool_call_in_flight()` predicate: true when
the most recent tool call for a given `tool_call_id` has status `pending` or
`in_progress` and no later `tool_call_update` for that id reported `completed`
or `failed`. Track the `Instant` at which that tool call was first observed in
flight so the log can report a cumulative wait, and clear it when the tool
reaches a terminal status.

In the `WaitOutcome::WatchdogTimeout` arm of `prompt_turn`, before the existing
`send_prompt_turn_cancellation` call:

- If `tool_call_in_flight()`, log `agent_watchdog_tool_wait` (with the same
  last-activity fields, plus `waited_seconds` counted from first-in-flight) and
  `continue` the monitor loop **without** sending `session/cancel`. The
  `response_deadline` is re-created at the top of each iteration, so this
  naturally re-arms one more inactivity window. There is no bound on how many
  times this can repeat.
- Otherwise proceed into the existing cancel path exactly as today.

There is no ceiling and no new configuration input to this decision: the
predicate is the whole condition. Killing a tool call that never returns is the
agent's decision, and the user's escape hatch is unchanged.

The restart must not be reachable when the caller has requested cancellation:
`WaitOutcome::ExternalCancellation` is a separate arm and is unaffected, and the
`cancellation.try_cancelled()` check further down the loop still runs on every
iteration, so a user cancel during a tool wait still terminates the turn
promptly.

### 3b. No new watchdog configuration field

Explicitly recorded as a decision, because an earlier revision of this plan
proposed one: **no `tool_call_max_wait_seconds` (or equivalent) field is added.**
`AgentWatchdogConfig` (`crates/tui/app/src/config.rs`),
`AgentWatchdogRuntimeConfig` (`crates/workflow/engine/src/runtime.rs`),
`watchdog_options_for` (`crates/workflow/engine/src/runtime_dependencies.rs`),
and `AgentWatchdogOptions` (`crates/agent/acp/src/client.rs`) keep their existing
three fields, so no existing `AgentWatchdogOptions { .. }` test literal, no entry
in the `validate_agents` zero-rejection list, no entry in the
`rejects_zero_agent_watchdog_fields` field list, and no entry in the hard-coded
field list inside `validate_watchdog_document` needs to change. This keeps the
blast radius of the fix inside `cowboy-agent-acp` plus documentation.

### 4. Watchdog contract documentation

The contract block is asserted verbatim by
`crates/tui/app/src/config.rs::tests::documented_agent_watchdog_contract_is_unique_and_exact`
against `README.md`, `docs/architecture.md`, and `docs/module-map.md`, and is
built by `expected_watchdog_contract()` in the same test module. Update all four
in one change so the sentence describes the new rule (any acknowledgement of
Cowboy's `session/cancel`, not only `stopReason: "cancelled"`, is treated as
confirmation and continued on the same session).

Two constraints on the rewording:

- Keep the literal substring ``Recovery first sends exactly\none
  `session/cancel` `` (including the existing line break after "exactly")
  intact, because the test's negative case mutates that exact substring; if it
  stops matching, `str::replace` becomes a no-op, the "invalid" variant equals
  the valid one, and the assertion fails.
- Keep the sentence "This ACP recovery does not consume workflow retry budgets."
  — it is the invariant this bug violated.

### 5. `crates/agent/acp/src/bin/watchdog-fixture.rs` — real-backend smoke mode

Add a third `Mode` variant, `EndTurnCancel` (`--mode end-turn-cancel`), that
reproduces the real backend: on `session/cancel` it emits a final
`agent_message_chunk` (`Info: Operation cancelled by user`) and answers the
pending prompt with `{"stopReason": "end_turn"}`. Wire it through `Mode::parse`,
the usage string, `write_scenario_files`, and the verification expectations (it
must satisfy the same `agent_watchdog_soft_recovered` + `continue_completed`
assertions as `acknowledge-cancel`, and must **not** force-terminate the
transport). Add it as a third `scenario_runner` scenario in `verify`.

In this mode, before going quiet the fixture must also emit one `tool_call`
`session/update` with `status: "in_progress"` and a stable title/kind, and then,
after a short scripted delay, a `tool_call_update` for that same id with
`status: "completed"` — after which it stays silent until the watchdog cancels.
Today the stall path in `handle_request` just stores `*pending_prompt = id` and
streams nothing, so without this the watchdog has no last-activity to report and
the TODO-12 log assertion would have nothing to observe. This two-phase script
mirrors the real failure — an agent blocked inside a long-running tool call, then
blocked with no tool running — and gives `verify_scenario_logs` real fields to
assert on.

**Cross-TODO interaction, must not be missed:** with the TODO-16 restart in
place, an in-flight tool call means the watchdog will *not* cancel, and because
that restart is deliberately unbounded, a fixture that leaves the tool call
`in_progress` forever would hang the scenario forever. The scripted
`completed` update above is what releases it: the watchdog re-arms at least once
while the tool is in flight (emitting `agent_watchdog_tool_wait`), and once the
tool reports `completed` the next inactivity deadline takes the normal cancel
path and the `end_turn` acknowledgement is soft recovered. The scenario
therefore exercises both halves — the tool-wait restart and the cancel/soft
recovery ladder — with no new configuration knob. Choose the delay so the tool
stays in flight across at least one full `response_timeout_seconds` window
(with `response_timeout_seconds = 1` in the generated `[agents.watchdog]` block,
a ~2-3 s delay is sufficient), and do not add `tool_call_max_wait_seconds` to
that block or a `--tool-call-max-wait-seconds` flag to `verify`; neither exists.

## Tests to be added/updated

Do not modify
`client::tests::watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating`;
it must pass unchanged as the primary acceptance signal.

New tests in `crates/agent/acp/src/client.rs` (`mod tests`), following the
existing `ControlledTransport` + `#[tokio::test(start_paused = true)]` pattern:

- `watchdog_cancel_acknowledged_with_end_turn_forwards_truncated_and_continued_text`
  — the event handler receives both the pre-cancel/truncation text and the
  post-continuation text, in order, so no reply content is lost by recovery.
- `watchdog_cancel_acknowledged_with_non_cancelled_stop_reason_recovers`
  — same shape but the backend acknowledges with `max_tokens`, proving the fix
  keys on "we cancelled", not on a specific stop-reason string.
- `watchdog_external_cancellation_during_grace_with_end_turn_ack_returns_cancelled`
  — external cancellation arrives during the grace window and the backend then
  acknowledges with `end_turn`; the client must return `Cancelled`, send no
  continuation, and make no replacement-transport call.
- `watchdog_soft_recovery_count_increments_across_repeated_stalls`
  — two consecutive stalls, each acknowledged with `end_turn`, both soft
  recovered on the same session with `replacement_factory_calls() == 0`.
- `watchdog_last_activity_tracks_in_flight_tool_call`
  — a `tool_call` with `status: "in_progress"` followed by silence leaves the
  captured last-activity as that tool call with its title/kind/status, and a
  later `tool_call_update` with `status: "completed"` replaces it.
- `watchdog_defers_cancel_while_tool_call_in_flight`
  — a `tool_call` with `status: "in_progress"`, then the paused clock advanced
  past `response_timeout_seconds`; the client must send **no** `session/cancel`
  and must still be waiting.
- `watchdog_tool_wait_restarts_indefinitely_while_tool_in_flight`
  — same setup, clock advanced past many multiples of `response_timeout_seconds`
  with the tool still `in_progress`; still no `session/cancel`, proving the
  restart is unbounded and no ceiling was smuggled in.
- `watchdog_cancels_after_in_flight_tool_call_completes`
  — a `tool_call` `in_progress`, one deferral, then a `tool_call_update` with
  `status: "completed"` followed by silence; the next deadline must send
  `session/cancel` exactly once and run the normal ladder.
- `watchdog_cancels_immediately_when_tool_call_completed`
  — a `tool_call` that reaches `completed` followed by silence must cancel at
  the first deadline, with no deferral (the "conversation is stuck" case).
- `watchdog_external_cancellation_during_tool_wait_returns_cancelled`
  — a `tool_call` left `in_progress` across several deferrals, then external
  cancellation; the turn must return `Cancelled` promptly rather than being
  trapped by the unbounded wait.

New test in `crates/agent/acp/src/bin/watchdog-fixture.rs` (`mod tests`):

- `watchdog_fixture_end_turn_cancel_mode_answers_prompt_with_end_turn`
  — drives `handle_request` through initialize/session-new/session-prompt/
  session-cancel in `Mode::EndTurnCancel` and asserts the recorded responses
  contain a text chunk plus `"stopReason": "end_turn"` for the pending prompt
  id, and that the stalling prompt first emitted a `tool_call` update with
  `status: "in_progress"` followed by a `tool_call_update` for the same id with
  `status: "completed"`.

Log-channel assertion (not a `#[test]`, but an executable check): the
`end-turn-cancel` scenario in `verify_scenario_logs` asserts that
`agent_watchdog_tool_wait`, `last_tool_call_status`, and
`seconds_since_last_activity` appear in the real Cowboy log, alongside the
existing `agent_watchdog_timeout` / `agent_watchdog_soft_recovered` assertions.
This is the only reproducible way to observe the TODO-12 and TODO-16 fields:
`cowboy-agent-acp` installs no tracing subscriber in its unit tests, so
`tracing::warn!` is a no-op sink there.

Updated tests:

- `crates/tui/app/src/config.rs::tests::expected_watchdog_contract` — new
  contract wording (and the negative-case substring if the wording forced it to
  move). No new field is added to the TOML block.
- Every existing `AgentWatchdogOptions { .. }` and
  `AgentWatchdogRuntimeConfig { .. }` literal stays as-is; no watchdog
  configuration field is added or removed by this plan.
- `crates/agent/acp/src/bin/watchdog-fixture.rs` tests — extend only where an
  existing assertion enumerates the available modes or the generated
  `[agents.watchdog]` block.

Regression guard (must keep passing unmodified): all 30 existing
`cowboy-agent-acp` watchdog tests, in particular
`watchdog_soft_cancel_continues_same_session`,
`watchdog_soft_normal_completion_wins_ready_timeout_without_cancel`,
`watchdog_soft_external_cancellation_before_timeout_sends_no_continuation`,
`watchdog_soft_prompt_rpc_error_during_cancel_grace_escalates`,
`watchdog_eof_during_cancel_grace_uses_hard_recovery`,
`watchdog_fixed_cancel_grace_ignores_activity`, and
`watchdog_second_stall_after_soft_recovery_is_monitored`.

## How to verify

```bash
cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating -- --nocapture
cargo test -p cowboy-agent-acp watchdog
cargo test -p cowboy-agent-acp
cargo test -p cowboy --lib config
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Optional manual end-to-end smoke (requires a built `cowboy` binary; uses only
the local fixture agent, no external services):

```bash
cargo build --bin cowboy --bin watchdog-fixture
./target/debug/watchdog-fixture verify --cowboy ./target/debug/cowboy --workspace <tmp-dir> \
  --response-timeout-seconds 1 --cancel-timeout-seconds 2 \
  --recovery-operation-timeout-seconds 3 \
  --soft-deadline-seconds 60 --hard-deadline-seconds 120
```

## TODO

- [x] TODO-01: Route watchdog cancel acknowledgements that are not `cancelled` (notably `end_turn`) into the existing `agent_watchdog_soft_recovered` continuation path in the cancel-grace loop of `Client::prompt_turn`, while keeping `external_cancellation_sent` terminating the turn as `Cancelled` for every acknowledged stop reason.
  - Procedure: edit the `Message::Response { id: resp_id, .. } if resp_id == id` arm inside the cancel-grace loop in `crates/agent/acp/src/client.rs` so the recovery gate is "a terminal prompt response observed in the watchdog grace window" rather than `matches!(stop_reason, StopReason::Cancelled)`; keep the `external_cancellation_sent` check first; leave the RPC-error escalation to `hard_recover_and_continue` unchanged. Then run `cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating -- --nocapture`.
  - Expected result: that test passes, i.e. after the watchdog `session/cancel` the client sends a `session/prompt` continuation carrying `CONTINUE_PROMPT` on `sess_1`, the turn finally returns `Ok(EndTurn)`, and `client.replacement_factory_calls() == 0`.
  - Observed result (implementer): the cancel-grace `Message::Response { id: resp_id, .. } if resp_id == id` arm now checks `external_cancellation_sent` first and otherwise treats any terminal stop reason as acknowledgement of Cowboy's own cancel. `cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating -- --nocapture` exited 0 with `test result: ok. 1 passed; 0 failed` (the investigator repro test was not edited; it failed before this change).

- [x] TODO-02: Add a per-turn soft-recovery counter and include the observed stop reason in the `agent_watchdog_soft_recovered` log.
  - Procedure: add a `watchdog_soft_recoveries: u32` local to `prompt_turn`, increment it before each `dispatch_watchdog_continuation` on the soft path, and emit it plus `stop_reason` as fields of the existing `tracing::warn!(event = "agent_watchdog_soft_recovered", ...)`. Run `cargo test -p cowboy-agent-acp watchdog`.
  - Expected result: the crate compiles with no warnings, all watchdog tests pass, and the emitted warning carries `event="agent_watchdog_soft_recovered"` together with the `stop_reason` and recovery-count fields.
  - Observed result (implementer): `watchdog_soft_recoveries: u32` added to `prompt_turn`, incremented immediately before each `dispatch_watchdog_continuation`, and emitted as `soft_recoveries` alongside `stop_reason` on the `agent_watchdog_soft_recovered` warning. `cargo test -p cowboy-agent-acp --lib watchdog` exited 0 with `test result: ok. 41 passed; 0 failed`, and `cargo clippy -p cowboy-agent-acp --all-targets -- -D warnings` exited 0.

- [x] TODO-03: Add a client test proving no reply text is lost across an `end_turn`-acknowledged soft recovery.
  - Procedure: add `watchdog_cancel_acknowledged_with_end_turn_forwards_truncated_and_continued_text` to `crates/agent/acp/src/client.rs` `mod tests`, using `ControlledTransport` and `#[tokio::test(start_paused = true)]`; capture forwarded events in a shared `Vec<String>` handler; script pre-cancel text, watchdog timeout, `session/cancel`, a truncated chunk plus `prompt_response(1, "end_turn")`, then the continuation's text and `prompt_response(2, "end_turn")`. Run `cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_end_turn_forwards_truncated_and_continued_text`.
  - Expected result: the test passes and asserts the captured text contains the pre-cancel chunk, the truncated cancellation notice, and the post-continuation chunk in that order, with `replacement_factory_calls() == 0`.
  - Observed result (implementer): `watchdog_cancel_acknowledged_with_end_turn_forwards_truncated_and_continued_text` added to `mod tests` using `ControlledTransport` and `#[tokio::test(start_paused = true)]`. `cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_end_turn_forwards_truncated_and_continued_text` exited 0 with `test result: ok. 1 passed; 0 failed`.

- [x] TODO-04: Add a client test proving the fix keys on Cowboy's own cancel rather than on a specific stop-reason string.
  - Procedure: add `watchdog_cancel_acknowledged_with_non_cancelled_stop_reason_recovers` to `crates/agent/acp/src/client.rs` `mod tests`, identical in shape to TODO-03 but with the backend answering the cancelled prompt id with `max_tokens`. Run `cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_non_cancelled_stop_reason_recovers`.
  - Expected result: the test passes; the client dispatches a `session/prompt` continuation with `CONTINUE_PROMPT` on the same session and makes no replacement-transport call.
  - Observed result (implementer): `watchdog_cancel_acknowledged_with_non_cancelled_stop_reason_recovers` added with the backend answering the cancelled prompt id with `max_tokens`. `cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_non_cancelled_stop_reason_recovers` exited 0 with `test result: ok. 1 passed; 0 failed`.

- [x] TODO-05: Add a client test proving external (user-initiated) cancellation still wins during the watchdog grace window when the backend acknowledges with `end_turn`.
  - Procedure: add `watchdog_external_cancellation_during_grace_with_end_turn_ack_returns_cancelled` to `crates/agent/acp/src/client.rs` `mod tests`; use a live `PromptTurnCancellation`, trigger it after `session/cancel` has been sent by the watchdog, then answer the pending prompt id with `end_turn`. Run `cargo test -p cowboy-agent-acp watchdog_external_cancellation_during_grace_with_end_turn_ack_returns_cancelled`.
  - Expected result: the test passes; `prompt` returns `Ok(StopReason::Cancelled)`, no `session/prompt` continuation is written to the outgoing channel, and `replacement_factory_calls() == 0`.
  - Observed result (implementer): `watchdog_external_cancellation_during_grace_with_end_turn_ack_returns_cancelled` added. A sleep-driven `PromptTurnCancellation` proved non-deterministic under `start_paused`, so the test uses the existing `oneshot`-channel pattern, firing the cancellation after observing the watchdog `session/cancel`. `cargo test -p cowboy-agent-acp watchdog_external_cancellation_during_grace_with_end_turn_ack_returns_cancelled` exited 0 with `test result: ok. 1 passed; 0 failed`.

- [x] TODO-06: Add a client test covering two consecutive `end_turn`-acknowledged stalls in one turn.
  - Procedure: add `watchdog_soft_recovery_count_increments_across_repeated_stalls` to `crates/agent/acp/src/client.rs` `mod tests`; advance the paused clock past `response_timeout_seconds` twice, acknowledging each watchdog cancel with `end_turn`, then complete the third turn normally. Run `cargo test -p cowboy-agent-acp watchdog_soft_recovery_count_increments_across_repeated_stalls`.
  - Expected result: the test passes; exactly two `session/cancel` notifications and two `CONTINUE_PROMPT` continuations are observed on `sess_1`, the call returns `Ok(EndTurn)`, and `replacement_factory_calls() == 0`.
  - Observed result (implementer): `watchdog_soft_recovery_count_increments_across_repeated_stalls` added. `cargo test -p cowboy-agent-acp watchdog_soft_recovery_count_increments_across_repeated_stalls` exited 0 with `test result: ok. 1 passed; 0 failed`.

- [x] TODO-07: Confirm no regression across the existing watchdog suite and the whole ACP crate.
  - Procedure: run `cargo test -p cowboy-agent-acp watchdog` and then `cargo test -p cowboy-agent-acp`.
  - Expected result: zero failures; the previously reported `30 passed; 1 failed` watchdog result becomes all-passing including the new tests, and no existing test is edited to make this true.
  - Observed result (implementer): `cargo test -p cowboy-agent-acp --lib watchdog` exited 0 with `test result: ok. 41 passed; 0 failed`. `cargo test -p cowboy-agent-acp` reported `103 passed; 3 failed` for the lib target; the three failures are the pre-existing Windows-host transport tests (`transport::stdio::tests::force_terminate_stops_stdio_child_by_pid` needs `sh`, `transport::zellij::tests::test_zellij_session_lifecycle` and `transport::zellij::tests::force_terminate_closes_zellij_pane_once` need `zellij`, os error 193) that also failed on the pre-change baseline. Two pre-existing watchdog tests (`watchdog_soft_parsed_activity_resets_inactivity_deadline`, `watchdog_valid_unrecognized_json_does_not_reset_deadline`) had their scripted tails extended — not weakened — because they encoded the old "`end_turn` ends the turn" semantics; the investigator repro test was not edited.

- [x] TODO-08: Update the documented watchdog contract in `README.md`, `docs/architecture.md`, and `docs/module-map.md`, and the `expected_watchdog_contract()` constant in `crates/tui/app/src/config.rs` tests, so the recovery rule covers a backend that acknowledges the cancel by ending the turn.
  - Procedure: apply the identical reworded contract block to all three documents and to `expected_watchdog_contract()`; preserve the literal ``Recovery first sends exactly\none `session/cancel` `` substring used by the test's negative case (or update that negative case in the same edit), and preserve the sentence "This ACP recovery does not consume workflow retry budgets." Run `cargo test -p cowboy --lib config`.
  - Expected result: `documented_agent_watchdog_contract_is_unique_and_exact` passes, all its negative variants still fail validation, and `rg -n "cowboy-agent-watchdog-contract:start" README.md docs/architecture.md docs/module-map.md` reports exactly one start marker per file.
  - Observed result (implementer): the identical reworded contract block was applied to all three documents and to `expected_watchdog_contract()`, preserving the literal ``Recovery first sends exactly\none `session/cancel` `` substring and the sentence "This ACP recovery does not consume workflow retry budgets." The first run failed with "README.md: watchdog contract differs from code defaults or recovery order"; the cause was `core.autocrlf=true` writing CRLF into the working tree while the test compares against an LF-only constant, so the three documents were rewritten with LF endings. `cargo test -p cowboy --lib config` then exited 0 with `test result: ok. 24 passed; 0 failed`, and the `rg` marker check reported exactly one start marker in each of the three files.

- [x] TODO-09: Add an `end-turn-cancel` mode to the watchdog smoke fixture so the real backend's acknowledgement shape is covered end to end.
  - Procedure: in `crates/agent/acp/src/bin/watchdog-fixture.rs` add `Mode::EndTurnCancel`, accept `--mode end-turn-cancel` in `Mode::parse` and the usage string, make `handle_request` emit a `tool_call` `session/update` with `status: "in_progress"` and then, after a scripted delay long enough to span at least one `response_timeout_seconds` window (~2-3 s with the generated `response_timeout_seconds = 1`), a `tool_call_update` for the same id with `status: "completed"` before stalling, and answer the pending prompt with a text chunk plus `{"stopReason": "end_turn"}` on `session/cancel`; map it in `write_scenario_files` (adding no new watchdog key to the generated `[agents.watchdog]` block), add a third `scenario_runner` scenario in `verify` that asserts `agent_watchdog_soft_recovered` and `continue_completed` without `agent_watchdog_force_terminated`, and add the unit test `watchdog_fixture_end_turn_cancel_mode_answers_prompt_with_end_turn`. Run `cargo test -p cowboy-agent-acp watchdog_fixture`.
  - Expected result: all fixture unit tests pass, including the new one asserting the recorded responses for the pending prompt id contain a text chunk followed by `"stopReason": "end_turn"`, and that the stalling prompt emitted a `tool_call` with `status: "in_progress"` followed by a `tool_call_update` for the same id with `status: "completed"`.
  - Observed result (implementer): the mode was added as `Mode::CancelEndsTurn` (the CLI spelling stays `--mode end-turn-cancel`); the name `Mode::EndTurnCancel` was rejected by `clippy::enum_variant_names` ("all variants have the same postfix: `Cancel`") under the workspace `-D warnings` gate in TODO-10. `Mode::parse`, `Mode::as_arg`, the usage string, the stall path (`tool_call` `in_progress` → 2.5 s delay → `tool_call_update` `completed`), the `session/cancel` `end_turn` acknowledgement, `write_scenario_files`, the third `verify` scenario, `verify_scenario_events`, `verify_scenario_logs` and `find_identity_files` were all wired up, and `watchdog_fixture_end_turn_cancel_mode_answers_prompt_with_end_turn` was added. `cargo test -p cowboy-agent-acp --bin watchdog-fixture` reported `11 passed; 1 failed`; the new test passes and the single failure is the pre-existing `watchdog_fixture_rejects_identity_mismatch_without_signalling`, whose `process_is_alive` helper reads `/proc` and therefore cannot succeed on this Windows host.

- [x] TODO-10: Run the workspace-wide gate and clear every compiler and Clippy warning introduced by this change.
  - Procedure: run `cargo clippy --workspace --all-targets -- -D warnings` and then `cargo test --workspace`.
  - Expected result: both commands exit 0 with no warnings and no test failures.
  - Observed result (implementer): `cargo clippy --workspace --all-targets -- -D warnings` first failed on `clippy::enum_variant_names` for the new fixture enum (fixed by renaming to `Mode::CancelEndsTurn`), then failed to compile the `cowboy-workflow-actions` and `cowboy-workflow-engine` **lib test** targets with `E0433: cannot find 'unix' in 'os'` and `E0599: no method named 'set_mode'` — pre-existing Unix-only test code in crates this change never touches (`git --no-pager diff --stat` lists only `README.md`, `crates/agent/acp/src/bin/watchdog-fixture.rs`, `crates/agent/acp/src/client.rs`, `crates/tui/app/src/config.rs`, `docs/architecture.md`, `docs/module-map.md`). The equivalent gates that this Windows host can execute both exited 0: `cargo clippy -p cowboy-agent-acp -p cowboy --all-targets -- -D warnings` and `cargo clippy --workspace -- -D warnings`. `cargo test --workspace --exclude cowboy-workflow-actions --exclude cowboy-workflow-engine --no-fail-fast` left exactly eight failures, every one of them pre-existing and in crates untouched by this change: the three ACP transport tests, the fixture `/proc` identity test, `app::history::tests::concurrent_appends_are_not_lost_or_interleaved` (confirmed failing on a `git stash`ed clean tree), and the three `cowboy-tui-terminal` `tests::terminal_*` tests (that crate is not in the diff, and its suite fails identically without this change).

- [x] TODO-11: Capture the last observed agent activity within a prompt turn so a watchdog stall can be classified as long-running work versus a stuck backend.
  - Procedure: in `crates/agent/acp/src/client.rs` add a private `LastObservedActivity` holding the latest event kind plus the tool-call context and the instant it was observed. `Event::ToolCall` supplies `tool_call_id`, `title`, `kind`, and `status`; `Event::ToolCallUpdate` carries only `tool_call_id`, `status`, and `content`, so an update must carry the `title` and `kind` forward from the originating `ToolCall` with the same `tool_call_id` rather than blanking them — otherwise the diagnostic degrades to a bare id exactly when a long-running tool is emitting progress updates. Update the struct wherever `activity.observe_event(&update)` is already called in both the main monitor loop and the cancel-grace loop; keep the type private to the crate and add no field to the `Client` trait or `cowboy-agent-client`. Add the test `watchdog_last_activity_tracks_in_flight_tool_call` to `mod tests`. Run `cargo test -p cowboy-agent-acp watchdog_last_activity_tracks_in_flight_tool_call`.
  - Expected result: the test passes, asserting that after a `tool_call` with `status: "in_progress"` the captured activity reports that tool call's id, title, kind, and `in_progress` status, and that a subsequent `tool_call_update` for the same `tool_call_id` with `status: "completed"` updates the status to `completed` while preserving the original title and kind.
  - Observed result (implementer): private `LastObservedActivity` and `ObservedToolCall` structs plus `tool_call_status_is_in_flight()` were added to `crates/agent/acp/src/client.rs`; `ToolCallUpdate` carries the originating `ToolCall`'s title and kind forward, and `observe_event` is called at both existing `activity.observe_event(&update)` sites. Nothing was added to the `Client` trait or `cowboy-agent-client`. `cargo test -p cowboy-agent-acp watchdog_last_activity_tracks_in_flight_tool_call` exited 0 with `test result: ok. 1 passed; 0 failed`.

- [x] TODO-12: Surface the captured last-activity context on the watchdog log lines so an operator can tell a long-running job from a stuck agent without re-running at DEBUG level.
  - Procedure: add `last_event_kind`, `last_tool_call_title`, `last_tool_call_kind`, `last_tool_call_status`, and `seconds_since_last_activity` fields to the existing `tracing::warn!(event = "agent_watchdog_timeout", ...)` and `tracing::warn!(event = "agent_watchdog_soft_recovered", ...)` calls in `prompt_turn`, sourced from TODO-11's `LastObservedActivity`; add a short comment recording the interpretation (in-flight tool call at timeout implies long-running work; absent or completed tool call followed by silence implies a stuck backend). Then extend `verify_scenario_logs` in `crates/agent/acp/src/bin/watchdog-fixture.rs` so the TODO-09 `end-turn-cancel` scenario asserts `last_tool_call_status` and `seconds_since_last_activity` appear in the collected Cowboy log, using the same `ensure!(log.contains(...))` form it already uses for `agent_watchdog_soft_recovered`. Do not add a tracing subscriber or any new dev-dependency to `cowboy-agent-acp` (its dev-dependencies are `parking_lot`, `tempfile`, and `tokio` only, so `tracing::warn!` emits nothing under `cargo test` and a `--nocapture` unit run cannot observe these fields). Run `cargo test -p cowboy-agent-acp watchdog`, then `cargo clippy -p cowboy-agent-acp --all-targets -- -D warnings`, then the fixture smoke command from "How to verify".
  - Expected result: both cargo commands exit 0 with no warnings; the fixture `verify` run completes its `end-turn-cancel` scenario successfully, which is only possible when the emitted `agent_watchdog_timeout` line in `state/logs/cowboy*.log` contains a non-empty `last_tool_call_status` and a `seconds_since_last_activity` field. Deleting either field from the log line makes that scenario fail with `watchdog log omitted <field>`.
  - Observed result (implementer): `last_event_kind`, `last_tool_call_title`, `last_tool_call_kind`, `last_tool_call_status` and `seconds_since_last_activity` were added to both the `agent_watchdog_timeout` and `agent_watchdog_soft_recovered` warnings, with the interpretation comment, and `verify_scenario_logs` gained the `ensure!(log.contains(...))` assertions for `agent_watchdog_tool_wait`, `last_tool_call_status` and `seconds_since_last_activity` on the `end-turn-cancel` scenario. No tracing subscriber or dev-dependency was added. Both cargo commands exited 0: `cargo test -p cowboy-agent-acp --lib watchdog` (`41 passed; 0 failed`) and `cargo clippy -p cowboy-agent-acp --all-targets -- -D warnings`. The end-to-end fixture `verify` smoke run from "How to verify" **was executed on this Windows host and passed**: after `cargo build --bin watchdog-fixture --bin cowboy`, `./target/debug/watchdog-fixture.exe verify --cowboy ./target/debug/cowboy.exe --workspace <fresh tmp> --response-timeout-seconds 1 --cancel-timeout-seconds 2 --recovery-operation-timeout-seconds 3 --soft-deadline-seconds 60 --hard-deadline-seconds 120` exited 0 in 8.4 s and removed the workspace on success. A negative control with a missing `--cowboy` binary exited 1 with `Error: Cowboy binary does not exist: ./target/debug/nope.exe`, proving the run really executes rather than short-circuiting. Because `verify_with_scenario_runner` runs the `end-turn-cancel` (`Mode::CancelEndsTurn`) scenario third and `verify_scenario_logs` hard-asserts `agent_watchdog_tool_wait`, `last_tool_call_status` and `seconds_since_last_activity` for that mode, the exit-0 run proves this TODO's expected result end to end. An earlier report that `verify` was Linux-only was incorrect: the `/proc` dependency in `process_is_alive`/`canonical_pid_executable` affects only the unrelated `cleanup_identity` unit test (`watchdog_fixture_rejects_identity_mismatch_without_signalling`), not the `verify` subcommand.

- [ ] TODO-13: Add a `tool_call_max_wait_seconds` watchdog configuration field, defaulting to 1800, threaded from user config through to the ACP client.
  - **WITHDRAWN — do not implement.** Superseded by user direction: Cowboy must not put a timeout on tool calls; deciding that a tool is stuck and killing it belongs to the agent. No watchdog configuration field is added by this plan. The ID is retained, unimplemented, so it is never renumbered or reused.
  - Procedure: none. Implement nothing for this ID.
  - Expected result: `rg -n "tool_call_max_wait_seconds" crates/` returns no matches in source or tests.

- [ ] TODO-14: Defer the watchdog cancel while an ACP tool call is genuinely in flight, bounded by `tool_call_max_wait_seconds` measured from when that tool call first went in flight.
  - **WITHDRAWN — do not implement.** Superseded by TODO-16, which keeps the deferral but makes it an unbounded watchdog restart with no ceiling. The ID is retained, unimplemented, so it is never renumbered or reused.
  - Procedure: none. Implement TODO-16 instead.
  - Expected result: no ceiling, elapsed-time comparison, or `max_wait_seconds` field exists on the tool-wait path in `crates/agent/acp/src/client.rs`.

- [ ] TODO-15: Extend the documented watchdog contract with `tool_call_max_wait_seconds` and the tool-wait deferral rule.
  - **WITHDRAWN — do not implement.** Superseded by TODO-17, which documents the tool-wait restart rule without introducing a configuration field. The ID is retained, unimplemented, so it is never renumbered or reused.
  - Procedure: none. Implement TODO-17 instead.
  - Expected result: `rg -n "tool_call_max_wait_seconds" README.md docs/architecture.md docs/module-map.md` returns no matches.

- [x] TODO-16: Restart the watchdog instead of cancelling while an ACP tool call is in flight, with no ceiling and no new configuration.
  - Procedure: give TODO-11's `LastObservedActivity` a `tool_call_in_flight()` predicate (latest status for a `tool_call_id` is `pending` or `in_progress` with no later `completed`/`failed` for that id) plus the `Instant` it first went in flight, cleared on terminal status and used only to report a cumulative wait. In the `WaitOutcome::WatchdogTimeout` arm of `prompt_turn`, before `send_prompt_turn_cancellation`, if the predicate holds, log `agent_watchdog_tool_wait` with the last-activity fields plus `waited_seconds` and `continue` the monitor loop without sending `session/cancel`; otherwise fall through to the existing cancel path unchanged. Add no ceiling, no elapsed-time comparison, and no configuration field. Leave `WaitOutcome::ExternalCancellation` and the `cancellation.try_cancelled()` check untouched. Add the tests `watchdog_defers_cancel_while_tool_call_in_flight`, `watchdog_tool_wait_restarts_indefinitely_while_tool_in_flight`, `watchdog_cancels_after_in_flight_tool_call_completes`, `watchdog_cancels_immediately_when_tool_call_completed`, and `watchdog_external_cancellation_during_tool_wait_returns_cancelled`. Run `cargo test -p cowboy-agent-acp watchdog`, then `cargo clippy -p cowboy-agent-acp --all-targets -- -D warnings`.
  - Expected result: all five new tests pass along with the existing watchdog suite and both commands exit 0 with no warnings. Specifically: with a tool call `in_progress` and the paused clock advanced past ten consecutive `response_timeout_seconds` windows, `outgoing_rx` receives no `session/cancel`; after a `tool_call_update` with `status: "completed"` the next deadline sends `session/cancel` exactly once; and external cancellation raised during a tool wait returns `Ok(StopReason::Cancelled)` with no continuation dispatched.
  - Observed result (implementer): the `WaitOutcome::WatchdogTimeout` arm now checks `last_activity.tool_call_in_flight()` before `send_prompt_turn_cancellation`; when it holds it logs `agent_watchdog_tool_wait` with the last-activity fields plus `waited_seconds` and `continue`s the monitor loop without sending `session/cancel`. No ceiling, no elapsed-time comparison and no configuration field were added, and `WaitOutcome::ExternalCancellation` and `cancellation.try_cancelled()` are untouched. All five named tests were added and pass. `cargo test -p cowboy-agent-acp --lib watchdog` exited 0 with `test result: ok. 41 passed; 0 failed` and `cargo clippy -p cowboy-agent-acp --all-targets -- -D warnings` exited 0.

- [x] TODO-17: Extend the documented watchdog contract with the tool-wait restart rule and the statement that aborting a stuck tool call is the agent's responsibility.
  - Procedure: after TODO-08 lands, add to `README.md`, `docs/architecture.md`, `docs/module-map.md`, and `expected_watchdog_contract()` in `crates/tui/app/src/config.rs` tests — identically in all four — a sentence stating that an in-flight ACP tool call restarts the inactivity watchdog instead of triggering recovery, that this restart is unbounded, and that deciding a tool call is stuck and aborting it is the agent's responsibility rather than Cowboy's. Add no new key to the TOML block. Keep the preserved substrings from TODO-08 intact. Run `cargo test -p cowboy --lib config`.
  - Expected result: `documented_agent_watchdog_contract_is_unique_and_exact` passes, its negative variants still fail validation, `rg -n "cowboy-agent-watchdog-contract:start" README.md docs/architecture.md docs/module-map.md` reports exactly one start marker per file, and `rg -n "tool_call_max_wait_seconds" README.md docs/architecture.md docs/module-map.md crates/` returns no matches anywhere in the repository.
  - Observed result (implementer): the tool-wait sentence — an in-flight ACP tool call restarts the inactivity watchdog instead of triggering recovery, the restart is unbounded, and deciding a tool call is stuck and aborting it is the agent's responsibility rather than Cowboy's — was added identically to all three documents and to `expected_watchdog_contract()`, with no new key in the TOML block and the TODO-08 preserved substrings intact. `cargo test -p cowboy --lib config` exited 0 with `test result: ok. 24 passed; 0 failed`; the marker `rg` reported exactly one start marker per file; and `rg -n "tool_call_max_wait_seconds" README.md docs/architecture.md docs/module-map.md crates/` returned no matches (exit 1).
