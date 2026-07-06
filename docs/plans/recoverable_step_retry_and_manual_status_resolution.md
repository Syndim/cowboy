# Plan

Add two related resilience features to the workflow runtime so a run that hits a
transient or malformed-output step failure is no longer a dead end:

1. **Auto-retry recoverable failures.** When a step action fails with an error
   classified as recoverable (for example the agent finished its work but its
   final message was missing the required YAML frontmatter, or a transient
   ACP/transport error), the runner retries the same step a bounded number of
   times before giving up. Agent frontmatter/parse failures get a corrective
   nudge appended to the retry prompt so the same session re-emits a
   frontmatter-tagged answer.
2. **Manual status resolution with guided choices.** When a run is stopped on a
   failed step (either after retries are exhausted or for a non-recoverable
   failure), the user first gets a CLI/TUI command that **lists the possible
   statuses the failed step can be resolved to and the information required for
   each**, so the user can choose one. They then run the resolve command with the
   chosen status (and any required fields/body). That synthesizes a completed
   `StepRecord` and routes the run to the next step via the workflow's normal
   transitions, letting the workflow continue.

   The set of resolvable statuses is derived from the failed step's transition
   table (`StepDefinition.transitions.by_status` keys) plus the implicit
   terminal `success` status handled by `next_step`
   (`crates/workflow/core/src/definition.rs:197-217`). The "required
   information" for each status is derived from the failed step's action
   `OutputSpec` (`crates/workflow/core/src/action.rs`) — its declared
   `statuses` and `fields` — which we recover by re-evaluating the current
   step's action via `LuaStepActionProvider`
   (`crates/workflow/engine/src/runner.rs`), since a failed step does not persist
   a `StepRecord`.

## Root cause reviewed (run `run-<redacted>`)

The reviewed run failed on the `plan` step. Diagnostic log line:

```
ERROR cowboy_workflow_agent::executor: crates/workflow/agent/src/executor.rs:246:
agent step: failed to parse frontmatter output run_id=run-<redacted> step=plan
reply=<prose plan text with no YAML frontmatter>
```

The planner agent completed its work and wrote the plan to disk, but its final
assistant message was plain prose with **no YAML frontmatter**, so
`parse_frontmatter_output` returned `Error::MissingFrontmatter`. That error is
propagated with `?` out of `AgentExecutor::execute_agent`
(`crates/workflow/agent/src/executor.rs:245`), through `AgentActionRunner::run`,
`EngineActionDispatcher::dispatch`, `execute_step`, and finally
`WorkflowRunner::execute_one`, which emits a `RunFailed` event and returns
`Err(err)` (`crates/workflow/engine/src/runner.rs:116-124`).

Two consequences make this a good fit for the requested features:

- The failure is **recoverable**: the agent session is intact and the plan
  content exists; a nudge to re-emit with frontmatter would very likely succeed.
- On this error path the run's persisted status is **not** updated to
  `Failed` — `run_existing_with_events` does `break result?`
  (`crates/workflow/engine/src/runtime.rs:527`) and returns the error before any
  `save_run`, so the stored run is left mid-flight with no clean way for the user
  to nudge it forward. Both features need a well-defined persisted terminal
  state to build on.

# Changes

## Recoverable-error classification (core + agent)

- In `crates/workflow/agent/src/error.rs`, add a `recoverable(&self) -> bool`
  method (or a `RetryClass` enum) on `Error`. Classify `MissingFrontmatter`,
  `FrontmatterNotMapping`, `FrontmatterFieldNotString`, `MissingStatus`,
  `MissingOutput`, and `Client(anyhow)` (transport/ACP) as recoverable; classify
  `MissingClient`, `Workflow`, and `Json` conversion of internal data as
  non-recoverable.
- In `crates/workflow/core/src/error.rs`, add a `recoverable(&self) -> bool`
  method on `WorkflowError`. Treat `InvalidAction` as recoverable **only** when it
  wraps a recoverable agent error; keep graph/definition errors
  (`UnknownStep`, `UnknownRole`, `UnknownTransitionTarget`,
  `UnknownRuntimeTransition`, id/empty validation) non-recoverable. To avoid
  brittle string matching, thread the recoverability decision through the
  `From<agent::Error> for WorkflowError` conversion by introducing a dedicated
  recoverable variant (for example `WorkflowError::RecoverableAction(String)`)
  or an explicit flag, rather than re-parsing `to_string()`.

## Retry loop (runner)

