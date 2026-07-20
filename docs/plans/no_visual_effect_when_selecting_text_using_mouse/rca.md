# Bug behavior

Selecting transcript text with the mouse produces no visible selection effect in the Cowboy TUI.

The repository-grounded reproduction combines two current behaviors: terminal setup enables crossterm mouse capture (`?1000h`), and transcript rendering deliberately leaves active selected cells unstyled. With mouse capture enabled, the terminal sends mouse events to Cowboy instead of relying on ordinary terminal-native text selection feedback. Because Cowboy also no longer paints selected transcript cells, the drag updates selection state and clipboard text without any visible highlight.

# Root cause

The previous dual-highlight fix removed Cowboy's app-owned transcript selection highlight from `crates/tui/app/src/app/controls/transcript.rs::render` by rendering `viewport.rows` directly. That made `crates/tui/app/src/app/controls/transcript.rs::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection` pass.

However, `crates/tui/app/src/app.rs::enter_terminal_screen` still enables mouse capture with `EnableMouseCapture`, and `crates/tui/app/src/app/input.rs::handle_mouse_event` still consumes left-button down, drag, and up events inside the transcript to update `AppState.transcript_selection` and queue OSC52 clipboard text. The code therefore captures mouse selection events but provides no app-rendered visual selection state.

The regression is the interaction between those two facts: the dual-highlight fix assumed terminal-native selection would be the visible selection owner, but the active terminal mode still routes selection gestures through Cowboy.

# Reproduction steps

1. Confirm the TUI terminal setup enables mouse capture:

   ```bash
   cargo test -p cowboy app::tests::terminal_screen_commands_pair_mouse_capture_and_bracketed_paste -- --exact
   ```

   Observed result: the test passes and asserts the terminal command stream contains `?1000h` and `?1000l`.

2. Confirm the previous dual-highlight regression test now requires selected card cells to have no app-owned reversed-video style:

   ```bash
   cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection -- --exact
   ```

   Observed result: the test passes.

3. Run the new focused regression test that models a captured mouse drag over transcript text and asserts the selected cells have visible app-owned selection styling while mouse capture remains enabled:

   ```bash
   cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured -- --exact
   ```

   Observed result: the test fails because the selected cells have no reversed-video modifier.

# Regression test

- Test file path: `crates/tui/app/src/app/controls/transcript.rs`
- Test name: `app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured`
- Command: `cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured -- --exact`
- Expected failure before the fix: the test fails because Cowboy captures transcript mouse selection but renders the selected text cells without a visible reversed-video highlight.

# Current failing result

```text
running 1 test
failures:

---- app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured stdout ----

thread 'app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured' (1118317) panicked at crates/tui/app/src/app/controls/transcript.rs:672:9:
mouse selection is captured by Cowboy, so selected transcript text needs a visible app-owned highlight; rendered selected cells had no reversed-video modifier: "│selectable transcript text                                                    │"
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::controls::transcript::tests::card_mouse_selection_has_visible_app_highlight_while_mouse_is_captured

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 293 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

# Fix constraints

- Preserve a visible selection affordance when a user drags over transcript text in the TUI.
- Avoid reintroducing the original dual-highlight bug: the final behavior must have exactly one visible selection highlight for a transcript selection gesture.
- If Cowboy keeps `EnableMouseCapture`, render selected transcript cells with a single app-owned visual style and update or replace the prior dual-highlight regression test accordingly.
- If Cowboy stops capturing mouse selection to rely on terminal-native selection feedback, preserve required mouse-driven transcript scrolling and clipboard behavior through another tested path.
- Preserve transcript selected-text extraction, OSC52 clipboard queuing on mouse-up, card wrapping, scroll-offset behavior, and existing non-selection card styles.
- Product code is intentionally unchanged during this investigation; only the regression test and this RCA were added.
