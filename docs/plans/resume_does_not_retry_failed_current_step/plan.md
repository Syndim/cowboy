# Plan: `cowboy resume` retries the current step when the run is Failed

Root cause analysis: [`rca.md`](./rca.md).

Reviewed regression test (input to this fix, do not rewrite/replace):
`crates/workflow/engine/src/runtime.rs::resume_retries_current_step_when_run_failed_by_exhausted_retries`
(command: `cargo test -p cowboy-workflow-engine resume_retries_current_step_when_run_failed_by_exhausted_retries`).

## Plan

Per the RCA, both `cowboy resume <run-id>` and TUI `/resume <run-id>` route
through `WorkflowRuntime::resume_run` → `resume_with(run_id, RunMode::UntilBlocked)`
in `crates/workflow/engine/src/runtime.rs`. `resume_with` short-circuits on any
status that is not `RunStatus::Running`, so a run that gave up as
`RunStatus::Failed { reason }` after exhausting its recoverable-retry budget (for
example an agent step whose output repeatedly fails YAML-frontmatter parsing,
including duplicate status keys) is returned unchanged, emits no events, and
never re-executes its retained `current_step`.

Fix approach — smallest correct change, centralized in the runtime resume entry
point exactly as the RCA constrains:

1. In `resume_with`, stop treating `Failed` like the other terminal/blocked
   states. Replace the single `if !matches!(run.status, RunStatus::Running)`
   guard with an explicit match:
   - `RunStatus::Running` → proceed as today.
   - `RunStatus::Failed { .. }` → flip `run.status` back to `RunStatus::Running`
     in memory (make `run` mutable) so the existing `run_existing` /
     `WorkflowRunner::run_until_blocked` path re-executes the retained
     `current_step` and emits/persists the normal lifecycle events
     (`StepStarted`, then `StepCompleted`/`StepRetrying`, and finally
     `RunCompleted`/`RunFailed`). The runner persists the resulting terminal
     status through the existing record/status-application path, so no separate
     durable write of the transient `Running` state is required; if execution is
     interrupted the durable run safely remains `Failed`.
   - `RunStatus::Completed`, `RunStatus::Cancelled` → keep the current safe
     no-op (return the run unchanged with no events). These are the only
     non-resumable statuses. (Per the later clarification, `WaitingForInput` is
     no longer a no-op: `resume`/`step` re-execute — re-prompt — its retained
     `ask_user` step and safely replace the durable pending resume callback,
     while `answer_run` remains available to supply a prompt answer.)

2. Retry-budget coherence (defined, tested outcome required by the RCA): do
   **not** reset the durable cumulative counters (`retries_used`,
   `step_retries_used`) on resume. The runner always runs one **initial
   attempt** for the current step before consulting the retry budget, and
   initial attempts never consume budget (`retry_step` only draws from remaining
   `max_retries_per_run` / `max_retries_per_step`). Therefore resume grants
   exactly one fresh initial attempt that can succeed without immediately
   re-exhausting — matching the RCA's first acceptable option and the reviewed
   repro test, whose valid 4th scripted response is consumed by that fresh
   initial attempt. If that initial attempt also fails and the step budget is
   already exhausted, the run deterministically returns to `Failed`, preserving
   the existing cumulative-across-visits budget model. This keeps
   `cowboy-workflow-core` `execute_step`/`RunStore` budget-and-record semantics
   unchanged.

3. Because `resume_with` is shared by both `RunMode::UntilBlocked` (`resume`)
   and `RunMode::SingleStep` (`step`), this change also lets `cowboy step` take
   one fresh retry attempt at a Failed current step. That is consistent with the
   requirement (a current step exists and retrying is meaningful) and
   complementary to `cowboy resolve` (which still forces a manual status). This
   behavior will be locked with a test.

4. Docs/help: the CLI/TUI resume help strings already say "continue a run until
   it blocks, fails, or completes", which remains accurate. Update the prose
   that documents the `step` vs `resume` distinction and the failed-run recovery
   story so it reflects that `resume`/`step` now retry a Failed current step
   (not only `resolve`). No command grammar, argument, or output-shape changes,
   so `cowboy-command-parser` is untouched.

## Changes

- `crates/workflow/engine/src/runtime.rs`
  - `resume_with`: make `run` mutable and replace the non-`Running` early-return
    guard with an explicit status match. `Failed { .. }` flips to `Running` and
    falls through to `run_existing`; `Completed` / `Cancelled` keep the no-op
    early return. (Per the later clarification, `WaitingForInput { .. }` also
    flips to `Running` and re-executes.) Keep the existing `tracing` debug
    logging (log the resume-of-failed transition).
- `README.md`
  - Update the `step` vs `resume` explanation and the "Resolve a failed run"
    section wording so it states that `resume`/`step` retry the Failed current
    step (one fresh attempt for `step`, continue-until-blocked for `resume`),
    while `resolve` remains the way to force a manual status. Keep guidance that
    a run only gives up as `Failed` after exhausting the recoverable-retry
    budget.
- `crates/tui/command-parser/src/lib.rs` (only if a help/about string is
  factually wrong after the semantics change) — expected to need no change;
  confirm the `resume`/`step` `about =` strings remain accurate.