- Extend `RunnerLimits` in `crates/workflow/engine/src/runner.rs` (backed by core
  `RunnerLimits` in `crates/workflow/core/src/runner.rs` if that is where it
  lives) with `max_retries_per_step: u32` (default e.g. `2`) and optionally a
  base backoff duration for transient client errors.
- In `WorkflowRunner::execute_one`, wrap the `execute_step` call in a bounded
  retry loop: on `Err(err)` where `err.recoverable()` is true and the step's
  retry budget is not exhausted, emit a new `StepRetrying` progress/event, wait
  an optional backoff, and re-run the same step. On success, proceed as today. On
  a non-recoverable error or exhausted retries, fall through to the current
  `RunFailed` path.
- Important budget interaction: `execute_step` calls `increment_budget` before
  dispatch (`crates/workflow/core/src/engine.rs:48`). A retry must not consume
  the whole `max_visits_per_step`/`max_steps_per_run` budget on one logical step.
  Add a retry-aware entry point (for example an
  `execute_step_once_without_budget` variant, or decrement/compensate on
  recoverable failure) so retries are counted against `max_retries_per_step`, not
  the visit budget. Keep the budget semantics for legitimate step re-visits via
  transitions unchanged.

## Corrective nudge for agent parse failures (agent executor)

- In `crates/workflow/agent/src/executor.rs`, when the executor is invoked as a
  retry after a frontmatter/parse failure, append a short corrective instruction
  to the prompt (reusing the required-frontmatter text already produced in
  `crates/workflow/agent/src/prompt.rs`) telling the agent to re-emit its result
  with the mandatory YAML frontmatter block and a valid `status`. Thread a
  lightweight `attempt`/`retry_reason` field through `ExecutionContext`
  (`crates/workflow/core/src/traits.rs`) or the `AgentAction` dispatch path so the
  executor knows it is a corrective retry. Session reuse already keeps the
  agent's prior work in context, so the nudge should be enough to recover.

## Persist a clean failed state on give-up (runtime)

- Change the give-up path so the run is durably marked
  `RunStatus::Failed { reason }` before returning. In
  `crates/workflow/engine/src/runtime.rs::run_existing_with_events`, instead of
  `break result?` on error, capture the error, apply
  `apply_run_status(&store, &mut run, RunStatus::Failed { reason })` (reusing the
  existing helper in `crates/workflow/core/src/engine.rs`), emit the
  `RunFailed`/status events, persist events, and return a `RunReport` with the
  failed run (or return the error after persisting — pick one and keep CLI
  behavior consistent). This gives the manual-resolution command a well-defined
  `Failed` run to act on and records which `current_step` failed.
- Ensure `current_step` still points at the failed step when the run is saved as
  `Failed`, so resolution knows which step to synthesize a record for.

## Manual status resolution command (core + actions + engine + CLI + TUI)

- Add a runtime **inspection** operation
  `WorkflowRuntime::resolution_options(run_id) -> ResolutionOptions` in
  `crates/workflow/engine/src/runtime.rs` that, for a `Failed` (or
  failed-`Running`) run, returns the guided choices the user needs:
  - Load the run and compile its snapshot definition (same as `resume_with`).
  - Compute the list of resolvable statuses from the failed
    `current_step`'s `transitions.by_status` keys plus the implicit `success`
    status, pairing each with its target step (or "run completes" for
    `success`).
  - Recover the "required information" per status by re-evaluating the failed
    step's action through `LuaStepActionProvider::step_action` (a failed step
    persists no `StepRecord`, so the action must be recomputed). When the action
    is an `Agent` action with an `OutputSpec`, expose its declared `statuses`
    and `fields` schema as the required/optional fields for resolution; when it
    is a `Status`/`AskUser`/`Fail` action, expose that shape instead.
  - Return a serializable `ResolutionOptions { failed_step, failure_reason,
    statuses: Vec<ResolutionStatus { status, target_step, required_fields,
    optional_fields, body_expected }> }` value (new type in the engine crate or
    `cowboy-workflow-core`).
