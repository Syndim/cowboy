## Plan

Use the classic Unix-style block cursor for the TUI input composer by changing the terminal cursor shape set when the TUI enters alternate-screen mode. Keep cursor placement, composer wrapping, and terminal restore behavior unchanged.

## Changes

- Treat "Unix cursor style" as a block cursor, replacing the current blinking bar cursor.
- Update `TerminalModeGuard::enter` in `crates/tui/src/app.rs` to use `SetCursorStyle::BlinkingBlock` instead of `SetCursorStyle::BlinkingBar`.
- Keep `TerminalModeGuard::restore` setting `SetCursorStyle::DefaultUserShape` so the user's terminal cursor preference is restored on exit.
- Add a small named helper or constant for the configured TUI input cursor style so the chosen cursor shape is explicit and testable without opening a real terminal.
- Leave `crates/tui/src/app/controls/composer.rs` cursor position logic unchanged; the requested change is cursor shape, not cursor location or input wrapping.

## Tests to be added/updated

- Add a focused unit test in the `cowboy` crate that asserts the configured TUI input cursor style is `SetCursorStyle::BlinkingBlock`.
- Keep the existing composer cursor-position tests (`draw_places_cursor_at_input_end` and `draw_places_cursor_at_wrapped_input_end`) passing to prove the style change did not move the cursor.

## How to verify

- Run `cargo test -p cowboy cursor` to cover the new cursor-style assertion and existing cursor-position tests.
- Optionally run `cargo run -p cowboy` and confirm the input box cursor appears as a block cursor, then exit and confirm the terminal cursor returns to the user's default shape.

## TODO

- [x] Add a testable helper or constant for the TUI input cursor style in `crates/tui/src/app.rs`.
- [x] Replace `SetCursorStyle::BlinkingBar` with the helper or constant returning `SetCursorStyle::BlinkingBlock`.
- [x] Add a unit test asserting the configured TUI input cursor style is `SetCursorStyle::BlinkingBlock`.
- [x] Run `cargo test -p cowboy cursor`.
- [ ] Manually smoke-test `cargo run -p cowboy` if terminal cursor appearance needs visual confirmation.