- `docs/` — if any command-reference doc restates the "resume returns early on
  non-running runs" or "only `resolve` continues a failed run" behavior, correct
  it to match the new semantics. (`AGENTS.md` retry narrative already describes
  give-up keeping the failed step current for continuation and needs no change.)

No changes to `cowboy-workflow-core`, `cowboy-workflow-actions`,
`cowboy-workflow-store`, `cowboy-workflow-agent`, or the ACP crates.

## Tests to be added/updated

- Keep as-is (input, must pass after the fix):
  `crates/workflow/engine/src/runtime.rs::resume_retries_current_step_when_run_failed_by_exhausted_retries`.
- Add `crates/workflow/engine/src/runtime.rs` unit tests:
  - Resume no-op is preserved for `Completed`: resuming a run that already
    completed returns the run unchanged with no new events.
  - Resume re-executes a `WaitingForInput` run: resuming an ask-user run
    re-prompts the retained step, safely replaces the durable pending callback,
    and the run stays answerable via `answer_run`. (Per the later clarification,
    `WaitingForInput` is no longer a no-op.)
  - (If a `Cancelled` run is constructible in tests) resume no-op is preserved
    for `Cancelled`.
  - `step_run` on a Failed current step takes exactly one fresh initial attempt:
    on a scripted agent whose next response is valid, `step_run` advances the
    run past `start`; assert the durable loaded run reflects the new status and
    that `retries_used` / `step_retries_used["start"]` are unchanged by the
    resume flip (only the free initial attempt ran).
  - Resume re-fails deterministically when the fresh initial attempt still fails
    and the step budget is already exhausted: assert the run returns to
    `Failed`, `current_step == "start"`, and the counters are unchanged (no
    budget was available to consume).

## How to verify

1. Confirm the reviewed repro test fails before the code change:
   `cargo test -p cowboy-workflow-engine resume_retries_current_step_when_run_failed_by_exhausted_retries`
   (expected FAIL: run stays `Failed`).
2. Implement the `resume_with` change and add the new tests.
3. Focused runtime tests:
   `cargo test -p cowboy-workflow-engine resume`
   and the exact repro command from step 1 (expected PASS).
4. Guard the UI/CLI delegation is unbroken:
   `cargo test -p cowboy` and `cargo test -p cowboy-command-parser`.
5. Full workspace gate: `cargo test --workspace`.
6. Lint/build hygiene: `cargo clippy --workspace --all-targets` and
   `cargo build --workspace`; fix all compiler and Clippy warnings.
7. Do not push or open a PR. Finish with a single commit ready for review
   (include the `Co-authored-by: Copilot` trailer).

## TODO

- [x] Reproduce the failure: run
  `cargo test -p cowboy-workflow-engine resume_retries_current_step_when_run_failed_by_exhausted_retries`
  and confirm it fails with the run stuck in `Failed`.
- [x] Edit `resume_with` in `crates/workflow/engine/src/runtime.rs`: make `run`
  mutable and replace the non-`Running` guard with a status match that flips
  `RunStatus::Failed { .. }` to `Running` and keeps `Completed` / `Cancelled` as
  safe no-ops. (Per the later clarification, `WaitingForInput { .. }` also flips
  to `Running`.)
- [x] Keep/refresh the `tracing` debug log to record the resume-of-failed
  transition.
- [x] Verify no durable double-write is needed (runner persists the terminal
  status); if a durable `Running` write is required for correctness, add it via
  the existing store/status-application helper rather than raw mutation.
- [x] Add runtime test: resume is a no-op on a `Completed` run (unchanged run,
  no events).
- [x] Add runtime test: resume re-executes a `WaitingForInput` run — it
  re-prompts the retained ask-user step, safely replaces the durable pending
  callback, and the run remains answerable via `answer_run`. (Per the later
  clarification, `WaitingForInput` is no longer a no-op.)
- [x] Add runtime test: resume is a no-op on a `Cancelled` run (if constructible
  in tests; otherwise document why it is covered by the match arm only).
- [x] Add runtime test: `step_run` on a Failed current step takes one fresh
  initial attempt and advances the run; assert durable status and unchanged
  retry counters.
- [x] Add runtime test: resume re-fails deterministically when the fresh initial
  attempt fails with an already-exhausted step budget (run back to `Failed`,
  `current_step` retained, counters unchanged).
- [x] Update `README.md` `step` vs `resume` wording and the "Resolve a failed
  run" section to reflect that `resume`/`step` retry a Failed current step.
- [x] Confirm `crates/tui/command-parser` `about =`/help strings are still
  accurate; update only if factually wrong.
- [x] Grep `docs/` for any statement that resume no-ops on failed runs or that
  only `resolve` continues a failed run; correct if present.
- [x] Run focused tests: the exact repro command plus
  `cargo test -p cowboy-workflow-engine resume`, and confirm PASS.
- [x] Run `cargo test -p cowboy` and `cargo test -p cowboy-command-parser` to
  confirm CLI/TUI delegation is intact.
- [x] Run `cargo test --workspace` and confirm green.
- [x] Run `cargo clippy --workspace --all-targets` and `cargo build --workspace`;
  fix all warnings.
- [x] Create a single review-ready commit (with the `Co-authored-by: Copilot`
  trailer). Do not push or open a PR.