- Add a runtime operation `WorkflowRuntime::resolve_run(run_id, status, fields?,
  body?)` in `crates/workflow/engine/src/runtime.rs`, modeled on `answer_run`:
  - Load the run; require it to be `Failed` (or `Running` left on a failed step).
  - Validate that `status` is an allowed transition out of the failed
    `current_step` using the same routing used by `next_step`
    (`crates/workflow/core/src/definition.rs`), returning a clear
    `WorkflowError::InvalidAction` that **lists the valid statuses and their
    required fields** (reusing `resolution_options`) when it is not.
  - Validate that any fields the chosen status's `OutputSpec` marks as required
    are present in the supplied `--fields`, returning an actionable error naming
    the missing fields when not.
  - Synthesize a completed `StepRecord` for the current step (reuse
    `StatusActionRunner` in `crates/workflow/actions/src/status.rs` or a small
    dedicated builder) with `action = "status"` (or a distinct
    `"manual_resolution"` marker), the provided `status`, `fields`, and `body`,
    correct `prev = run.head`, and timestamps.
  - Apply it through `apply_step_record`, flip the run back to `Running`, emit
    `StepCompleted` + status events, persist events, and continue the run via
    `run_existing_with_events` when the resulting status is `Running` (matching
    the `answer_run` tail).
- Add CLI subcommands in `crates/tui/src/main.rs`:
  - `cowboy resolve <run-id>` (no status) prints the guided
    `resolution_options`: the failed step, failure reason, and a table of
    possible statuses with target step and required/optional fields, so the user
    can choose.
  - `cowboy resolve <run-id> <status> [--fields <json>] [--body <text>]`
    performs the resolution via `runtime.resolve_run(...)` and prints the
    resulting report.
  - Update the clap `Command` enum and dispatch match accordingly.
- Add TUI slash commands in `crates/tui/src/app`: `/resolve <run-id>` lists the
  options, and `/resolve <run-id> <status>` performs the resolution via the same
  runtime methods. When a run enters the failed state, surface the failed step,
  the possible statuses with their required information, and the exact `cowboy
  resolve ...` command in the failed-run rendering so the user is told exactly
  what to type.
- When a run fails on a step, make the CLI/TUI failure output include the
  guided resolution summary (valid statuses + required fields) and the exact
  `cowboy resolve <run-id>` command, satisfying "provide the possible status and
  the required information to the user so [the] user can choose one to resolve."

## Events / progress surfacing

- In `crates/workflow/engine/src/events.rs`, add `WorkflowEventKind::StepRetrying
  { step_id, attempt, max_attempts, reason }` and a
  `ManuallyResolved { step_id, status }` event (or reuse `StepCompleted` with a
  marker) so both the auto-retry and manual-resolution flows are visible in the
  live event stream and persisted event log.
- Update any exhaustive matches over `WorkflowEventKind` (TUI rendering in
  `crates/tui/src/app`, `engine-cli`, `store-cli`) to handle the new variants.

## Docs

- Update `docs/architecture.md` and `docs/module-map.md` to describe the retry
  policy and the resolve/manual-status flow.
- Update `README.md` and the AGENTS.md CLI/TUI command lists to document
  `cowboy resolve` and `/resolve`.

# Tests to be added/updated

- **Error classification (core + agent):** unit tests asserting
  `MissingFrontmatter`/`MissingStatus`/`FrontmatterNotMapping`/transient
  `Client` are recoverable, and definition/graph errors + `MissingClient` are
  not; and that the agent→core error conversion preserves recoverability.
- **Runner retry (engine `runner.rs` tests):** with a fake dispatcher that fails
  recoverably N times then succeeds, assert the step is retried up to
  `max_retries_per_step`, succeeds without exhausting `max_visits_per_step`, and
  emits `StepRetrying` events with increasing `attempt`.
- **Runner give-up:** a fake dispatcher that always fails recoverably exhausts
  retries and produces a persisted `RunStatus::Failed` with a reason; a
  non-recoverable failure fails immediately without retrying.
- **Corrective nudge (agent executor tests):** assert a retry attempt appends the
  required-frontmatter corrective instruction to the prompt (using a fake
  `Client` that records prompts), and that a second attempt returning valid
  frontmatter yields a completed record.
- **Resolution options discovery (engine `runtime.rs` tests):** for a `Failed`
  run whose failed step declares transitions and an agent `OutputSpec`,
  `resolution_options` returns each valid status (transition keys + `success`)
  with its target step and the required/optional fields derived from the
  recomputed action; a step with no explicit transitions still surfaces the
  implicit `success` option.
- **Manual resolution (engine `runtime.rs` tests):** starting from a `Failed`
  run on a known step, `resolve_run` with a valid status synthesizes a
  `StepRecord`, routes to the correct next step, flips to `Running`, and
  continues; an invalid/unroutable status returns `WorkflowError::InvalidAction`
  whose message lists valid statuses and their required fields; omitting a
  required field returns an actionable error naming the missing field; provided
  `fields`/`body` appear on the synthesized record and are visible to the next
  step as `ctx.prev`.
