# Bug behavior

A user-supplied screenshot shows text being mouse-selected inside a Cowboy card while a second bright selection/highlight region is visible elsewhere on the same terminal row. The visible symptom is two selection highlights for one card text selection.

The repository reproduction isolates the app-owned half of the double-highlight: after a transcript card mouse selection is active, Cowboy renders the selected card text with reversed-video styling. In terminals that also display native mouse-selection feedback, that app-owned reversed-video range appears in addition to the terminal selection highlight.

# Root cause

`crates/tui/app/src/app/input.rs` treats left-button down/drag/up events inside the transcript as application-owned transcript selection. On drag it updates `AppState.transcript_selection` and computes selected text for clipboard copy.

`crates/tui/app/src/app/controls/transcript.rs` then renders every active or finalized `transcript_selection` through `selected_rows`. The selected row is split by `line_with_selection`, and `push_split_span` applies `style_transcript_selection`, which adds `Modifier::REVERSED` to the selected card text. This creates a Cowboy-rendered selection highlight inside the card instead of leaving the terminal as the only visual selection owner.

The user-supplied screenshot grounds the duplicate visual behavior, and the focused regression test grounds the repository behavior by showing that selecting card text causes the rendered terminal buffer cells for that selected text to carry `Modifier::REVERSED`.

# Reproduction steps

1. Render a transcript card containing selectable text.
2. Start and update a transcript selection over text inside that card, matching the state created by mouse down/drag handling.
3. Render the transcript into a terminal buffer.
4. Inspect the cells covering the selected card text.
5. Observe that Cowboy marks those cells with reversed-video styling, which is the app-owned highlight that can stack with terminal-native selection feedback.

# Regression test

- Test file path: `crates/tui/app/src/app/controls/transcript.rs`
- Test name: `app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection`
- Command: `cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection -- --exact`
- Expected failure before the fix: the test fails because the selected card text cells contain `Modifier::REVERSED`, proving Cowboy draws an application-owned highlight during card text selection.

# Current failing result

```text
running 1 test
failures:

---- app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection stdout ----

thread 'app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection' (1083691) panicked at crates/tui/app/src/app/controls/transcript.rs:733:9:
mouse selection inside a card should not draw an application-owned reversed-video highlight on top of the terminal's native selection: "│selectable transcript text                                                    │"
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 285 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

# Fix constraints

- Do not render an app-owned reversed-video selection highlight over card text when mouse selection is in progress, because terminals can already show native selection feedback.
- Preserve the ability to compute selected transcript text for clipboard copy unless the product deliberately drops custom transcript copy behavior.
- Preserve transcript card rendering, wrapping, scroll offset behavior, and existing card styles unrelated to selection.
- Update existing selection tests that currently assert `Modifier::REVERSED` on selected transcript text if the fix removes app-owned selection highlighting.
