# Plan

Introduce a durable resume-callback model for actions that stop at an external input boundary. Do not persist Rust closures or Lua continuations; persist a small `ResumeCallback` descriptor on the waiting run, then rebuild a process-local callback registry when the runtime handles the answer. This keeps workflow state restart-safe while matching the requested design: an action registers a resumable callback when it blocks, and answer handling searches registered callbacks instead of directly calling the ask-user runner.

`action.ask_user` becomes the first and only resume-callback producer. On initial dispatch, `AskUserActionRunner` returns `RunStatus::WaitingForInput` containing UI prompt metadata plus a callback descriptor whose payload is the pending ask-user completion data. On answer, a renamed `ResumeRouter` validates prompt id/choices, looks up the callback kind in a `ResumeCallbackRegistry`, invokes the matching handler, applies the returned `ActionResult`, emits the same ask-user `StepCompleted` event ordering, and continues the workflow through the existing runtime loop.

Remove the unused suspend action as a clean cutover. No current workflow files call `action.suspend`; only docs, tests, event/UI affordances, Lua conversion, and action-dispatch plumbing mention it. The implementation should remove `StepAction::Suspend`, `SuspendAction`, `SuspendActionRunner`, Lua `action.suspend`, `RunStatus::Suspended`, `WorkflowEventKind::Suspended`, CLI/TUI suspended rendering, and authoring docs that advertise suspend. Historical persisted runs with a suspended status will no longer deserialize; that is the main compatibility tradeoff of a full removal.

# Changes

- In `crates/workflow/core/src/state.rs`, add a serializable `ResumeCallback` model, likely `{ kind: String, payload: serde_json::Value }`, with constructor helpers that reject empty kinds.
- Reshape `RunStatus::WaitingForInput` so it keeps UI-facing prompt fields (`step`, `prompt_id`, `message`, `choices`) plus the generic `resume_callback`; move ask-user-specific pending fields (`record_id`, `prev`, `started_at`, `output_status`, `output_fields`) into the callback payload.
- Remove `RunStatus::Suspended` from core state and update `RunHead` serialization expectations accordingly.
- In `crates/workflow/core/src/action.rs`, remove `StepAction::Suspend`, `SuspendAction`, and the `"suspend"` branch from `StepAction::action_name`.
- In `crates/workflow/core/src/traits.rs`, add resume callback interfaces: a `ResumeInput` value for prompt id, answer text, and completion time; a `ResumeCallbackHandler` trait that handles a persisted `ResumeCallback`; and/or a result type that returns `ActionResult` so callbacks can complete a step or update run status through the common path.
- Update core `execute_step` tests and test dispatchers in `crates/workflow/core/src/engine.rs` to cover only `agent`, `status`, `ask_user`, and `fail` action variants.
- In `crates/workflow/actions/src/ask_user.rs`, make `AskUserActionRunner::run` register the ask-user resume callback descriptor in `WaitingForInput` instead of storing pending ask-user fields directly on the run status.
- In `crates/workflow/actions/src/ask_user.rs`, implement the resume-callback handler for `kind == "ask_user"`; it should deserialize pending ask-user payload, merge the answer into fields, and return `ActionResult::Completed` with the ask-user `StepRecord`.
- In `crates/workflow/actions/src/lib.rs`, add a `ResumeCallbackRegistry` or equivalent lookup owned near `EngineActionDispatcher`, register the ask-user handler, and expose a small constructor the engine runtime can use for answer handling.
- Delete `crates/workflow/actions/src/suspend.rs`, remove its module/export, and remove dispatcher/test cases for `StepAction::Suspend`.
- Rename or refactor `crates/workflow/engine/src/input.rs` from an ask-specific `InputRouter` to a generic `ResumeRouter` that depends on the callback registry, validates the waiting prompt, finds the callback, and returns the callback `ActionResult`.
- Update `WorkflowRuntime::answer_run` in `crates/workflow/engine/src/runtime.rs` to invoke `ResumeRouter` instead of `InputRouter::answer`, then apply `ActionResult::Completed` through `apply_step_record` or `ActionResult::Blocked` through `apply_run_status` before optionally continuing `run_existing_with_events`.
- Preserve answer-time invariants in `WorkflowRuntime::answer_run`: no step-budget increment on answer, ask-user `StepCompleted` emitted before resumed step events, and resume only when callback application leaves the run `Running`.
- Update `crates/workflow/engine/src/events.rs` so `WorkflowEventKind::from(&RunStatus)` still projects `WaitingForInput` without leaking callback payloads, and remove the `Suspended` event variant.
- Update TUI handling in `crates/tui/src/app/state.rs`, `crates/tui/src/app/events.rs`, and `crates/tui/src/app/styles.rs` to remove suspended run-state rendering and warning aliases.
- Update CLI/test-app output in `crates/workflow/engine/src/bin/engine-cli.rs` and `crates/workflow/store/src/bin/store-cli.rs` to remove suspended status formatting and `suspended:` parsing.
- Update Lua authoring support in `crates/workflow/lua/src/api.rs` and `crates/workflow/lua/src/convert.rs` to remove `action.suspend`; `action.suspend` should now be unavailable/unknown.
- Update Lua/runtime tests in `crates/workflow/lua/src/runtime.rs` and engine fixture tests that enumerate action kinds or construct waiting statuses for the new callback-backed shape.
- Update documentation in `docs/architecture.md`, `docs/module-map.md`, `docs/workflow-authoring.md`, `README.md`, and any affected plan/test notes so they describe resume callbacks and no longer list suspend as an action or run lifecycle.

