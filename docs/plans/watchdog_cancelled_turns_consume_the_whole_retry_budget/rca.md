# Watchdog-cancelled turns consume the whole retry budget

## Bug behavior

A long-running dev-loop run spent its entire per-step retry budget on the
`implement` step and failed:

```text
invalid action: config set "default" exhausted retry budget for step
"implement": 20/20 retries used; last recoverable error: recoverable action
failure: agent reply did not contain a workflow result
```

The retry mechanism — including the recent no-result retry-prompt escalation —
did not help. All 20 retries failed the same way, and the run made no progress
toward a workflow result.

The essential observation is that these were **not** cases of an agent
completing its work and forgetting the YAML frontmatter. Every one of the 20
attempts was an agent turn that Cowboy's own watchdog cancelled after 100
seconds of stream inactivity, while the agent was legitimately waiting on
long-running external work (remote CI builds, package publishing, device
installs). The truncated reply from each cancelled turn was then treated as an
ordinary completed reply, classified as a recoverable "no workflow result"
failure, and charged against the retry budget.

Because no retry ever gave the agent more time, and because each retry restarted
the same unavoidably slow wait, the loop could not converge. The budget was
consumed at a near-fixed cadence of roughly one attempt every two minutes
(100 s watchdog timeout + 10 s cancel grace + overhead) until it hit 20/20.

## Root cause

Cowboy's ACP watchdog cancels a stalled prompt turn and is designed to recover
it on the same session. In `Client::prompt_turn`
(`crates/agent/acp/src/client.rs`), on inactivity it logs
`agent_watchdog_timeout`, sends `session/cancel`, logs
`agent_watchdog_cancel_sent`, and then enters a grace loop waiting for the
backend to acknowledge.

The recovery only triggers when the backend reports `stopReason: "cancelled"`:

```rust
// crates/agent/acp/src/client.rs, watchdog cancel-grace loop
let stop_reason = result.stop_reason.unwrap_or(StopReason::EndTurn);
if matches!(stop_reason, StopReason::Cancelled) {
    if external_cancellation_sent {
        break 'monitor StopReason::Cancelled;
    }
    tracing::warn!(event = "agent_watchdog_soft_recovered", ...);
    id = self.dispatch_watchdog_continuation(session_id).await?;
    continue 'monitor;
}
break 'monitor stop_reason;
```

The backend in use acknowledges `session/cancel` by **ending the turn normally**:
it emits a final text chunk (`Info: Operation cancelled by user`) and answers the
prompt request with `stopReason: "end_turn"`. That falls through to
`break 'monitor stop_reason`, so the client returns `StopReason::EndTurn` and the
truncated text as if it were a genuine, complete agent reply. The fact that
Cowboy itself had just cancelled this turn — which it knows, having sent the
cancel milliseconds earlier — is discarded.

Downstream, `AgentExecutor::execute_agent`
(`crates/workflow/agent/src/executor.rs`) parses that truncated text, finds no
frontmatter, and maps the failure to a recoverable no-result error:

```rust
let parsed = parse_frontmatter_output(&visible)
    .map_err(|err| match err {
        Error::MissingFrontmatter => Error::NoWorkflowResult,
        other => other,
    })
```

`WorkflowRunner::retry_step` sees a recoverable failure, charges the cumulative
run-wide and per-step retry counters, and re-prompts.

Net root cause: **a watchdog-cancelled turn whose backend acknowledges the
cancel with `end_turn` is indistinguishable from a completed reply, so a
Cowboy-initiated timeout is misattributed to the agent as a formatting/no-result
failure and charged to the retry budget.** Retrying cannot fix it, because the
condition that truncated the turn is a fixed 100 s inactivity timeout, not
anything the agent said or omitted. Every retry re-enters the same long wait and
is cancelled again, so the budget drains deterministically.

## Root cause evidence

Reconstructed from the run's diagnostic logs. Private paths, request content,
and customer/project specifics are generalized; the run id is truncated.

1. The `implementer` role session is created and the step begins working:

   ```text
   2026-07-28T03:32:47.728665Z INFO cowboy_agent_acp::client:
     crates\agent\acp\src\client.rs:574: ACP session created
     session_id=f55a37e6-… model_id=None provider=None
   2026-07-28T03:32:47.729636Z INFO cowboy_workflow_agent::executor:
     crates\workflow\agent\src\executor.rs:875: agent session saved
     run_id=run-8591d3bb-… role=implementer
   ```

2. About seven minutes later the agent has gone quiet (it is waiting on a remote
   build). The watchdog fires and cancels the turn:

   ```text
   2026-07-28T03:40:23.843464Z WARN cowboy_agent_acp::client:
     crates\agent\acp\src\client.rs:994: Agent watchdog detected response
     inactivity event="agent_watchdog_timeout" session_id="f55a37e6-…" id=2
     timeout_seconds=100
   2026-07-28T03:40:23.843883Z WARN cowboy_agent_acp::client:
     crates\agent\acp\src\client.rs:1012: Agent watchdog sent session/cancel
     event="agent_watchdog_cancel_sent" session_id="f55a37e6-…" id=2
   ```

   `timeout_seconds=100` is the default `response_timeout_seconds` in
   `AgentWatchdogOptions::default()`; the user config sets no watchdog overrides.

