# Plan

Base the fix on the reviewed RCA at `docs/plans/initial_user_input_lacks_card_ui/rca.md` and the existing investigator-added regression test `crates/tui/app/src/app/commands.rs::plain_request_submission_renders_initial_input_as_card`.

Fix the presentation seam in the TUI app layer only. The plain initial request path already starts the correct workflow run; the bug is that the immediate submission notice is inserted as `TranscriptEntry::Plain` before workflow events arrive. Make that immediate initial request notice render as a `Run` card while preserving the submitted label text, background task spawning, composer locking, input history, and runtime dispatch behavior.

Do not change workflow runtime behavior, Lua workflow behavior, catalog selection, slash command parsing, or the existing repro test's contract.

# Changes

- Update `crates/tui/app/src/app/state.rs` to support starting a background report task while pushing a card transcript entry instead of a plain transcript entry. Prefer a small internal helper that preserves the current `spawn_report_task` semantics: set `status` to the submitted label, set `run_state` to `running`, push the transcript entry, then spawn the future.
- Keep the existing `spawn_report_task` behavior available for current plain-label callers unless a caller is intentionally migrated.
- Update `crates/tui/app/src/app/commands.rs` so the plain non-slash initial request path (`spawn_start_run`) records `submitted run: {request}` as a `Run` card body. The rendered output should include the card title and framed body expected by the repro test.
- Leave the slash command grammar, `WorkflowRuntime::start_run` call, stepwise/workflow-specific run variants, answer/resolve/step/resume dispatch, and workflow event rendering unchanged unless compilation requires a mechanical call-site adjustment.
- Do not log or persist raw user input beyond the existing in-memory transcript behavior exercised by the TUI.

# Tests to be added/updated

- Keep the existing regression test unchanged as the primary acceptance test: `crates/tui/app/src/app/commands.rs::plain_request_submission_renders_initial_input_as_card`.
- Do not rewrite or replace that test; it already asserts the user-visible contract: a `Run` card title, a framed `submitted run: build health route` body, and no bare leading `submitted run:` line.
- Add no source-text or implementation-detail tests. Add a supplemental state-level test only if the new card-capable report-task helper cannot be covered through the existing command-path regression.

# How to verify

1. Confirm the current repro is the red-capable input to the fix:
   `cargo test -p cowboy app::commands::tests::plain_request_submission_renders_initial_input_as_card -- --nocapture`
2. After implementing the fix, run the same command and require it to pass unchanged.
3. Run the focused command-module tests:
   `cargo test -p cowboy app::commands::tests -- --nocapture`
4. Because this is a Rust code change in the TUI app crate, run Clippy for the touched package and fix warnings before yielding:
   `cargo clippy -p cowboy --all-targets -- -D warnings`

# TODO

- [x] Read `docs/plans/initial_user_input_lacks_card_ui/rca.md` and the existing repro test before editing.
- [x] Add or refactor an `AppState` background report-task path that can push a `TranscriptEntry::Card` while preserving current status, run-state, and task-spawn semantics.
- [x] Update only the plain initial request `spawn_start_run` path to use a `Run` card for `submitted run: {request}`.
- [x] Preserve existing non-targeted dispatch behavior for slash commands, pending prompt answers, resolve/step/resume actions, workflow events, and background task draining.
- [x] Keep `plain_request_submission_renders_initial_input_as_card` unchanged and make it pass.
- [x] Run the focused verification commands and fix any Rust compiler or Clippy warnings they surface.