- **Event persistence:** assert `StepRetrying` and the manual-resolution
  completion events are persisted in the run event log in the correct order
  (mirroring existing `answer_run` persistence-order tests).
- **CLI wiring:** a smoke test (or manual verification note) that
  `cowboy resolve <run-id>` prints options and `cowboy resolve <run-id> <status>`
  parses and dispatches; update any clap argument tests.
- Regression: existing `execute_step`, `answer_run`, budget-limit, and
  action-serialization tests must continue to pass unchanged.

# How to verify

- `cargo build` and `cargo test` pass for the workspace.
- Targeted: `cargo test -p cowboy-workflow-core`,
  `cargo test -p cowboy-workflow-agent`,
  `cargo test -p cowboy-workflow-actions`,
  `cargo test -p cowboy-workflow-engine`.
- Manual end-to-end (using `demo-config.toml` or a local config):
  - Reproduce the recoverable case with a workflow/agent that omits frontmatter
    on the first attempt and confirm the run auto-retries and completes the step,
    with `StepRetrying` visible via `cowboy runs` / event log.
  - Force a run to fail terminally, confirm it is stored as `Failed` (via
    `cowboy runs`), then run `cowboy resolve <run-id>` and confirm it prints the
    failed step, failure reason, and the possible statuses with their required
    fields. Then run `cowboy resolve <run-id> <status> [--fields ...]` and
    confirm the run advances to the next step and continues.
  - Confirm the failure output and `cowboy resolve <run-id>` print the possible
    statuses and required information, and that resolving with a missing required
    field or an invalid status produces a clear, actionable error.

# TODO

- [x] Add recoverability classification to `crates/workflow/agent/src/error.rs`
      (`recoverable()` or `RetryClass`).
- [x] Add a recoverable-aware variant/flag to `WorkflowError` in
      `crates/workflow/core/src/error.rs` and update
      `From<agent::Error> for WorkflowError` to preserve recoverability (no
      string re-parsing).
- [x] Add `max_retries_per_step` (and optional backoff) to `RunnerLimits` and
      config plumbing in `crates/tui/src/config.rs` +
      `cowboy-workflow-engine` `RuntimeConfig`.
- [x] Implement the bounded retry loop in `WorkflowRunner::execute_one`
      (`crates/workflow/engine/src/runner.rs`) with a budget-safe re-execution
      entry point so retries don't consume `max_visits_per_step`.
- [x] Thread an `attempt`/`retry_reason` signal through `ExecutionContext` /
      dispatch and append a corrective frontmatter nudge on retry in
      `crates/workflow/agent/src/executor.rs` (reusing `prompt.rs` text).
- [x] Persist `RunStatus::Failed { reason }` on give-up in
      `WorkflowRuntime::run_existing_with_events`
      (`crates/workflow/engine/src/runtime.rs`), keeping `current_step` on the
      failed step.
- [x] Add `WorkflowRuntime::resolution_options(run_id)` returning a
      serializable `ResolutionOptions` (valid statuses + target steps +
      required/optional fields) by recomputing the failed step's action via
      `LuaStepActionProvider` and reading its transitions/`OutputSpec`.
- [x] Add `WorkflowRuntime::resolve_run(run_id, status, fields?, body?)`
      modeled on `answer_run`, with transition validation via `next_step`,
      required-field validation against the chosen status's `OutputSpec`, and a
      synthesized `StepRecord` (reuse `StatusActionRunner`); errors must list
      valid statuses and required fields.
- [x] Add `cowboy resolve <run-id>` (lists options) and
      `cowboy resolve <run-id> <status> [--fields <json>] [--body <text>]`
      (performs resolution) CLI subcommands in `crates/tui/src/main.rs`
      (clap enum + dispatch).
- [x] Add `/resolve <run-id>` (lists options) and `/resolve <run-id> <status>`
      (resolves) TUI slash commands, and render the failed-run hint with the
      possible statuses, required information, and exact resolve command in
      `crates/tui/src/app`.
- [x] Add `WorkflowEventKind::StepRetrying` and manual-resolution event variant
      in `crates/workflow/engine/src/events.rs`; update all exhaustive matches
      (TUI, `engine-cli`, `store-cli`).
- [x] Add/adjust unit tests: error classification, retry loop, give-up,
      corrective nudge, manual resolution, event ordering, CLI parsing.
- [x] Update docs: `docs/architecture.md`, `docs/module-map.md`, `README.md`,
      and AGENTS.md CLI/TUI command lists.
- [x] Run `cargo test` (workspace + targeted per-crate) and perform the manual
      end-to-end verification above.