3. **273 milliseconds later** the executor reports a parse failure. The reply is
   the agent's partial narration, terminated by the backend's cancellation
   notice:

   ```text
   2026-07-28T03:40:24.116711Z ERROR cowboy_workflow_agent::executor:
     crates\workflow\agent\src\executor.rs:748: agent step: failed to parse
     frontmatter output run_id=run-8591d3bb-… step=implement
     reply=I'll start by reading the plan and validation docs …
     Now implementing TODO-01 … Committing and pushing (TODO-05).Pushed.
     Now TODO-06: publishing the … package via <build tool> (this takes a
     while).Info: Operation cancelled by user
   ```

   This is decisive: the reply ends with `Info: Operation cancelled by user`,
   and it appears immediately after Cowboy's own `session/cancel`. The agent did
   not "forget frontmatter" — Cowboy truncated it mid-work. The narration also
   shows the work was progressing normally (edits, tests, commit, push) right up
   to a step the agent itself flags as slow.

4. The pattern repeats with strict one-to-one correlation. Each
   `agent_watchdog_timeout` → `agent_watchdog_cancel_sent` pair is followed
   within ~0.3 s by a parse failure:

   ```text
   04:12:40.623604 WARN … event="agent_watchdog_timeout" id=2 timeout_seconds=100
   04:12:40.624079 WARN … event="agent_watchdog_cancel_sent" id=2
   04:12:40.891543 ERROR … failed to parse frontmatter output step=implement
     reply=Resuming. First checking whether the … publish build fin…

   04:24:01.869662 WARN … event="agent_watchdog_timeout" id=3 timeout_seconds=100
   04:24:01.870071 WARN … event="agent_watchdog_cancel_sent" id=3
   04:24:02.157494 ERROR … failed to parse frontmatter output step=implement
     reply=Build … still running. Polling for the published versi…

   04:26:23.026612 WARN … event="agent_watchdog_timeout" id=4 timeout_seconds=100
   04:26:23.027142 WARN … event="agent_watchdog_cancel_sent" id=4
   04:26:23.297926 ERROR … failed to parse frontmatter output step=implement
     reply=Package `…` is published. Recording TODO-0…
   ```

   Every failing attempt in the run's retry sequence carries the
   `Info: Operation cancelled by user` terminator (19 of 19 attempts inspected
   in the retry log; two of them consist of nothing else, i.e. the turn was
   killed before any text was produced).

5. The attempt cadence matches the watchdog, not the agent. Consecutive failures
   land at 04:28:16, 04:30:14, 04:32:08, 04:38:35, 04:40:36, 04:42:34,
   04:44:26, 04:46:18, 04:48:11, 04:50:07 — roughly 114–120 s apart, i.e.
   100 s inactivity timeout plus the 10 s cancel grace plus overhead. The retry
   budget is being spent by a timer.

