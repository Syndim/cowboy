# Plan

Complete the TUI transcript card migration in `crates/tui/app` without changing command parsing, workflow execution, persisted events, CLI output, or composer/status-strip behavior. The remaining unframed transcript output is concentrated in `TranscriptEntry::Plain`: immediate labels from `AppState::spawn_report_task` and background-task error/cancellation results. Production command callers are the stepwise and workflow-specific `/run` variants plus `/step`, `/resume`, `/answer`, and the status-bearing `/resolve` path.

Make cards the only non-workflow transcript representation. Preserve each existing full submission string in `AppState::status()` because the status strip and command behavior tests rely on it, but split transcript presentation into a concise card title/context and a body containing only the user-facing request or run/prompt/status identifiers. For example, `/run --workflow test-failure-fix Fix recent test failures` should produce a `Run` card whose title retains the submitted workflow context and whose framed body contains `Fix recent test failures`, rather than a bare `submitted run --workflow test-failure-fix: Fix recent test failures` line.

Use the existing `Card`, `TranscriptEntry::Card`, `push_card`, and card-report-task path. Do not add another renderer or duplicate status glyph logic. Keep submitted answer text out of the immediate transcript card, matching the current behavior that displays only the run and prompt identifiers.

# Changes

- Update `crates/tui/app/src/app/commands.rs` so every background command submission uses the card-report-task path:
  - render `/run --step`, `/run --workflow <workflow-id>`, and `/run --step --workflow <workflow-id>` as `Run` cards consistent with the existing plain/default `/run` card;
  - keep the fixed initial `00:00:00` title prefix, identify the selected flags/workflow in the title context, and place only the request text in the framed body;
  - render `/step`, `/resume`, `/answer`, and status-bearing `/resolve` submissions as concise action cards with their run, prompt, and status identifiers in the card context/body instead of as bare labels;
  - preserve the exact existing `submitted ...` status strings, pending-prompt clearing, runtime method calls, arguments, task spawning, and background-task count semantics.
- Update `crates/tui/app/src/app/state.rs` so background task completion feedback also uses cards:
  - map runtime-returned errors and failed task joins to `Error` cards;
  - map cancelled task joins to a `Cancelled` card;
  - preserve the current status text and run-state transitions for each branch.
- Remove the obsolete plain transcript path after all producers are migrated: delete `TranscriptEntry::Plain`, its rendering/plain-text branches, `render_plain_lines`, and the plain-producing `spawn_report_task` API. Keep one shared internal task-spawn implementation beneath the card-aware entry point so status assignment, `running` state, transcript insertion, and Tokio spawning remain atomic and consistent.
- Update `crates/tui/app/src/app/controls/transcript.rs` to remove the `TranscriptEntry::Plain` rendering branch. Cards remain width-aware; workflow events retain their existing streaming/tail behavior.
- Mechanically migrate test setup code that currently calls `spawn_report_task` to the card-aware helper (using local test helpers where repeated) without changing the production behavior those fixtures exercise.
- Keep the change TUI-local. Do not change `cowboy-command-parser`, `cowboy-workflow-engine`, Lua workflows, workflow event schemas, redb persistence, ACP behavior, or non-interactive CLI formatting.

# Tests to be added/updated

- Expand command tests in `crates/tui/app/src/app/commands.rs` to cover every immediate submission path: plain/default `/run`, `/run --step`, `/run --workflow`, `/run --step --workflow`, `/step`, `/resume`, `/answer` (explicit and pending-prompt fallback), and status-bearing `/resolve`.
- For each path, assert observable card output: a card title, rounded framed body rows, no bare leading `submitted ...` line, and retention of the relevant request/workflow/run/prompt/status information. Also assert the exact pre-existing `AppState::status()` value and one spawned background task.
- Keep `plain_request_submission_renders_initial_input_as_card` as the baseline contract for the default run path, including its exact `00:00:00 · ◌ Run · submitted run` title and prefix-free request body.
- Update state tests in `crates/tui/app/src/app/state.rs` to assert that a runtime error, cancelled join, and failed join render `Error`/`Cancelled` cards while preserving status and run-state behavior.
- Update transcript and draw fixtures that used plain report labels so they continue to cover composer locking, status metadata, scrolling, resizing, and task draining with card entries.
- Do not add source-text assertions. The behavioral tests should verify rendered rows and state transitions; deleting `TranscriptEntry::Plain` provides the compile-time guarantee that new non-workflow transcript entries cannot bypass card rendering.

# How to verify

1. Run the focused command submission tests:
   `cargo test -p cowboy app::commands::tests -- --nocapture`
2. Run the focused state tests, including background task drain/error/cancellation coverage:
   `cargo test -p cowboy app::state::tests -- --nocapture`
3. Run transcript and draw regressions after removing the plain-entry branch:
   `cargo test -p cowboy app::controls::transcript::tests -- --nocapture`
   `cargo test -p cowboy app::tests -- --nocapture`
4. Run the complete TUI package test suite:
   `cargo test -p cowboy`
5. Check formatting and Rust warnings:
   `cargo fmt --check`
   `cargo clippy -p cowboy --all-targets -- -D warnings`
6. Manually launch `cargo run -p cowboy` and submit representative commands for all migrated categories. Confirm each immediate transcript entry has a card title and rounded border, the workflow-specific run card shows the selected workflow and request without a bare `submitted run --workflow ...:` line, answer cards do not echo answer text, and runtime failures appear as `Error` cards.

# TODO

- [x] Migrate all stepwise and workflow-specific run submission paths to the existing card-report-task API.
- [x] Migrate `/step`, `/resume`, explicit and fallback `/answer`, and status-bearing `/resolve` submissions to action cards.
- [x] Preserve every existing submission status string, runtime call, argument, pending-prompt transition, and background-task state transition.
- [x] Convert runtime errors, cancelled joins, and failed joins from plain transcript lines to `Error` or `Cancelled` cards.
- [x] Remove `TranscriptEntry::Plain`, its renderer branches, `render_plain_lines`, and the plain-producing report-task API after all producers are migrated.
- [x] Update transcript rendering to operate only on workflow-event and card entries.
- [x] Migrate test fixtures from the removed plain report-task helper to card-backed task fixtures.
- [x] Add or update command regressions for every submitted command variant and their rendered card/status contracts.
- [x] Add or update state regressions for card-rendered background task failures and cancellations.
- [x] Run the focused command, state, transcript, and draw tests; then run the full `cowboy` package tests, formatting check, Clippy check, and manual TUI smoke verification.

# Manual TUI verification evidence

Executed `cargo run -p cowboy` on 2026-07-15 in a 120×40 pseudo-terminal with isolated XDG config and state directories. Observed results:

- `/run --step Manual cancellation check` rendered a rounded `Run` card containing only the request. Pressing `Esc` rendered a rounded `Cancelled` card with `cancelled 1 background task(s)`.
- `/run --workflow test-failure-fix Fix recent test failures` rendered the workflow id in the `Run` title and the request in the rounded body, with no bare `submitted run --workflow ...:` body line.
- `/step run-smoke` and `/resume run-smoke` rendered rounded action cards containing the run id.
- `/resolve run-smoke accepted` rendered `● Resolve · submitted resolve`, not the completed glyph, with the run id and status in the rounded body.
- `/answer run-redaction prompt-redaction answer-must-not-render` rendered the run and prompt ids after submission; `answer-must-not-render` was absent from post-submit output.
- Invalid workflow and run operations rendered rounded `Error` cards.

All representative interactive scenarios passed, and the TUI exited normally.
