# Plan

Redesign action execution so every `StepAction` runs through one common host-action flow. The Lua step provider still returns a declarative `StepAction`; the core engine should no longer special-case `ask_user` by mutating `run.resume` or re-running the same step. Instead, the engine builds an action context, sends the action to a centralized dispatcher, receives an `ActionResult`, and applies that result uniformly.

Introduce per-action runners behind the dispatcher. `status`, `agent`, `ask_user`, `fail`, and `suspend` each get an action runner that knows how to turn its action input into an `ActionResult`. The dispatcher owns the variant switch once; `execute_step` should not contain action-specific runtime logic beyond asking the provider for the action and applying the returned result.

Replace the current `ActionExecution` enum with an `ActionResult` model that describes what the action produced, not how the engine should special-case it. The result should support two outcomes:

- completed action: contains the completed `StepRecord`; the engine stores it and routes by `record.output.status`;
- blocked/terminal action: contains a new `RunStatus` without a completed record, used for states such as waiting for user input, suspension, or failure.

`ask_user` then fits the same model as `agent`. The difference is timing, not semantics: an agent usually returns a completed record during the same async dispatch call, while `ask_user` initially returns a waiting `RunStatus` and later, when the user answers, the ask-user runner builds the completed `StepRecord` from the pending context and answer. The next workflow step receives that completed ask-user record as `ctx.prev`.

Use `answered` as the default ask-user output status. Preserve any workflow data needed after the wait by letting `action.ask_user` carry output `fields`; merge the actual answer into those fields when the prompt is answered. This avoids losing pre-prompt `ctx.prev` data when the ask-user record becomes the new run head.

Step accounting invariant: reaching `ask_user` counts once when the prompt is shown; answering completes that already-counted pending action and must not increment `steps_executed`, `step_visits`, or runner budgets again. Capture pending record id, previous-head hash, and start timestamp when the ask-user runner returns `WaitingForInput`; reuse them when the answer completes the record.

Event invariant: answering a prompt must emit and persist a `StepCompleted` event for the ask-user record before any resumed next-step events.

Scope decision from user feedback: do not implement a compatibility or migration path for old persisted `WaitingForInput` runs. This change updates runtime logic, tests, docs, and current example workflow definitions to the new model.

# Changes

- Replace `ActionExecution` in `crates/workflow/core/src/traits.rs` with an `ActionResult` type:
  - `completed(record: StepRecord)` for action outputs that complete a step;
  - `blocked(status: RunStatus)` for action outputs that leave the run waiting/suspended/failed without a completed record;
  - helper constructors and validation so callers do not build invalid results with both/neither outcome.
- Add an action-dispatch seam in `crates/workflow/core/src/traits.rs`:
  - define an `ActionDispatcher` trait that receives a `StepAction` plus `ExecutionContext` and returns `ActionResult`;
  - keep the existing `ExecutionContext` shape or rename it to `ActionContext` if that makes the interface clearer, but it must carry `run_id`, `step_id`, `step_record_id`, previous head, and optional role metadata.
- Refactor `crates/workflow/core/src/engine.rs` so `execute_step`:
  - evaluates the current Lua step through `StepActionProvider`;
  - resolves role metadata for agent actions before dispatch;
  - increments step counters once before dispatch;
  - passes every action variant to `ActionDispatcher`;
  - applies `ActionResult::completed` through one generic step-record application helper;
  - applies `ActionResult::blocked` through one generic run-status application helper.
- Refactor the private `handle_step_record` helper in `crates/workflow/core/src/engine.rs` into a generic reusable record-application function, named along the lines of `apply_step_record`, that stores any completed `StepRecord`, updates `run.head`, routes through `next_step`, persists the run, and updates `RunHead`.
- Update `crates/workflow/core/src/action.rs` so `AskUserAction` can describe its eventual output:
  - add defaulted output status, defaulting to `"answered"`;
  - add defaulted structured `fields` for context that must survive the wait;
  - keep `id`, `message`, and `choices` as prompt metadata.
- Update `RunStatus::WaitingForInput` in `crates/workflow/core/src/state.rs` to retain pending ask-user completion metadata:
  - `step`, `prompt_id`, `message`, and `choices` for UI/event projection;
  - pending `record_id`, previous-head hash, and prompt `started_at`;
  - pending output status and fields copied from `AskUserAction`.
- Update Lua conversion in `crates/workflow/lua/src/convert.rs` so `action.ask_user { ... }` accepts output status and fields while preserving prompt metadata validation.
- Add action runners in the appropriate runtime/core modules:
  - `StatusActionRunner` builds a completed `StepRecord` from `StatusAction`;
  - `FailActionRunner` returns blocked/terminal failed status;
  - `SuspendActionRunner` returns blocked suspended status;
  - `AskUserActionRunner` returns blocked `WaitingForInput` when first dispatched and can build a completed ask-user `StepRecord` when provided an answer;
  - `AgentActionRunner` wraps the existing `cowboy-workflow-agent::AgentExecutor` behavior and returns completed records.
