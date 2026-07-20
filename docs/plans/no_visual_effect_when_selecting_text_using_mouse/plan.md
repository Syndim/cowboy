# Plan

Use the RCA at `docs/plans/no_visual_effect_when_selecting_text_using_mouse/rca.md` as the source of truth. The failing rendering test is `crates/tui/app/src/app/controls/transcript.rs::app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured`; keep that test intact as the repro contract and make it pass.

Keep terminal mouse capture enabled. `crates/tui/app/src/app.rs::enter_terminal_screen` currently enables `EnableMouseCapture`, and `crates/tui/app/src/app/input.rs::handle_mouse_event` depends on captured mouse events for transcript drag selection, scroll-wheel handling, selected-text extraction, and OSC52 copy queuing. Removing mouse capture would require a broader input redesign and would risk losing existing behavior.

Fix the rendering side instead: when `AppState` has a transcript selection, `crates/tui/app/src/app/controls/transcript.rs::render` should render the same viewport rows with a single app-owned selection style applied only to selected text cells. The helper should use the existing normalized selection and row-range logic, split spans at character boundaries, preserve each span's existing style, and add a visible selection modifier such as `Modifier::REVERSED` only for characters whose display columns intersect the selected range.

The final behavior should have exactly one visible selection affordance: Cowboy owns the highlight while mouse capture is active, and the highlight must not spill into card borders, padding outside the selected text, unrelated rows, or scrollback rows outside the current viewport.

# Changes

- Update `crates/tui/app/src/app/controls/transcript.rs` so `render` passes viewport rows through a selection-highlighting helper before constructing the `Paragraph`.
- Add focused helper functions in `transcript.rs` for applying selection styling to `Vec<Line<'static>>` without changing viewport generation, selected-text extraction, scroll-offset calculations, or transcript state.
- Reuse existing selection primitives where possible: `normalize_selection`, `row_selection_range_between`, `line_display_width`, and `char_intersects_range`.
- Preserve existing span foreground/background/style choices by deriving selected spans from the original span style and adding the selection modifier instead of replacing the whole style.
- Leave `crates/tui/app/src/app.rs` mouse capture setup and `crates/tui/app/src/app/input.rs` mouse-event state transitions unchanged unless a narrow compiler issue requires an import-only adjustment.
- Keep the investigator-added repro test `card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured` as an input to the fix; do not rewrite or replace it.
- Update the older dual-highlight regression coverage in `transcript.rs` so it no longer asserts the impossible contract that captured mouse selection has no app-owned highlight. It should instead assert the selected text range is highlighted and neighboring card border/padding/unselected text cells are not highlighted.

# Tests to be added/updated

- Keep `app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured` unchanged and make it pass.
- Rename `app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection` to `app::controls::transcript::tests::card_mouse_selection_highlights_only_selected_text_while_mouse_is_captured` and update it to cover the corrected dual-highlight invariant: with mouse capture active, only the selected text range receives the app-owned highlight.
- Add helper-level coverage in `crates/tui/app/src/app/controls/transcript.rs` for selection across multiple spans with different styles and for empty selections that should not apply selection styling.
- Preserve existing input behavior tests in `crates/tui/app/src/app/input.rs`, especially `transcript_mouse_drag_is_handled_for_text_selection` and `transcript_mouse_selection_updates_finalizes_and_queues_copy`.

# How to verify

Run these commands from the repository root after implementation:

```bash
cargo test -p cowboy app::tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact
cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured -- --exact
cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_highlights_only_selected_text_while_mouse_is_captured -- --exact
cargo test -p cowboy app::input::tests::transcript_mouse_drag_is_handled_for_text_selection -- --exact
cargo test -p cowboy app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy -- --exact
```

Observable pass criteria:

- Mouse capture remains paired with terminal restoration.
- The RCA repro test passes because selected transcript text has a visible app-owned highlight.
- The corrected dual-highlight regression test passes because the highlight is constrained to the selected text cells only.
- Mouse drag selection still updates state, finalizes selection on mouse-up, and queues the selected text for clipboard copying.

# TODO

