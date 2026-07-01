# Plan

Fix the TUI composer so a long typed input line that visually wraps gets more vertical space and keeps the cursor on the visible wrapped row. The current composer height is based on `AppState::input_line_count()`, which only counts explicit `\n` separators. A single long logical line can therefore need two or more terminal rows while the layout still reserves the minimum three-row composer, hiding the wrapped continuation inside the bordered input area.

Use the composer area's available width when calculating the composer height and when rendering/cursoring input. Keep the existing explicit-newline behavior, slash-suggestion behavior, and 12-row composer cap.

# Changes

- Update `crates/tui/src/app.rs` so `draw` passes the terminal/composer width into `composer::height` before building layout constraints.
- Update `crates/tui/src/app/controls/composer.rs` so height calculation counts visual rows, not only explicit input lines:
  - derive inner input width from the composer width minus borders and the `> ` prompt prefix;
  - count wrapped rows for each logical input line, with empty lines still counting as one row;
  - include slash-suggestion rows in the requested height;
  - preserve the current clamp/minimum behavior and terminal-height guard.
- Update composer rendering to make its displayed lines match the same wrap model used by height calculation:
  - either pre-wrap input lines into visual rows before building `Line`s, or enable ratatui wrapping and compute cursor/visibility from the same width-aware model;
  - keep the `> ` prompt prefix and earlier-line hidden marker semantics;
  - ensure long input beyond the 12-row cap shows the latest visual rows rather than clipping the cursor row.
- Update `set_input_cursor` so the cursor moves to the correct visual row and column after soft wrapping instead of clamping only the x coordinate at the right border.
- Avoid moving wrapping logic into `AppState`; this is terminal-layout state and belongs in the TUI composer.

# Tests to be added/updated

- Add a composer unit test in `crates/tui/src/app/controls/composer.rs` for a long single logical line whose display width exceeds the available input width, asserting that the height calculation requests an additional visible row.
- Add or update a draw-level test in `crates/tui/src/app/tests.rs` that renders a narrow terminal with long input and asserts the wrapped second row is visible inside the composer.
- Add a cursor-position test for wrapped input asserting the cursor y-position advances to the wrapped row and x-position is the wrapped-column position, not the terminal right edge.
- Keep existing tests for explicit newlines, slash suggestions, short terminals, and oversized pasted input passing.

# How to verify

- Run `cargo test -p cowboy app::controls::composer` to verify the composer wrapping/unit behavior.
- Run `cargo test -p cowboy app::tests` to verify full TUI layout rendering and cursor placement.
- Manually smoke-test with `cargo run`, type a long request in a narrow terminal until it wraps, and confirm the input box grows and the wrapped second line remains visible.

## Verification record

- `cargo fmt -p cowboy --check`: passed.
- `cargo test -p cowboy app::controls::composer`: 7 passed.
- `cargo test -p cowboy app::tests`: 6 passed.
- `cargo test -p cowboy`: 40 passed.
- `RUSTFLAGS="-D warnings" cargo test -p cowboy`: 40 passed.
- Manual TUI smoke via `cargo run -p cowboy` in a narrow 16x10 PTY: typed `abcdefghijklmnop`; final composer rows showed `│> abcdefghijkl│` then `│  mnop        │`, confirming the input box grew and the wrapped second row was visible.

## Reviewer follow-up

- Ratatui provides `Paragraph::wrap(Wrap { trim: false })` and, behind the `unstable-rendered-line-info` feature, `Paragraph::line_count(width)` for calculating wrapped text height.
- Ratatui does not automatically grow the surrounding `Layout` constraint for an input box; Cowboy still has to compute the composer height before splitting the frame.
- Updated the height calculation to use Ratatui's wrapped line-count API. Kept Cowboy's render/cursor wrapping code for application-specific prompt prefix, continuation indentation, latest-lines clipping, and cursor placement.

# TODO

- [x] Add width-aware visual row calculation to `crates/tui/src/app/controls/composer.rs`.
- [x] Pass composer width from `crates/tui/src/app.rs` into `composer::height`.
- [x] Render long logical input lines using the same wrapping model as height calculation.
- [x] Update cursor placement to use wrapped visual row and column.
- [x] Preserve slash suggestions and explicit newline rendering with the new height model.
- [x] Add composer unit coverage for soft-wrapped input height.
- [x] Add draw-level coverage for visible wrapped second-line text.
- [x] Add cursor-position coverage for wrapped input.
- [x] Run the targeted TUI tests and record the commands/results.
- [x] Manually smoke-test long input in the TUI.
- [x] Investigate Ratatui auto-sizing support and use `Paragraph::line_count` for composer height.