# Tests to be added/updated

- Add core unit tests for `ResumeCallback` construction/serialization and rejection of empty callback kinds.
- Update core `RunStatus::WaitingForInput` JSON tests to assert the new prompt-plus-callback shape and absence of ask-user pending fields at the top level.
- Update core action serialization tests to assert the remaining `StepAction` variants and that `suspend` is no longer a valid `StepAction` variant.
- Update core `execute_step` dispatch tests to cover `agent`, `status`, `ask_user`, and `fail`, with no suspend case.
- Add action-runner tests proving `AskUserActionRunner::run` stores an `ask_user` resume callback descriptor with the pending record id, previous head, start time, output status, and output fields in the payload.
- Add callback-handler tests proving the ask-user resume callback returns the same completed ask-user `StepRecord` shape as today, including `fields.answer`, body, raw prompt metadata, previous head, and preserved record id.
- Add registry tests proving a known callback kind is dispatched and an unknown/missing callback kind returns a clear `WorkflowError::InvalidAction`.
- Update engine resume/input tests to prove prompt id and choices are still validated before callback invocation.
- Update engine resume/input tests to prove answering does not mutate `run.resume`, `steps_executed`, or `step_visits`.
- Update `WorkflowRuntime::answer_run` tests to prove callback-produced `ActionResult::Completed` is applied through `apply_step_record`, events are persisted in the existing ask-user-completed-before-resumed order, and resumed failures still preserve the answer-time event prefix.
- Add/update event projection tests to prove `WaitingForInput` events expose only `step`, `prompt_id`, `message`, and `choices`, not callback payloads.
- Update TUI state/event tests to remove suspended-state assertions and keep waiting prompt behavior unchanged.
- Update Lua conversion/runtime tests to prove `action.suspend` is unknown/unavailable and existing `action.ask_user` conversion still accepts `status` and `fields`.
- Update CLI/test-app tests or snapshots that print/list run statuses to remove suspended output.
- Update docs tests if any documentation examples are compiled or linted.

# How to verify

- Run `cargo test -p cowboy-workflow-core resume_callback`.
- Run `cargo test -p cowboy-workflow-core waiting_for_input`.
- Run `cargo test -p cowboy-workflow-core action_dispatch`.
- Run `cargo test -p cowboy-workflow-actions ask_user`.
- Run `cargo test -p cowboy-workflow-actions resume_callback`.
- Run `cargo test -p cowboy-workflow-lua suspend`.
- Run `cargo test -p cowboy-workflow-lua ask_user`.
- Run `cargo test -p cowboy-workflow-engine input` or the renamed `resume` filter after the router rename.
- Run `cargo test -p cowboy-workflow-engine runtime answer_run`.
- Run `cargo test -p cowboy-workflow-engine events`.
- Run `cargo test -p cowboy` if TUI suspended-state code or tests changed.
- Run the status-only demo workflow through `engine-cli`: start the demo, answer the ask-user prompt, confirm the prompt answer still produces an ask-user `StepCompleted` event before resumed-step events, and confirm the next step receives the answer through `ctx.prev.fields.answer`.

# TODO

- [x] Add the core `ResumeCallback` data model and constructor validation.
- [x] Reshape `RunStatus::WaitingForInput` to store prompt metadata plus a generic resume callback descriptor.
- [x] Add core resume callback handler/input traits or equivalent callback result interfaces.
- [x] Move ask-user pending completion metadata into the ask-user callback payload.
- [x] Implement ask-user resume callback handling that returns a completed ask-user `ActionResult`.
- [x] Add the process-local resume callback registry and register the ask-user handler.
- [x] Refactor `InputRouter` into a generic `ResumeRouter` that dispatches by callback kind.
- [x] Update `WorkflowRuntime::answer_run` to apply callback-produced `ActionResult` values through common status/record helpers.
- [x] Preserve answer-time event ordering and no-budget-increment invariants.
- [x] Remove `StepAction::Suspend` and the `SuspendAction` type from core.
- [x] Delete the suspend action runner and remove suspend from the action dispatcher.
- [x] Remove `RunStatus::Suspended` and suspended event projection.
- [x] Remove Lua `action.suspend` registration and conversion.
- [x] Remove suspended-state rendering and formatting from TUI, engine CLI, and store CLI helpers.
- [x] Update all tests and fixtures that construct `WaitingForInput`, enumerate action variants, or mention suspended statuses.
- [x] Update README and workflow documentation to describe resume callbacks and remove suspend from the authoring surface.
- [x] Add/update focused core tests for resume callback state and action variant removal.
- [x] Add/update actions tests for ask-user callback registration, callback completion, and registry lookup errors.
- [x] Add/update engine runtime tests for answer routing through the callback registry.
- [x] Add/update Lua tests proving `action.suspend` is unavailable and `action.ask_user` still works.
- [x] Add/update TUI/CLI tests affected by suspended-status removal.
- [x] Run the focused verification commands listed above.
- [x] Run the manual `engine-cli` ask-user resume smoke test.
