# Plan: `resume`/`step` re-execute the retained `WaitingForInput` current step

Bug-fix plan grounded in the reviewed RCA:
[`rca.md`](./rca.md). This revises the existing `fix/resume-failed-step` branch
(PR #1) so `resume`/`step` retry the retained current step for **every**
non-terminal run status. The prior step already added the failing regression
test
`crates/workflow/engine/src/runtime.rs::resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback`;
that test is an input to this fix and must not be rewritten or replaced.

## Plan

The clarified requirement: `cowboy resume <run-id>` / `/resume` and
`cowboy step <run-id>` / `/step` must re-execute the retained current step for
**every** non-terminal status. Only `RunStatus::Completed` and
`RunStatus::Cancelled` are legitimate no-ops. Therefore `Running`, `Failed`, and
`WaitingForInput` must all proceed through execution. The current branch already
handles `Running` and `Failed`; the remaining gap is that `WaitingForInput` is
still lumped into the early-return no-op arm of `WorkflowRuntime::resume_with`.

Root cause (per RCA): the status match in `resume_with`
(`crates/workflow/engine/src/runtime.rs`) puts `WaitingForInput` in the same arm
as `Completed`/`Cancelled` and returns early, so the runner never re-executes
the retained `ask_user` step. The downstream runner only executes while
`run.status == Running`, so the fix must flip a waiting run back to `Running`
before re-entering `run_existing`, exactly as the `Failed` arm already does.

Approach: make the smallest centralized change in `resume_with`. Move
`WaitingForInput { .. }` out of the early-return arm and treat it like the
`Failed` arm — flip it to `RunStatus::Running` via `apply_run_status`, persist,
and re-enter `run_existing`. Re-executing the `ask_user` step increments
`steps_executed`, mints a fresh `record_id`, and overwrites the prior
`WaitingForInput` status, so the durable pending resume callback is safely
replaced (not duplicated or orphaned). `answer_run` is unchanged: after resume,
the run is again `WaitingForInput` with a fresh callback and remains answerable.

Non-goals / invariants to preserve:
- Do not change `answer_run`; answering must still route through the (possibly
  replaced) pending callback and advance the run.
- Do not add divergent status handling in the CLI (`crates/tui/app/src/main.rs`)
  or TUI (`crates/tui/app/src/app/commands.rs`); both already delegate to
  `resume_run`/`step_run`.
- Keep `execute_step`/`RunStore` budget-and-record semantics in
  `cowboy-workflow-core` unchanged; re-executing the current step already
  consumes one step/visit through the normal initial-attempt path.
- Preserve `user_feedback` exactly in output fields when present; it is
  cumulative raw user direction and must not be augmented with agent- or
  reviewer-generated feedback.
- Amend on the current `fix/resume-failed-step` branch; do not create a new
  branch or PR and do not push.

## Changes

- `crates/workflow/engine/src/runtime.rs` — `WorkflowRuntime::resume_with`:
  - Move `RunStatus::WaitingForInput { .. }` from the `Completed | Cancelled |
    WaitingForInput { .. }` early-return arm into an arm that flips the run to
    `RunStatus::Running` (via `apply_run_status`) and falls through to
    `run_existing`. Simplest form: fold it into the existing `Failed { .. }`
    arm, e.g. `RunStatus::Failed { .. } | RunStatus::WaitingForInput { .. } =>
    { ... apply_run_status(Running) ... }`, leaving the no-op arm as
    `RunStatus::Completed | RunStatus::Cancelled`.
  - Update the code comment so it states that only `Completed` and `Cancelled`
    are non-resumable no-ops, and that `WaitingForInput` (like `Failed`) flips
    to `Running` so the retained current step is re-executed and the durable
    pending resume callback is safely replaced.
  - Do not touch `answer_run`, `run_existing`, or the `Running`/`Failed`
    execution mechanics beyond the match arm.
- `README.md` (~line 141) — replace "`Completed`, `Cancelled`, and
  waiting-for-input runs are left unchanged; answer a waiting run with `answer`
  instead." Only `Completed` and `Cancelled` are left unchanged; `resume`/`step`
  re-execute (re-prompt) a `WaitingForInput` run's retained `ask_user` step, and
  `answer` remains the way to supply a prompt answer.
- `docs/architecture.md` — align any wording that implies `WaitingForInput` is
  non-resumable. Update the retry paragraph (~lines 225–229) and, if needed, the
  idle-status table (~line 244) so they state only `Completed`/`Cancelled` are
  non-resumable no-ops, and that `/resume` and `/step` re-execute the retained
  current step for `Running`/`Failed`/`WaitingForInput`.
- `docs/plans/resume_and_step_skip_waiting_for_input_current_step/rca.md` — keep
  as the authoritative RCA; already states only `Completed`/`Cancelled` are
  no-ops. No content change required beyond confirming alignment.
- `docs/plans/resume_does_not_retry_failed_current_step/rca.md` and
  `.../plan.md` — update the sibling (pre-clarification) language so it no longer
  lists `WaitingForInput` as a non-resumable no-op; state only `Completed` and
  `Cancelled` are non-resumable. Do not resurrect the removed no-op-for-waiting
  test expectation.

## Tests to be added/updated

- Keep (input, do not rewrite): the prior step's failing regression test
  `crates/workflow/engine/src/runtime.rs::resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback`.
  It must fail before the code change and pass after. It asserts the run stays
  `WaitingForInput` on `ask`/`approval`, a fresh `StepStarted { step_id: "ask"
  }` event is emitted, the durable pending callback `record_id` changes, and the
  run remains answerable to `Completed`.
- Update/remove the contradicting existing test
  `crates/workflow/engine/src/runtime.rs::resume_is_noop_for_waiting_run_and_prompt_stays_answerable`.
  It encodes the pre-clarification "resume is a no-op on a waiting run"
  expectation (asserts no events and an unchanged run) and will contradict the
  new behavior. Remove it (its answerability coverage is subsumed by the
  regression test) or repurpose it to assert the new re-prompt behavior; do not
  leave an assertion that resume no-ops on `WaitingForInput`.
- Keep unchanged: `resume_is_noop_for_completed_run` and
  `resume_is_noop_for_cancelled_run` — these remain valid because only
  `Completed`/`Cancelled` are no-ops.
- Confirm no other resume/step tests (e.g. the `Failed`/`Running` resume and
  `answer_run` tests) regress; adjust only if the new match arm changes their
  observed events.

## How to verify

- Focused repro test (must pass after the fix):
  - `cargo test -p cowboy-workflow-engine resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback`
- Focused resume/answer tests in the engine crate:
  - `cargo test -p cowboy-workflow-engine resume`
  - `cargo test -p cowboy-workflow-engine answer_run`
- Formatting: `cargo fmt --check`
- Full workspace tests: `cargo test --workspace`
- Lints: `cargo clippy --workspace --all-targets -- -D warnings`
- Manual sanity (optional): start a run whose current step is an `ask_user`
  step so it blocks `WaitingForInput`, run `cowboy resume <run-id>`, and confirm
  the run is re-prompted (still `WaitingForInput`, new `StepStarted` event) and
  still answerable via `cowboy answer <run-id> approval yes`.
- Amend the change onto the current `fix/resume-failed-step` branch (e.g.
  `git commit --amend` or a fixup folded into the branch tip); do not create a
  new branch or PR and do not push.

## TODO

- [x] Confirm the repro test
  `resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback` fails
  before any product-code change
  (`cargo test -p cowboy-workflow-engine resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback`).
- [x] In `crates/workflow/engine/src/runtime.rs`, edit `resume_with` to move
  `RunStatus::WaitingForInput { .. }` out of the `Completed | Cancelled`
  early-return arm and into the arm that flips the run to `RunStatus::Running`
  (fold into the `Failed { .. }` arm) so it re-enters `run_existing`.
- [x] Leave the no-op early-return arm as exactly
  `RunStatus::Completed | RunStatus::Cancelled`.
- [x] Update the `resume_with` code comment to state only `Completed`/`Cancelled`
  are non-resumable and that `WaitingForInput` is re-executed (re-prompted) with
  its durable pending callback safely replaced.
- [x] Verify `answer_run`, `run_existing`, and the `Running`/`Failed` paths are
  otherwise unchanged.
- [x] Remove or repurpose
  `resume_is_noop_for_waiting_run_and_prompt_stays_answerable` so no test asserts
  resume no-ops on `WaitingForInput`.
- [x] Keep `resume_is_noop_for_completed_run` and
  `resume_is_noop_for_cancelled_run` unchanged.
- [x] Update `README.md` (~line 141) so only `Completed`/`Cancelled` runs are
  left unchanged and `resume`/`step` re-prompt a `WaitingForInput` run.
- [x] Update `docs/architecture.md` (retry paragraph ~225–229 and idle-status
  table ~244) so wording states only `Completed`/`Cancelled` are non-resumable.
- [x] Update the sibling
  `docs/plans/resume_does_not_retry_failed_current_step/rca.md` and `plan.md`
  language to state only `Completed`/`Cancelled` are non-resumable (drop
  `WaitingForInput` from the no-op list).
- [x] Ensure `user_feedback` output fields are preserved exactly (cumulative raw
  user direction, not augmented).
- [x] Run `cargo test -p cowboy-workflow-engine resume_reexecutes_waiting_ask_user_step_and_replaces_pending_callback`
  and confirm it now passes.
- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings` and fix all
  warnings.
- [x] Amend the change on the current `fix/resume-failed-step` branch; do not
  create a new branch or PR and do not push.
