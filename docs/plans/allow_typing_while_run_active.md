# Plan

Change the TUI active-run input model from "composer disabled" to "composer editable but not submittable". Today `AppState::composer_enabled()` in `crates/tui/src/app/state.rs` is used as one combined gate for editing, paste, cursor placement, slash suggestions, rendering copy, and `Enter` submission. That is why `crates/tui/src/app/input.rs` ignores all non-global keys while a background run task is active, `crates/tui/src/app.rs` ignores paste, and `crates/tui/src/app/controls/composer.rs` hides the cursor and shows `Input disabled while run active`.

Split that behavior into two explicit concepts: the composer accepts draft editing while a run is active, but plain `Enter` may submit only when there is no active background task or when the run is waiting for an input prompt. Update the composer header/title and status copy to say that draft typing is allowed and `Enter` is blocked until the active run blocks, finishes, or is cancelled.

`WaitingForInput` must remain the prompt-answer exception: when `pending_prompt()` is present, typing and `Enter` should still answer that prompt through the existing `commands::dispatch_submitted_input` path.

# Changes

- In `crates/tui/src/app/state.rs`, replace the single-purpose mental model around `composer_enabled()` with explicit queries, for example:
  - `composer_accepts_edits()` or equivalent: true for normal idle input, active background tasks, and pending prompts.
  - `composer_accepts_submit()` or equivalent: true when `pending_prompt().is_some()` or `background_task_count() == 0`; false while a run task is active and no prompt is pending.
  - Keep or rename `composer_enabled()` only if its meaning is unambiguous at every callsite; do not leave a method named "enabled" that means both editable and submittable.
- In `crates/tui/src/app/input.rs`, use the submit gate only for plain `Enter`:
  - While a run is active and no prompt is pending, plain `Enter` returns `KeyHandling::Continue` without calling `commands::submit_input`, without clearing the draft, and without adding to input history.
  - Printable characters, cursor movement, `Backspace`, `Delete`, modified `Enter`, and `Ctrl+J` continue to edit the current draft while the run is active.
  - Keep global controls working during active runs: `Esc` cancels background tasks, `Ctrl+C` exits, `Ctrl+U`/`Ctrl+D` scroll, and `End` follows latest events.
  - Keep `Tab` slash completion and `Up`/`Down` history inert while the submit gate is closed, unless the implementation deliberately updates the active-run hint and tests to make those affordances non-misleading.
- In `crates/tui/src/app.rs`, allow `Event::Paste(text)` to append to the draft while a run is active, because paste is an edit operation, not a submit operation.
- In `crates/tui/src/app/controls/composer.rs`, update active-run rendering from disabled to draft-only:
  - Replace the active-run title `Run active ─ Esc cancels` with copy such as `Run active ─ type draft, Enter waits ─ Esc cancels`.
  - Remove or replace `DISABLED_NOTICE`; the composer should not claim input is disabled.
  - Place the cursor in the composer while the run is active so the input box visibly remains editable.
  - Do not reserve an extra disabled-notice row in active-run height calculations unless a new draft-only notice line is intentionally rendered.
  - Suppress slash suggestions while submit is blocked if `Tab` completion remains inert.
- In `crates/tui/src/app/controls/status.rs`, replace `input disabled while run active` with draft-only copy, for example `draft allowed ─ Enter waits for active run ─ Esc cancel`.
- In render-level assertions under `crates/tui/src/app/tests.rs`, update expectations that currently require disabled copy so they instead require draft-only copy and editable input visibility.
- Preserve prompt-answer behavior in `crates/tui/src/app/commands.rs`: `pending_prompt_answer_target()` should still route normal text to `spawn_answer_task`, and answer submission should temporarily close the submit gate again once the answer task starts running.

# Tests to be added/updated

- Update `crates/tui/src/app/input.rs` tests for the active-run/no-prompt state:
  - printable characters insert into the draft;
  - paste-covered edit behavior is handled by the app loop or extracted helper;
  - `Backspace`, `Delete`, and left/right word/character movement work on the draft;
  - modified `Enter` and `Ctrl+J` insert newlines;
  - plain `Enter` does not return `KeyHandling::Submit`, does not clear the draft, and does not add history;
  - `Esc`, `Ctrl+C`, `Ctrl+U`/`Ctrl+D`, and `End` still keep their existing global behavior;
  - `Tab` completion and `Up`/`Down` history remain inert while submit is blocked if that is the chosen behavior.