- [x] TODO-01: Add transcript row highlighting for captured mouse selections.
  - Procedure: In `crates/tui/app/src/app/controls/transcript.rs`, route `render` through a helper that applies selection styling to the current viewport rows when `state.transcript_selection()` is present, then run `cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured -- --exact`.
  - Expected result: The command exits successfully, and the selected substring cells include `Modifier::REVERSED` while the test body remains the RCA-provided repro contract.
  - Implementation observed result: `render` now applies selection styling to current viewport rows through `apply_selection_highlight` when `state.transcript_selection()` is present. `cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured -- --exact` exited successfully, and the RCA repro test body was left unchanged.

- [x] TODO-02: Constrain the selection highlight to the selected text range only.
  - Procedure: Implement span splitting so selected characters inherit original span styling plus the selection modifier, unselected characters retain their original style, and no border/padding cells are modified; verify with a transcript-rendering test named `card_mouse_selection_highlights_only_selected_text_while_mouse_is_captured` by running `cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_highlights_only_selected_text_while_mouse_is_captured -- --exact`.
  - Expected result: The command exits successfully, selected text cells are highlighted, and neighboring card border, padding, and unselected text cells do not contain the selection modifier.
  - Implementation observed result: `line_with_selection_highlight` splits spans at selected-character boundaries, applies `Modifier::REVERSED` on top of each original selected span style, and leaves unselected fragments with their original style. The first focused test run exposed a test cell-indexing issue in the renamed regression coverage; after correcting that assertion to convert the rendered string byte offset to display-cell width, `cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_highlights_only_selected_text_while_mouse_is_captured -- --exact` exited successfully. Post-feedback verification inspected the corrected render test and reran the same focused command; it exited successfully and proves only selected transcript text cells receive the app-owned `Modifier::REVERSED`, with no highlight spill into card border, leading padding, unselected text, trailing padding, or border.

- [x] TODO-03: Preserve mouse capture and existing mouse-selection state behavior.
  - Procedure: Leave `EnableMouseCapture`/`DisableMouseCapture` pairing in `crates/tui/app/src/app.rs` and mouse-event handling in `crates/tui/app/src/app/input.rs` behaviorally unchanged, then run `cargo test -p cowboy app::tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact`, `cargo test -p cowboy app::input::tests::transcript_mouse_drag_is_handled_for_text_selection -- --exact`, and `cargo test -p cowboy app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy -- --exact`.
  - Expected result: All three commands exit successfully; terminal mouse capture remains paired, transcript drag events are handled, and mouse-up still queues the selected text for clipboard copying.
  - Implementation observed result: `crates/tui/app/src/app.rs` and `crates/tui/app/src/app/input.rs` were not changed. `cargo test -p cowboy app::tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact`, `cargo test -p cowboy app::input::tests::transcript_mouse_drag_is_handled_for_text_selection -- --exact`, and `cargo test -p cowboy app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy -- --exact` each exited successfully. Post-feedback verification inspected `enter_terminal_screen`/`restore_terminal_screen` and confirmed `EnableMouseCapture`/`DisableMouseCapture` remain paired; inspected `handle_mouse_event` and confirmed captured transcript left-button down, drag, and up still start, update, and finalize app-owned selection. Rerunning `cargo test -p cowboy app::tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact` exited successfully, confirming terminal-native selection is not the visual owner during the captured transcript drag path.

- [x] TODO-04: Add helper-level coverage for styled spans and empty selections.
  - Procedure: Add focused tests named `selection_highlight_preserves_existing_span_styles` and `selection_highlight_skips_empty_selection` in `crates/tui/app/src/app/controls/transcript.rs`, then run `cargo test -p cowboy app::controls::transcript::tests::selection_highlight_preserves_existing_span_styles -- --exact` and `cargo test -p cowboy app::controls::transcript::tests::selection_highlight_skips_empty_selection -- --exact`.
  - Expected result: Both commands exit successfully; selected span fragments preserve their original styles plus the selection modifier, and empty selections leave every rendered cell without the selection modifier.
  - Implementation observed result: Added `selection_highlight_preserves_existing_span_styles` and `selection_highlight_skips_empty_selection` in `crates/tui/app/src/app/controls/transcript.rs`. `cargo test -p cowboy app::controls::transcript::tests::selection_highlight_preserves_existing_span_styles -- --exact` and `cargo test -p cowboy app::controls::transcript::tests::selection_highlight_skips_empty_selection -- --exact` each exited successfully.