- Add a centralized dispatcher implementation in `cowboy-workflow-engine` wiring those runners together:
  - dispatch `StepAction::Agent` to the existing agent executor adapter;
  - dispatch `StepAction::AskUser` to the new ask-user runner;
  - dispatch `status`, `fail`, and `suspend` to their runners;
  - keep ACP/client-specific behavior in `cowboy-workflow-agent`, not in core.
- Update `crates/workflow/engine/src/input.rs` so it no longer mutates `run.resume`. It should validate a waiting prompt answer, call the ask-user runner's answer-completion path, and return the completed ask-user `StepRecord` plus resulting answer metadata for runtime event emission.
- Update `crates/workflow/engine/src/runtime.rs::answer_run` so it:
  - loads and compiles the run's workflow snapshot before applying the answer;
  - calls the ask-user runner/input router to produce an `ActionResult::completed` ask-user record;
  - applies that completed record through the generic core record-application helper without incrementing step counters;
  - emits/persists the ask-user `StepCompleted` and status events before any events collected from resuming subsequent steps;
  - resumes from the next step only when answer routing leaves the run `Running`.
- Preserve the UI-facing `WorkflowEventKind::WaitingForInput` payload in `crates/workflow/engine/src/events.rs`; update all `RunStatus::WaitingForInput` constructors and matches in core, engine, CLI test apps, and TUI tests for the added internal fields.
- Update `crates/workflow/engine/src/runner.rs` so Lua workflow context no longer exposes prompt answers through `ctx.resume`. `ctx.prev` after an answer should include `action = "ask_user"`, `status = "answered"` unless overridden, `fields.answer`, prompt metadata, body, and raw answer metadata.
- Update current workflow definitions that read `ctx.resume`:
  - `crates/workflow/engine/test_files/00-demo.lua` should ask in one step and route on `answered` to a normal decision/status step that reads `ctx.prev.fields.answer`;
  - `crates/workflow/engine/test_files/agent/00-feature.lua` should handle clarification through `ctx.prev.fields.answer` instead of scanning `ctx.resume`;
  - `examples/workflows/steps/clarify.lua`, `blocked.lua`, `confirm_plan.lua`, and `confirm_result.lua` should stop re-entering the ask step and instead pass required pre-prompt fields through `action.ask_user.fields` for the follow-up triage/status step;
  - `examples/workflows/utils/context.lua` should stop deriving clarification context from `ctx.resume`.
- Update workflow graph definitions in `examples/workflows/workflows/feature.lua` and `bugfix.lua` so ask-user steps transition on `answered` into explicit follow-up steps that interpret `ctx.prev.fields.answer`.
- Update documentation in `docs/architecture.md` and `docs/module-map.md` to describe the action dispatcher, per-action runners, `ActionResult`, and ask-user answer delivery through `ctx.prev`.
- Remove or update obsolete comments/tests that describe `ctx.resume` as the active answer path. If `WorkflowRun.resume` remains in the serialized model temporarily, treat it as inactive legacy state and not part of the Lua authoring contract.

# Tests to be added/updated

- Add core tests for `ActionResult` constructors/invariants so invalid completed/blocked result shapes cannot be constructed through the public helpers.
- Add core tests proving `execute_step` dispatches each `StepAction` variant through `ActionDispatcher` instead of handling variants inline.
- Add core tests for the generic step-record application helper: stores the record, updates `run.head`, routes by `output.status`, persists run state, and completes the run when no transition exists.
- Add core tests proving `StatusActionRunner`, `FailActionRunner`, and `SuspendActionRunner` produce the expected `ActionResult` shapes.
- Add engine tests proving the centralized dispatcher routes `agent`, `ask_user`, `status`, `fail`, and `suspend` to the correct runner.
- Add ask-user runner tests proving initial dispatch returns `WaitingForInput` with pending record/output metadata and does not write a `StepRecord` yet.
- Update `crates/workflow/engine/src/input.rs` tests so answer submission validates prompt id/choices, does not mutate `run.resume`, reuses pending record id/previous head, builds a completed ask-user record, and does not increment step counters.
- Add an engine-runtime test proving `answer_run` applies the ask-user completed record through the common record path and returns/persists the ask-user `StepCompleted` event before resumed next-step events.
- Update `crates/workflow/engine/src/events.rs` tests so `RunStatus::WaitingForInput` with internal pending metadata still projects to the same `WorkflowEventKind::WaitingForInput` payload.
- Update affected CLI/TUI tests that construct or match `RunStatus::WaitingForInput` or `WorkflowEventKind::WaitingForInput`.
- Update Lua conversion tests to cover optional ask-user output status and fields.
- Update `crates/workflow/engine/src/runner.rs` Lua-provider tests to cover a workflow where step A asks, step B reads `ctx.prev.action == "ask_user"` and `ctx.prev.fields.answer`, and step C receives step B's normal status output.
- Update workflow fixture tests in `crates/workflow/engine/src/runtime.rs` that currently pass `resume` JSON, including clarification, plan confirmation, result confirmation, and blocked-flow tests.
- Add or update example workflow graph tests proving `confirm_plan`, `confirm_result`, `blocked`, and clarification flows transition through `answered` and then route based on a normal follow-up status step.

