# Plan

Use the TUI composer state that already drives input behavior to choose the input box color at render time. The color applies to the composer `Block` border, and the title should use the same style if Ratatui does not inherit the border style into the title. Keep input behavior unchanged: normal idle composer submits as today, an active background run without a pending prompt accepts draft edits but disables submit, and `WaitingForInput` enables prompt answers.

State precedence:

1. `WaitingForInput`: `state.pending_prompt().is_some()` wins and uses the same orange warning style as the status bar (`style_warning()` / `style_for_run_state("waiting")`).
2. Submit disabled: no pending prompt and `!state.composer_accepts_submit()` uses gray (`style_muted()` / transcript metadata gray).
3. Initial/enabled: all other composer states keep the current blue accent (`style_border_accent()` / `style_accent()`).

# Changes

- Update `crates/tui/app/src/app/controls/composer.rs` to introduce a small local composer visual-state helper, for example `ComposerVisualState::{Initial, SubmitDisabled, WaitingForInput}`, derived only from `AppState::pending_prompt()` and `AppState::composer_accepts_submit()`.
- Add a composer style helper that maps the visual state to existing palette functions: initial to blue accent, submit-disabled to muted gray, waiting-for-input to warning orange.
- Use that helper in `composer::render` for `Block::border_style(...)`; style the composer title with the same state color if the current Ratatui title rendering otherwise stays uncolored.
- Leave `composer::title`, `height`, wrapping, cursor placement, slash suggestions, and input handling semantics unchanged except where tests need to inspect the title as a styled span.
- Do not change workflow runtime state, prompt-answer routing, background task lifecycle, or status/header/transcript styling.

# Tests to be added/updated

- Add focused composer tests in `crates/tui/app/src/app/controls/composer.rs` for the visual-state helper or style helper:
  - idle/enabled composer resolves to the current blue accent color;
  - active background task with no pending prompt resolves to the muted gray color;
  - pending prompt resolves to the warning orange color.
- Add or update a full render test in `crates/tui/app/src/app/tests.rs` that draws the TUI with a `TestBackend` and asserts the composer border cell foreground color changes for the three states.
- Reuse existing test setup patterns: pending background task via `state.spawn_report_task(...)`, waiting prompt via applying `WorkflowEventKind::WaitingForInput`, and buffer cell inspection like `draw_preserves_transcript_styles`.
- Keep existing input behavior tests unchanged unless they need imports for color assertions; this feature is visual only.

# How to verify

- Run `cargo test -p cowboy app::controls::composer::tests`.
- Run the focused full-render color test added in `crates/tui/app/src/app/tests.rs` with `cargo test -p cowboy <test_name>`.
- Run `cargo test -p cowboy app::tests::draw_places_cursor_in_active_run_draft_input app::input::tests` to confirm submit-disabled visual changes did not regress active-run draft editing, cursor placement, cancellation, or key handling.
- Optional manual smoke: launch `cargo run -p cowboy`, observe the composer border/title blue when idle, gray while a run is active and submit is disabled, and orange when a workflow is waiting for an answer.

# TODO

- [x] Add a local composer visual-state helper in `crates/tui/app/src/app/controls/composer.rs`.
- [x] Map composer visual states to existing blue, gray, and orange style functions.
- [x] Apply the state style to the composer border and title rendering.
- [x] Preserve existing composer behavior for submit gating, draft editing, prompt answers, slash suggestions, wrapping, and cursor placement.
- [x] Add focused composer state/style tests.
- [x] Add full TUI render coverage for the three composer colors.
- [x] Run the focused composer tests.
- [x] Run the focused TUI render/color test.
- [x] Run focused input behavior tests to guard against visual changes affecting input semantics.
