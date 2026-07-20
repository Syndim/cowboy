# Bug behavior

The TUI currently scrolls an overflowing transcript when the mouse wheel is used over the transcript region. After that support was reintroduced, an unmodified left-button drag no longer selects transcript text: the terminal sends mouse input to Cowboy, but Cowboy neither maintains a text selection nor returns the gesture to the terminal.

The behavior is deterministic. Wheel events reach the transcript scrolling branch, while the first `Down(Left)` event in a selection gesture is ignored. Git history grounds the regression in the region-aware mouse scrolling change, which added terminal mouse capture and `Event::Mouse` dispatch together; commit `26e96cd` reintroduced that change after its earlier revert.

Both behaviors are compatible. Copilot CLI keeps mouse reporting enabled and supplies application-managed text selection and copying. Cowboy currently has the scrolling half of that design but no selection half.

# Root cause

`crates/tui/app/src/app.rs` calls crossterm 0.29.0 `EnableMouseCapture` while entering the alternate screen. Crossterm emits terminal modes `?1000h`, `?1002h`, `?1003h`, `?1015h`, and `?1006h`; these modes report button presses, button-motion/drag events, all motion events, and coordinates to the application. Consequently, the terminal does not perform its normal unmodified click-drag text selection.

The run loop forwards every resulting `Event::Mouse` to `crates/tui/app/src/app/input.rs::handle_mouse_event`. That handler has only two successful branches: `ScrollUp` and `ScrollDown` inside `layout.transcript`. `Down(Left)`, `Drag(Left)`, and `Up(Left)` fall through to `_ => false`. No selection state, selection rendering, selected-text extraction, or copy path exists elsewhere under `crates/tui/app/src`.

The regression is therefore the combination of two individually observable facts: Cowboy requests exclusive mouse reporting from the terminal, then discards the press/drag/release events needed to replace terminal-native selection. Disabling only the wheel handler would not restore native selection because terminal mouse capture would remain active; handling only wheel events cannot provide selection while capture remains active.

# Reproduction steps

1. Start the TUI with `cargo run -p cowboy` in a terminal that supports SGR mouse reporting.
2. Produce enough workflow transcript content to overflow the main transcript viewport.
3. Place the pointer over the transcript and use the mouse wheel. The transcript scroll position changes.
4. Press the left mouse button over transcript text, drag across characters, and release without a terminal override modifier. No text selection appears and there is no selected text to copy.
5. Run the deterministic automated reproduction:

   ```text
   cargo test -p cowboy app::input::tests::transcript_mouse_drag_is_handled_for_text_selection -- --exact
   ```

# Regression test

- Test file: `crates/tui/app/src/app/input.rs`
- Test name: `app::input::tests::transcript_mouse_drag_is_handled_for_text_selection`
- Command: `cargo test -p cowboy app::input::tests::transcript_mouse_drag_is_handled_for_text_selection -- --exact`
- Expected failure before the fix: the first simulated `MouseEventKind::Down(MouseButton::Left)` over the transcript returns `false`, and the test panics with `Down(Left) over transcript was ignored, so captured mouse input cannot select text`.

The test drives the same `handle_mouse_event` entry point used by the production event loop and supplies the complete left-button press/drag/release gesture. It is intentionally limited to the missing event-routing prerequisite; existing focused wheel tests continue to cover transcript scrolling and state isolation.

# Current failing result

The narrow command was run repeatedly and failed deterministically. The current result is:

```text
running 1 test
failures:

---- app::input::tests::transcript_mouse_drag_is_handled_for_text_selection stdout ----

thread 'app::input::tests::transcript_mouse_drag_is_handled_for_text_selection' panicked at crates/tui/app/src/app/input.rs:921:13:
Down(Left) over transcript was ignored, so captured mouse input cannot select text

failures:
    app::input::tests::transcript_mouse_drag_is_handled_for_text_selection

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 268 filtered out

error: test failed, to rerun pass `-p cowboy --lib`
```

The command exits with status 101.

# Fix constraints

- Preserve mouse-wheel scrolling of overflowing transcript content and the existing transcript-only region routing.
- Preserve composer input, input-history position, follow-latest behavior, scroll limits, and non-scrollable-region behavior while scrolling or selecting.
- Support an unmodified left-button press/drag/release selection gesture rather than requiring a terminal-specific override modifier.
- If terminal mouse capture remains enabled, Cowboy must own the complete selection contract: selection state, visible highlighting, selected-text extraction, and copy behavior. Ignoring captured button events is not acceptable.
- If terminal-native selection is restored instead, wheel input must still be distinguishable from keyboard history/navigation and must continue to scroll only the main transcript.
- Keep terminal setup and restoration paired on normal exit and setup failure so no mouse mode leaks into the caller's shell.
- Make the new regression test pass without weakening the existing transcript wheel regression tests.