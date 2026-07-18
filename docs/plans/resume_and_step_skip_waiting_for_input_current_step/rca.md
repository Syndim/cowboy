# RCA: `resume`/`step` skip the retained `WaitingForInput` current step

## Bug behavior

Per the clarified requirement, `cowboy resume <run-id>` / `/resume <run-id>` and
`cowboy step <run-id>` / `/step <run-id>` must retry/re-execute the retained
current step for **every** non-terminal run status. Only `RunStatus::Completed`
and `RunStatus::Cancelled` are legitimate no-ops. Therefore `Running`, `Failed`,
and `WaitingForInput` must all proceed through execution.

The current branch (`fix/resume-failed-step`, PR #1) handles `Running` and
`Failed`, but still treats `RunStatus::WaitingForInput` as a resume no-op:

- Resuming a run whose current step is an `ask_user` step returns the run
  unchanged, still `WaitingForInput`.
- It emits/persists no workflow events (no `StepStarted` for the waiting step).
- The retained `ask_user` current step is never re-executed, so the run is
  never re-prompted and its durable pending resume callback is never refreshed.

Expected behavior: resuming (or stepping) a `WaitingForInput` run re-executes
the retained `ask_user` step, re-prompts the user, and safely replaces the
durable pending resume callback with the freshly minted one. The run stays
answerable, and `answer` behavior is unchanged.

## Root cause

Both product surfaces route through `WorkflowRuntime::resume_run` /
`WorkflowRuntime::step_run`, which call
`resume_with(run_id, mode)` in `crates/workflow/engine/src/runtime.rs`. The
status match in `resume_with` lumps `WaitingForInput` together with the true
terminal states and returns early:

```rust
// crates/workflow/engine/src/runtime.rs (resume_with)
match run.status {
    RunStatus::Running => {}
    RunStatus::Failed { .. } => {
        // ... flips to Running and re-executes the retained current step ...
        apply_run_status(&store, &mut run, RunStatus::Running)?;
    }
    RunStatus::Completed | RunStatus::Cancelled | RunStatus::WaitingForInput { .. } => {
        // early return: no execution, no events
        return Ok(RunReport { run, events: Vec::new() });
    }
}
```

Because `WaitingForInput` is in the early-return arm, `resume_with` never
re-enters `run_existing` for a waiting run. The downstream runner
(`WorkflowRunner::run_until_blocked` / `step_once`) only executes while
`run.status` is `RunStatus::Running`, so even reaching the runner would require
the status to be flipped back to `Running` first (exactly as the `Failed` arm
already does). The centralized cause is the single `resume_with` match arm; the
CLI (`crates/tui/app/src/main.rs`) and TUI
(`crates/tui/app/src/app/commands.rs`) resume/step paths add no status filtering
of their own.

Re-executing the `ask_user` step is safe and idempotent for the durable state:
`AskUserActionRunner::run` (`crates/workflow/actions/src/ask_user.rs`) builds a
fresh `ResumeCallback` whose payload `record_id` is derived from
`ExecutionContext::step_record_id` (`{run_id}-{steps_executed + 1}` in
`crates/workflow/core/src/engine.rs`). A fresh execution increments
`steps_executed`, mints a new `record_id`, and `apply_run_status` overwrites the
prior `WaitingForInput` status, so the pending callback is replaced rather than
duplicated or left dangling.

## Reproduction steps

1. Author a workflow whose current step is an `ask_user` step (for example a
   step returning `action.ask_user { id = "approval", message = "Approve?",
   choices = { "yes" } }`).
2. Start a run. It reaches `RunStatus::WaitingForInput`, retaining the
   `ask_user` step as the current step with a durable pending resume callback.
3. Run `cowboy resume <run-id>` (or `cowboy step <run-id>`, or the TUI
   `/resume` / `/step`).
4. Observe that the run is returned unchanged: still `WaitingForInput`, no new
   events emitted, the `ask_user` step is not re-executed/re-prompted, and the
   durable pending callback is not refreshed.

This sequence is encoded deterministically in the regression test below, so no
real ACP agent or network access is required.

## Regression test

- Test file path: `crates/workflow/engine/src/runtime.rs`
- Test name:
  `runtime::tests::resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback`
- Command:
  `cargo test -p cowboy-workflow-engine resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback`
- Expected result before the fix: FAIL. The test starts a single-`ask_user`-step
  run that blocks as `WaitingForInput` on step `ask`, records the durable
  callback `record_id`, then calls `resume_run`. It asserts (a) the run is still
  `WaitingForInput` on `ask`/`approval`, (b) a fresh `StepStarted { step_id:
  "ask" }` event is emitted (proving re-execution/re-prompt), (c) the durable
  pending callback `record_id` changed (proving the callback was safely
  replaced), and (d) the run remains answerable to `Completed`. Before the fix,
  `resume_with` short-circuits on `WaitingForInput`, so no events are emitted and
  the durable callback is unchanged, failing assertion (b) first.

## Current failing result

Running the narrow command before any product-code change:

```
cargo test -p cowboy-workflow-engine resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback

running 1 test
test runtime::tests::resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback ... FAILED

failures:

---- runtime::tests::resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback stdout ----

thread 'runtime::tests::resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback'
panicked at crates/workflow/engine/src/runtime.rs:4107:9:
resume must re-execute the waiting ask_user step, emitting StepStarted for it

failures:
    runtime::tests::resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 119 filtered out; finished in 0.06s
```

The panic confirms resume performed no execution on the waiting run: no
`StepStarted` event for the retained `ask_user` step, so the step was never
re-prompted and the durable callback was never replaced.

## Fix constraints

- Investigation only: this change set edits tests and documentation. No product
  code was modified. The regression test must fail before the fix and pass
  after.
- Centralize the fix in `WorkflowRuntime::resume_with`
  (`crates/workflow/engine/src/runtime.rs`). Both the CLI (`main.rs`) and TUI
  (`commands.rs`) already delegate to `resume_run`/`step_run`; do not add
  divergent status handling in the UI/CLI crates.
- Make `resume`/`step` re-execute the retained current step for **every**
  non-terminal status. Move `WaitingForInput` out of the early-return arm and
  flip it back to `RunStatus::Running` before re-entering `run_existing` (the
  same mechanism the `Failed` arm already uses), so the runner re-executes the
  current step and emits/persists the normal lifecycle events (`StepStarted`,
  then the resulting `WaitingForInput`/`Completed`/`RunFailed`).
- Only `RunStatus::Completed` and `RunStatus::Cancelled` remain no-ops. State
  this exactly in code comments, tests, `README.md`, and `docs/architecture.md`
  (remove any wording that lists `WaitingForInput` as non-resumable).
- Re-executing an `ask_user` step must safely replace the durable pending resume
  callback (a fresh `record_id`/callback), not duplicate or orphan it.
- Preserve `answer_run` behavior exactly: answering a `WaitingForInput` run must
  still route through the (possibly replaced) pending callback and advance the
  run. Do not change `answer_run`.
- Update the existing test
  `runtime::tests::resume_is_noop_for_waiting_run_and_prompt_stays_answerable`,
  which encodes the pre-clarification no-op expectation and will contradict the
  new behavior after the fix; keep the two other no-op tests
  (`resume_is_noop_for_completed_run`, `resume_is_noop_for_cancelled_run`).
- Align the sibling plan/RCA in
  `docs/plans/resume_does_not_retry_failed_current_step/` so their language
  states only `Completed`/`Cancelled` are non-resumable.
- Keep `execute_step`/`RunStore` budget-and-record semantics in
  `cowboy-workflow-core` unchanged; re-executing the current step already
  consumes one step/visit through the normal initial-attempt path. Ensure
  `max_visits_per_step` is not exhausted by repeated re-prompts in practice.
- Preserve `user_feedback` exactly in output fields when present; it is
  cumulative raw user direction and must not be augmented with agent- or
  reviewer-generated feedback.
- Validate with the focused resume tests plus `cargo fmt --check`,
  `cargo test --workspace`, and
  `cargo clippy --workspace --all-targets -- -D warnings`. Amend the fix on the
  current `fix/resume-failed-step` branch; do not create a new branch or PR and
  do not push.