6. The reply texts show the agent was blocked on unavoidable external latency,
   so no prompt wording could shorten the wait below 100 s:

   ```text
   reply=… <internal project> build still in configuration phase. Waiting.Info: Operation cancelled by user
   reply=… compiling. Giving it one more wait window before reporting.Info: Operation cancelled by user
   reply=… stage. Polling in short windows to avoid interruption.Info: Operation cancelled by user
   reply=… install is in flight. Waiting for it to land.Info: Operation cancelled by user
   ```

   The agent is visibly aware it is being interrupted ("Polling in short windows
   to avoid interruption") and still cannot beat the timer.

7. A later log captured at DEBUG level shows the exact stop reason Cowboy
   received after its own cancel, proving the misclassification:

   ```text
   08:42:16.594195 WARN … event="agent_watchdog_timeout" session_id="f55a37e6-…"
     id=2 timeout_seconds=100
   08:42:16.594724 WARN … event="agent_watchdog_cancel_sent" … id=2
   08:42:16.908897 DEBUG cowboy_agent_acp::client:
     crates\agent\acp\src\client.rs:1220: ACP prompt turn completed
     session_id="f55a37e6-…" id=2 stop_reason=EndTurn activity=Text
     trailing_text=true
   08:42:16.909002 DEBUG cowboy_workflow_agent::executor:
     crates\workflow\agent\src\executor.rs:614: agent step: initial reply
     run_id=run-8591d3bb-… step=implement stop_reason=EndTurn reply_chars=1652
   ```

   `stop_reason=EndTurn` immediately after `agent_watchdog_cancel_sent` is the
   defect: the grace-loop branch that checks
   `matches!(stop_reason, StopReason::Cancelled)` does not match, so the
   `agent_watchdog_soft_recovered` continuation path is skipped, and the
   truncated turn is returned to the executor as a normal completed turn. Note
   the absence of any `agent_watchdog_soft_recovered` line anywhere in the run's
   logs, despite ~20 watchdog cancellations.

8. Consequently `parse_frontmatter_output` fails, the error is mapped to
   `Error::NoWorkflowResult`, `Error::recoverable` reports it as recoverable,
   and `WorkflowRunner::retry_step` charges the run-wide and per-step counters
   and re-prompts. After 20 such charges the per-step budget is exhausted and
   the runner gives up with the reported message.

9. The regression test reproduces exactly step 7 at the ACP client seam with a
   scripted transport: the watchdog fires, the client sends `session/cancel`,
   the backend replies with a truncated text chunk plus
   `stopReason: "end_turn"`, and the client returns `Ok(EndTurn)` with the
   truncated reply and dispatches no recovery continuation.

## Reproduction steps

1. Configure an agent-backed workflow step whose agent must wait on external
   work that produces no ACP stream activity for longer than the watchdog's
   `response_timeout_seconds` (default 100 s).
2. Start the run and let the step's agent turn go quiet past that threshold.
3. Observe `agent_watchdog_timeout` followed by `agent_watchdog_cancel_sent` in
   the log.
4. Have the backend acknowledge the cancel the way the real one does: emit a
   final text chunk such as `Info: Operation cancelled by user` and answer the
   prompt request with `stopReason: "end_turn"` (not `"cancelled"`).
5. Observe `ACP prompt turn completed … stop_reason=EndTurn`, with no
   `agent_watchdog_soft_recovered` line — the cancelled turn is surfaced as a
   normal completed turn.
6. Observe `agent step: failed to parse frontmatter output`, a recoverable
   no-result failure, and one retry charged to the budget.
7. Because the retry re-enters the same long wait, steps 2–6 repeat at roughly
   the watchdog period until the step's retry budget is exhausted and the run
   fails with `exhausted retry budget for step … N/N retries used`.

Run the focused automated reproduction below for a deterministic version of
steps 2–5.

## Regression test

- Test file: `crates/agent/acp/src/client.rs`
- Test name:
  `client::tests::watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating`
- Command:
  `cargo test -p cowboy-agent-acp watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating -- --nocapture`
- Expected failure before the fix: after the watchdog sends `session/cancel` and
  the backend acknowledges with a truncated text chunk plus `end_turn`, the
  client dispatches no recovery continuation and instead returns `Ok(EndTurn)`
  carrying the truncated reply. The test panics on the missing continuation.

## Current failing result

```text
running 1 test

thread 'client::tests::watchdog_cancel_acknowledged_with_end_turn_recovers_instead_of_truncating'
panicked at crates\agent\acp\src\client.rs:3444:13:
a watchdog-cancelled turn must be recovered on the same session, but the client
sent no continuation after cancelling and returned Ok(EndTurn) with the
truncated reply

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 95 filtered out
```

The 30 sibling watchdog tests in the same crate pass, so the failure isolates
the `end_turn`-acknowledged cancellation path rather than watchdog behavior in
general:

```text
cargo test -p cowboy-agent-acp watchdog
test result: FAILED. 30 passed; 1 failed; 0 ignored; 0 measured; 65 filtered out
```

## Fix constraints

- Do not modify the investigator-added regression test while implementing the
  fix.
- A turn that Cowboy's own watchdog cancelled must not be surfaced to the
  executor as an ordinary completed turn. The client already knows it sent
  `session/cancel`; that knowledge must survive a backend that acknowledges the
  cancel with `stopReason: "end_turn"` instead of `"cancelled"`.
- Reuse the existing soft-recovery design rather than inventing a parallel one:
  the `end_turn` acknowledgement should reach the same
  `agent_watchdog_soft_recovered` continuation path as a `cancelled`
  acknowledgement, keeping the same session.
- Preserve the existing distinction for **external** (user-initiated)
  cancellation: when `external_cancellation_sent` is set, the turn must still
  terminate as cancelled and must not be auto-continued.
- Do not regress genuine completions. A turn that ends with `end_turn` without a
  preceding watchdog cancel must continue to be returned as `EndTurn` with its
  reply intact, including the trailing-event drain behavior.
- Keep the existing watchdog escalation ladder intact: cancel-grace timeout and
  unusable-stream handling must still fall through to hard recovery
  (`hard_recover_and_continue`) as they do today.
- Do not "fix" this by enlarging or resetting retry budgets, and do not make the
  no-result failure non-recoverable; the defect is the misclassification of a
  Cowboy-initiated timeout as an agent reply, not the retry policy itself.
- Do not change the no-result classification or retry-prompt escalation added by
  earlier work; they remain correct for genuinely completed replies that omit a
  workflow result.
- Consider making watchdog-truncated turns observable to the caller (for example
  a distinct, non-retry-charging failure) so an unavoidably slow step cannot
  silently drain a run's retry budget; any such change must keep the
  `Client` trait provider-neutral.
- All 30 existing `cowboy-agent-acp` watchdog tests must keep passing.
