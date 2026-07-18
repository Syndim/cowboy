# RCA: `cowboy resume` does not retry the current step when the run is Failed

## Bug behavior

`cowboy resume <run-id>` (CLI) and `/resume <run-id>` (TUI) are expected to
retry the current step of a run whenever a current step exists and retrying is
meaningful, regardless of run status. In practice, when the durable run is
`Failed` — including a run that failed because a recoverable step exhausted its
retry budget (for example an agent step whose output repeatedly fails
frontmatter parsing) — resume does nothing:

- It returns the run unchanged, still `Failed`.
- It emits/persists no workflow events.
- The retained current step is never retried, so the only recovery path is
  `cowboy resolve`, which forces a manual status rather than actually retrying
  the failed work.

The user requirement is that resume should retry the current step even when the
run is `Failed`, while preserving safe no-op behavior for terminal states where
retrying is not meaningful (`Completed`, `Cancelled`) or defining/testing exact
behavior consistent with that requirement.

## Root cause

Both product surfaces route through `WorkflowRuntime::resume_run`, which calls
`resume_with(run_id, RunMode::UntilBlocked)` in
`crates/workflow/engine/src/runtime.rs`. `resume_with` short-circuits on any run
status that is not `Running`:

```rust
// crates/workflow/engine/src/runtime.rs (~line 716)
if !matches!(run.status, RunStatus::Running) {
    tracing::debug!(run_id = %run.id, status = ?run.status,
        "workflow run is not running; returning without execution");
    return Ok(RunReport {
        run,
        events: Vec::new(),
    });
}
```

A run that gives up after exhausting recoverable retries is persisted as
`RunStatus::Failed { reason }` while `current_step` is intentionally retained
(see `WorkflowRunner::execute_one` in `crates/workflow/engine/src/runner.rs`,
which calls `apply_run_status(.., RunStatus::Failed { .. })` on give-up so
`cowboy resolve` can continue the run). Because the guard in `resume_with`
treats `Failed` the same as `Completed`/`Cancelled`/`WaitingForInput`, resume
returns early and never re-enters `run_existing` to re-execute the retained
current step. The guard is the single centralized cause; the CLI
(`crates/tui/app/src/main.rs`) and TUI (`crates/tui/app/src/app/commands.rs`)
resume paths both delegate to `resume_run` and add no status filtering of their
own.

## Reproduction steps

1. Configure a workflow whose current step is an agent step (recoverable action)
   with `max_retries_per_step` small (e.g. `2`).
2. Start a run where the agent returns output that fails frontmatter parsing on
   the initial attempt and on every retry (e.g. a body with no YAML
   frontmatter). The run exhausts the per-step recoverable retry budget and is
   persisted as `Failed`, with the failing step retained as `current_step`.
3. Run `cowboy resume <run-id>`.
4. Observe that the run is returned still `Failed`, with no new events, and the
   current step is never retried.

This sequence is encoded deterministically in the regression test below using a
scripted agent backend, so no real ACP agent or network access is required.

## Regression test

- Test file path: `crates/workflow/engine/src/runtime.rs`
- Test name: `runtime::tests::resume_retries_current_step_when_run_failed_by_exhausted_retries`
- Command:
  `cargo test -p cowboy-workflow-engine resume_retries_current_step_when_run_failed_by_exhausted_retries`
- Expected result before the fix: FAIL. The test starts an agent-step run whose
  scripted responses lack frontmatter, exhausting the per-step retry budget so
  the run is persisted as `Failed` with `current_step == "start"`. It then calls
  `resume_run` and asserts the run is retried to completion
  (`RunStatus::Completed`) with a fresh `StepStarted { step_id: "start" }`
  event. Before the fix, `resume_with` short-circuits on the `Failed` status, so
  the run stays `Failed`, no events are emitted, and the scripted recovery
  response is never consumed.

## Current failing result

Running the narrow command before any product-code change:

```
cargo test -p cowboy-workflow-engine resume_retries_current_step_when_run_failed_by_exhausted_retries

running 1 test

thread 'runtime::tests::resume_retries_current_step_when_run_failed_by_exhausted_retries'
panicked at crates/workflow/engine/src/runtime.rs:3833:9:
assertion `left == right` failed: resume should retry the failed current step and complete the run
  left: Failed { reason: "invalid action: config set \"default\" exhausted retry budget for step \"start\": 2/2 retries used; last recoverable error: recoverable action failure: agent response is missing YAML frontmatter" }
 right: Completed
test runtime::tests::resume_retries_current_step_when_run_failed_by_exhausted_retries ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 113 filtered out
```

The `left` value confirms the durable run stayed `Failed` due to an exhausted
recoverable retry budget and that resume performed no execution.

## Fix constraints

- Investigation only: this change set edits tests and documentation. No product
  code was modified. The regression test must fail before the fix and pass
  after.
- Centralize the fix in `WorkflowRuntime::resume_with`
  (`crates/workflow/engine/src/runtime.rs`). Both the CLI (`main.rs`) and TUI
  (`commands.rs`) already delegate to `resume_run`; do not add divergent status
  handling in the UI/CLI crates.
- Allow resume to re-execute the retained current step when the run is `Failed`
  (including failure from exhausted recoverable retries). Flip the run back to a
  runnable state before re-entering `run_existing` so lifecycle events
  (`StepStarted`, `StepCompleted`/`StepRetrying`, `RunCompleted`/`RunFailed`)
  are emitted and persisted through the existing path.
- Preserve safe no-op semantics for terminal/blocked states where retrying is
  not meaningful, or define and test exact behavior consistent with the
  requirement: `Completed` and `Cancelled` should not be silently re-run;
  `WaitingForInput` must continue to be answered via `answer`, not resumed.
- Retry-budget interaction must remain coherent. Durable counters
  (`retries_used`, `step_retries_used`) and per-visit numbering already exist; a
  resumed retry of a step that previously exhausted its budget must have a
  defined, tested outcome (either a fresh visit/initial attempt that can succeed
  without immediately re-exhausting, or an explicit, tested budget policy) so
  resume is actually able to make progress rather than instantly re-failing.
- Keep `execute_step`/`RunStore` budget-and-record semantics in
  `cowboy-workflow-core` unchanged unless a change is strictly required; prefer
  adjusting only the runtime resume entry point.
- If resume semantics change in a user-visible way, update the CLI/TUI help text
  and `README.md`/`docs` command descriptions that document the `step` vs
  `resume` distinction.
- Preserve `user_feedback` exactly in output fields when present; it is
  cumulative raw user direction and must not be augmented with agent- or
  reviewer-generated feedback.
- Validate with the focused command above plus `cargo test --workspace`. Do not
  push or open a PR; finish with a commit ready for review.