# How to verify

- Run `cargo test -p cowboy-workflow-core action_result`.
- Run `cargo test -p cowboy-workflow-core action_dispatch`.
- Run `cargo test -p cowboy-workflow-core apply_step_record`.
- Run `cargo test -p cowboy-workflow-lua ask_user`.
- Run `cargo test -p cowboy-workflow-engine input`.
- Run `cargo test -p cowboy-workflow-engine action_dispatcher`.
- Run `cargo test -p cowboy-workflow-engine events`.
- Run `cargo test -p cowboy-workflow-engine runner`.
- Run `cargo test -p cowboy-workflow-engine runtime`.
- Run `cargo test -p cowboy-workflow-engine example_workflows`.
- Run `cargo test -p cowboy` if TUI event/state constructors change outside existing focused coverage.
- Manually smoke test the status-only demo workflow with `engine-cli`: start the demo workflow, answer its prompt, confirm the persisted head record has `action = "ask_user"`, confirm event logs contain the ask-user `StepCompleted` before resumed next-step events, and confirm the next Lua step consumed `ctx.prev.fields.answer`.

# TODO

- [x] Replace `ActionExecution` with `ActionResult` and helper constructors in `crates/workflow/core/src/traits.rs`.
- [x] Add an `ActionDispatcher` trait in core for dispatching `StepAction` plus action context to per-action runners.
- [x] Refactor `execute_step` to dispatch every action through `ActionDispatcher` and apply only `ActionResult` values.
- [x] Refactor the private step-record handler into a generic reusable `apply_step_record`-style helper.
- [x] Update status, agent, and future completed-action paths to use the same generic record-application helper.
- [x] Add defaulted output status and structured fields to `AskUserAction`.
- [x] Extend Lua ask-user conversion to accept output status and fields.
- [x] Extend `RunStatus::WaitingForInput` with pending record id, previous-head hash, started timestamp, output status, and output fields while preserving UI-facing event payloads.
- [x] Implement a status action runner that returns a completed `ActionResult`.
- [x] Implement fail and suspend action runners that return blocked/terminal `ActionResult` values.
- [x] Implement an ask-user action runner that returns waiting `ActionResult` on initial dispatch and can complete pending input into an ask-user `StepRecord` on answer.
- [x] Adapt the existing agent executor behind an agent action runner that returns completed `ActionResult` values.
- [x] Implement the centralized engine action dispatcher that wires status, agent, ask-user, fail, and suspend runners.
- [x] Update runtime runner wiring to pass the centralized dispatcher into `WorkflowRunner`.
- [x] Preserve step-counter invariants: initial ask increments once; answer completion never increments `steps_executed`, `step_visits`, or budgets.
- [x] Update `InputRouter` to validate answers and invoke ask-user completion instead of mutating `run.resume`.
- [x] Update `WorkflowRuntime::answer_run` to apply ask-user completed records through the common record path, emit/persist completion events, and resume only when routing leaves the run running.
- [x] Update runtime event collection/persistence so answer-time `StepCompleted` and status events precede resumed-step events in `RunReport.events` and event-log JSON.
- [x] Update `WorkflowEventKind::from(&RunStatus)` and tests so internal waiting metadata does not leak into TUI-facing waiting events.
- [x] Update all affected `RunStatus::WaitingForInput` constructors and match patterns in core, engine, CLI test apps, and TUI tests.
- [x] Update Lua context construction so prompt answers are consumed from `ctx.prev`, not `ctx.resume`.
- [x] Update `crates/workflow/engine/test_files/00-demo.lua` to split prompting from answer interpretation.
- [x] Update `crates/workflow/engine/test_files/agent/00-feature.lua` to remove clarification reads from `ctx.resume`.
- [x] Update example workflow ask/confirmation/blocked steps to pass required context through ask-user fields and read answers from `ctx.prev.fields.answer`.
- [x] Update feature and bugfix example workflow transitions to route ask-user steps through `answered` follow-up steps.
- [x] Update user-input architecture/module documentation to describe the dispatcher, action runners, `ActionResult`, and `ctx.prev` answer delivery.
- [x] Add core `ActionResult` invariant tests.
- [x] Add core action-dispatch tests covering every `StepAction` variant.
- [x] Add generic record-application helper tests.
- [x] Add action runner tests for status, fail, suspend, ask-user, and agent dispatch behavior.
- [x] Update input-router validation tests to assert no active `run.resume` answer flow and no answer-time step-counter increment.
- [x] Add runtime event tests for answer-time ask-user `StepCompleted` emission and persistence.
- [x] Add event projection tests for waiting statuses with pending metadata.
- [x] Update Lua provider/runtime tests for `ctx.prev` answer delivery.
- [x] Update example workflow tests for clarification, confirmation, blocked, and result-confirmation flows.
- [x] Run the focused core, Lua, engine, and TUI test commands listed in How to verify.
- [x] Perform the manual `engine-cli` demo prompt-answer smoke test.
