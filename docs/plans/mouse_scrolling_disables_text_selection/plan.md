# Plan

Base the fix on `docs/plans/mouse_scrolling_disables_text_selection/rca.md` and keep the investigator-added repro test `crates/tui/app/src/app/input.rs::transcript_mouse_drag_is_handled_for_text_selection` unchanged. The implementation should keep terminal mouse capture enabled for transcript wheel scrolling and add the missing application-owned transcript text selection path for captured left-button mouse gestures.

Use the current TUI architecture boundaries: keep mouse event routing in `crates/tui/app/src/app/input.rs`, persistent UI state in `crates/tui/app/src/app/state.rs`, transcript viewport/hit-testing/rendering logic in `crates/tui/app/src/app/controls/transcript.rs`, and terminal escape output in `crates/tui/app/src/app.rs` if selected text must be copied through OSC 52. Do not move runtime or workflow behavior into the TUI input layer.

# Changes

1. Add transcript selection state to `AppState`.
   - Track selection anchor, focus, active/finalized state, and the selected plain text.
   - Store selection coordinates in transcript viewport-relative row/display-column units so the input handler does not need to understand card or workflow rendering internals.
   - Clear or recompute selection when transcript content changes, when scrolling changes the viewport, or when the user starts a new transcript selection.

2. Add transcript viewport hit-testing and extraction helpers in `app/controls/transcript.rs`.
   - Reuse the existing `content_viewport`/wrapping path so hit-testing, highlighting, selected text extraction, and rendering all use the same visual rows and scrollbar-adjusted content area.
   - Convert mouse coordinates to clamped transcript selection points inside the visible transcript content area, excluding the scrollbar column.
   - Extract selected plain text from normalized start/end points, preserving row breaks and handling Unicode display width without byte slicing.

3. Handle captured left-button transcript gestures in `handle_mouse_event`.
   - `Down(Left)` inside the transcript content area starts selection and returns `true`.
   - `Drag(Left)` updates selection while active and returns `true`.
   - `Up(Left)` finalizes selection, stores the selected text if non-empty, and returns `true`.
   - Left-button events outside the transcript should not mutate composer input or history; they may clear an existing transcript selection if that matches the final UI behavior.
   - Existing `ScrollUp`/`ScrollDown` handling must continue to scroll only the transcript and leave composer/history state unchanged.

4. Render visible transcript selections.
   - Apply a clear selection style, preferably reverse video or a dedicated style in `app/styles.rs`, by splitting existing `Line`/`Span` values without losing their original foreground styles outside the selected range.
   - Selection highlighting must work across single rows, wrapped rows, and multiple visible rows.
   - Keep existing transcript scrollbar behavior and rightmost scrollbar reservation unchanged.

5. Provide a copy path for application-owned selection.
   - If terminal mouse capture remains enabled, implement selected-text copying from the app-owned selection rather than relying on terminal-native selection.
   - Prefer an OSC 52 clipboard write queued by state and emitted from `app.rs` after a non-empty selection finalizes, because it avoids adding workflow/runtime dependencies and keeps terminal I/O in the app shell.
   - Keep terminal setup/restoration pairing unchanged unless the implementation intentionally replaces `EnableMouseCapture` with narrower explicit mouse modes; if terminal modes change, update the terminal command tests accordingly.

# Tests to be added/updated

- Keep `crates/tui/app/src/app/input.rs::transcript_mouse_drag_is_handled_for_text_selection` unchanged and make it pass by implementing product behavior.
- Add input tests in `crates/tui/app/src/app/input.rs` for selection start/update/finalize over the transcript, non-transcript left-button events not editing composer state, and wheel scrolling still affecting only transcript scroll state.
- Add transcript tests in `crates/tui/app/src/app/controls/transcript.rs` for selected text extraction and highlighting across one row, wrapped rows, multiple rows, and Unicode display-width edge cases.
- Add an app or helper test for OSC 52 clipboard emission if the implementation copies finalized selections through terminal escape output.
- Keep existing terminal setup/rollback tests in `crates/tui/app/src/app/tests.rs` passing; update only if the terminal mouse-mode strategy changes deliberately.

# How to verify

Run the narrow repro first, then the focused TUI checks:

```text
cargo test -p cowboy app::input::tests::transcript_mouse_drag_is_handled_for_text_selection -- --exact
cargo test -p cowboy app::input::tests
cargo test -p cowboy app::controls::transcript::tests
cargo test -p cowboy app::tests::terminal_screen
cargo fmt -p cowboy --check
cargo clippy -p cowboy --lib --tests --no-deps -- -D warnings
```

Manual smoke test after automated checks:

1. Start the TUI with `cargo run -p cowboy`.
2. Produce enough transcript output to overflow the main content area.
3. Use the mouse wheel over the transcript and confirm it scrolls up and down like Copilot CLI, without changing composer input or history.
4. Press, drag, and release the left mouse button over transcript text without a modifier and confirm the selected range is visibly highlighted.
5. Confirm the selected transcript text can be copied through the implemented app-owned copy path.
6. Exit the TUI and confirm normal shell mouse behavior is restored.

# TODO

- [x] Add transcript selection state and accessors to `AppState`.
- [x] Add transcript viewport hit-testing helpers based on existing wrapped visual rows.
- [x] Add selected text extraction from transcript visual rows.
- [x] Route transcript `Down(Left)`, `Drag(Left)`, and `Up(Left)` events in `handle_mouse_event`.
- [x] Render selected transcript ranges without disrupting existing span styles or scrollbar layout.
- [x] Add the app-owned selected-text copy path required while mouse capture is enabled.
- [x] Add or update focused input, transcript rendering, and copy-path tests.
- [x] Run the narrow repro, focused TUI tests, formatter check, and Clippy check listed above.
