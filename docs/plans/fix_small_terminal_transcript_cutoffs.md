# Plan

Fix the small-terminal cutoff by making transcript clipping wrap-aware. The current transcript renderer chooses the last `N` logical `Line`s from the event log, then asks Ratatui to soft-wrap them inside the bordered transcript panel. In narrow terminals, one logical line can consume several screen rows, so the selected tail can exceed the panel height and Ratatui clips the bottom of the latest response. That makes the final wrapped rows unreachable even though the status and input composer remain visible.

Render the transcript from pre-wrapped visual rows instead: compute the transcript inner width, preserve each span's style while splitting long logical lines into screen-width rows, then apply the follow/scroll window to those visual rows. Keep footer/status/composer allocation deterministic in very short terminals so the input box remains complete and transcript space shrinks first.

# Changes

- Update `crates/tui/src/app/controls/transcript.rs` so `render` calculates the transcript inner width from the bordered area and passes both visible height and inner width into the line-selection path.
- Replace logical-line clipping in `transcript::lines` with visual-row clipping:
  - render all transcript entries and pending-prompt lines as today;
  - split each `Line` into width-bounded visual `Line`s before clipping;
  - preserve span styles for thoughts, metadata, normal text, warnings, and code-highlight spans;
  - keep blank logical lines as one blank visual row;
  - apply `state.scroll_offset()` after wrapping so `0` means the latest visible terminal row is shown.
- Remove reliance on `Paragraph::wrap` for transcript body rows after pre-wrapping, avoiding double wrapping and making clipping deterministic.
- Update scroll bounds so `scroll_offset` is not capped by the old logical `transcript_line_count()` when narrow wrapping creates more visual rows than logical rows. Clamp only at render time against the visual row count, or replace the state cap with a width-independent safe cap.
- Keep the existing TUI crate boundary: no workflow/runtime changes, no event model changes, and no layout logic moved into workflow crates.
- Add a small-height draw guard if needed so header/status/composer rows are allocated first and the transcript panel shrinks before the input composer loses its border.

# Tests to be added/updated

- Add transcript unit coverage in `crates/tui/src/app/controls/transcript.rs` for a long final agent response rendered with a narrow inner width and a small visible height; assert the final sentinel text appears in the returned visible rows.
- Add transcript unit coverage that styled spans survive wrapping by checking thought/metadata styles on wrapped visual rows.
- Add scroll coverage for narrow wrapped content: after scrolling up, assert older wrapped visual rows become visible; after scrolling down/follow latest, assert the latest wrapped tail returns.
- Add or update a draw-level test in `crates/tui/src/app/tests.rs` that renders a narrow, short terminal with a long response plus pending input and asserts:
  - the latest response tail is visible in the transcript panel;
  - the status line is present;
  - the input composer top and bottom borders are present.
- Keep existing composer wrapping, cursor, slash suggestion, and transcript color tests passing.

# How to verify

- Run `cargo test -p cowboy app::controls::transcript` to verify wrap-aware transcript clipping and style preservation.
- Run `cargo test -p cowboy app::tests` to verify full-frame layout behavior in narrow/short terminal buffers.
- Run `cargo test -p cowboy app::state` if scroll-state tests are changed or `transcript_line_count()` is removed/rewired.
- Manually smoke-test with `cargo run -p cowboy` in a narrow terminal using a long agent response or pasted transcript-like text; shrink the terminal height and width, then confirm the newest wrapped response rows, waiting-input status, and input composer remain visible.

# TODO

- [x] Thread transcript inner width from `transcript::render` into the transcript line-selection helper.
- [x] Add a style-preserving visual-row wrapping helper for `ratatui::text::Line` values.
- [x] Change transcript clipping to apply follow/scroll offsets after visual wrapping.
- [x] Stop capping scroll offsets with the old logical transcript line count, or replace that cap with a render-time visual-row clamp.
- [x] Remove `Paragraph::wrap` from transcript rendering once rows are pre-wrapped.
- [x] Add transcript unit tests for narrow-width latest-tail visibility.
- [x] Add transcript unit tests for wrapped-span style preservation.
- [x] Add scroll tests for wrapped visual rows.
- [x] Add a draw-level narrow/short terminal regression test covering transcript tail, status line, and composer borders.
- [x] Run targeted TUI tests and record the results in the implementation response.
- [x] Manually smoke-test the small-terminal scenario before marking the bug fixed.
