# Card content is cut off

## Bug behavior

Long card body text can render flush against the right border of the card. In a narrow transcript, wrapped text such as a long file path is split directly before the border, for example a row can end with `crates/t│` and the next row continues with `ui/app...`. No content is removed from the underlying string, but the UI makes the text look cut off because there is no blank column separating full-width body text from the right border.

The final ellipsis shown in the reported sample can still be agent-provided content if the raw message ended with an ellipsis. The reproducible UI defect owned by Cowboy is the card renderer allowing body text to touch the card border.

## Root cause

`crates/tui/app/src/app/card.rs` renders card body rows with `framed_body_line`. The function uses `width - 2` as the body content width, pushes the left border, then pushes the wrapped content spans, optional trailing padding, and the right border. When a wrapped body row exactly fills the interior width, trailing padding is zero, so the last text cell is immediately adjacent to the right border.

`wrap_line` wraps body content at that same full interior width and splits on display width, not word or path boundaries. That combination makes long body text wrap inside paths and hyphenated phrases while also touching the border on full rows. The reported behavior is therefore produced by Cowboy's card rendering, not by the agent truncating those interior wrapped rows.

## Reproduction steps

1. Add a card body containing a long instruction with a path-like segment.
2. Render the card at a narrow width where body text wraps.
3. Inspect body rows framed by `│` borders.
4. At least one full wrapped row has a non-space character immediately before the right border, reproducing the apparent cut-off effect.

## Regression test

- Test file path: `crates/tui/app/src/app/card.rs`
- Test name: `app::card::tests::card_body_rows_keep_padding_before_right_border`
- Command: `cargo test -p cowboy card_body_rows_keep_padding_before_right_border`
- Expected failure before the fix: the test panics because rendered card body rows include text immediately adjacent to the right border instead of leaving one blank column before `│`.

## Current failing result

Command run:

```text
cargo test -p cowboy card_body_rows_keep_padding_before_right_border
```

Observed output:

```text
running 1 test
failures:

---- app::card::tests::card_body_rows_keep_padding_before_right_border stdout ----

thread 'app::card::tests::card_body_rows_keep_padding_before_right_border' (1237236) panicked at crates/tui/app/src/app/card.rs:515:9:
card body rows should keep one blank column before the right border so wrapped text does not look cut off: ["│- Keep scroll amount, │", "│saturation, and follow│", "│-latest transitions in│", "│ crates/tui/app/src/ap│", "│p/state.rs.           │"]
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::card::tests::card_body_rows_keep_padding_before_right_border

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 302 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Do not change agent output or trim/rewrite message text to mask the symptom.
- Fix card layout in `crates/tui/app/src/app/card.rs` so body text has visible separation from the right border when rows fill the available width.
- Preserve full card width behavior and keep all rendered rows within the requested width.
- Preserve existing markdown span styling and multiline body rendering.
- Keep zero-width and very narrow widths panic-free.
- Update existing card rendering expectations only where they intentionally assumed no horizontal body padding.