- Update or replace `crates/tui/src/app/state.rs` coverage so the new edit/submit gates are tested independently:
  - idle state allows edits and submit;
  - active background task allows edits but blocks submit;
  - `WorkflowEventKind::WaitingForInput` allows edits and submit for prompt answers;
  - completed, failed, drained, or cancelled background tasks allow submit again once no background task remains.
- Update `crates/tui/src/app/controls/composer.rs` tests:
  - active-run title says draft typing is allowed and `Enter` is blocked/waits;
  - disabled-input copy is absent;
  - active-run height no longer reserves the old disabled notice row unless replaced by an intentional notice;
  - slash suggestions are hidden while submit is blocked if `Tab` remains inert;
  - cursor placement is enabled while active-run draft editing is allowed.
- Add or update `crates/tui/src/app/controls/status.rs` tests for the active-run status line copy.
- Update `crates/tui/src/app/tests.rs` render tests that currently assert `input disabled while run active` or `Input disabled while run active. Press Esc to cancel.` so they assert the new draft-only hint and visible typed draft.

# How to verify

- Run `cargo test -p cowboy app::input::tests`.
- Run `cargo test -p cowboy app::state::tests`.
- Run `cargo test -p cowboy app::controls::composer::tests`.
- Run `cargo test -p cowboy app::controls::status::tests` if status tests are added.
- Run `cargo test -p cowboy app::tests` for full TUI render assertions.
- Manual TUI smoke test:
  - launch `cargo run -p cowboy`;
  - submit a workflow request that keeps a run active long enough to interact;
  - while the run is active, type text, paste text, move the cursor, delete characters, and insert a newline; confirm the draft changes and the cursor is visible;
  - press plain `Enter` while the run is active and confirm no second run starts, the draft remains in the composer, and no history entry is written;
  - confirm the composer/status hint says draft typing is allowed but `Enter` is unavailable until the active run blocks or finishes;
  - press `Esc` during an active run and confirm cancellation still works;
  - start or use a workflow that reaches `WaitingForInput`; confirm `Enter` still submits the prompt answer;
  - after the active run completes or is cancelled, press `Enter` with the saved draft and confirm it starts normally.

Manual smoke evidence (2026-07-07 follow-up): ran `cargo run -p cowboy -- --config <temp-config>` through a real pseudo-terminal. Verified an active run accepted typing, bracketed paste, cursor left/right movement, Backspace deletion/replacement, and Ctrl-J newline input; the composer/status hint said draft typing was allowed and Enter waits; plain Enter did not submit while active and the draft remained; Esc cancelled the active task; the saved draft submitted after cancellation; a WaitingForInput workflow accepted an Enter-submitted answer and completed.

Paste follow-up evidence (2026-07-07): removed the `handle_paste` helper. Paste events now directly append text with `state.push_input(&text)` and mark the draw scheduler dirty, so paste is not gated by submit availability. Verified with `cargo test -p cowboy app::tests::paste_appends_to_active_run_draft_input` and `cargo test -p cowboy app::input::tests::active_run_allows_draft_edits_but_plain_enter_does_not_submit`; both passed.

# TODO

- [x] Add separate AppState queries for edit acceptance and submit acceptance.
- [x] Update active-run key handling so edits work but plain Enter cannot submit.
- [x] Allow paste to edit the draft while a run is active.
- [x] Keep active-run global controls unchanged.
- [x] Preserve WaitingForInput answer submission as the submit-gate exception.
- [x] Update composer title/header copy for draft-only active-run input.
- [x] Remove or replace disabled-input composer notice and height reservation.
- [x] Render the input cursor while active-run draft editing is allowed.
- [x] Update active-run status copy from disabled input to draft-only input.
- [x] Decide and test whether slash completion and history remain inert while submit is blocked.
- [x] Update input-handler unit tests for active-run draft editing and Enter blocking.
- [x] Update AppState gate transition tests.
- [x] Update composer/status/render tests for the new active-run hint and visible draft.
- [x] Run focused TUI tests and manual smoke verification.
