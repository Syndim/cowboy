## Bug behavior

On a wide terminal, transcript cards do not fill the available horizontal space. The card title/body chrome is rendered at 80 columns and remains left-aligned, leaving the rest of the transcript area blank on the right.

## Root cause

`crates/tui/app/src/app/card.rs` clamps the requested render width with `width.clamp(MIN_CARD_WIDTH, DEFAULT_CARD_WIDTH)`. `DEFAULT_CARD_WIDTH` is 80, so any transcript width above 80 is discarded before card borders, body padding, and title truncation are calculated.

The transcript renderer passes the current available area width into `TranscriptEntry::render_lines_for_width`, so the full-width value reaches `Card::render`; the card renderer then caps it to 80. This makes wide-screen cards visually occupy only the left side of the UI.

## Reproduction steps

1. Add a card with body content so border rows are rendered.
2. Render it with an available width of 120 columns.
3. Measure the rendered border row display width.
4. The current result is 80 columns instead of the expected 120 columns.

## Regression test

- Test file path: `crates/tui/app/src/app/card.rs`
- Test name: `app::card::tests::wide_cards_expand_to_available_width`
- Command: `cargo test -p cowboy app::card::tests::wide_cards_expand_to_available_width`
- Expected failure before the fix: the assertion reports `left: 80` and `right: 120`, proving the card border is capped at 80 instead of consuming the available transcript width.

## Current failing result

```text
running 1 test
failures:

---- app::card::tests::wide_cards_expand_to_available_width stdout ----

thread 'app::card::tests::wide_cards_expand_to_available_width' panicked at crates/tui/app/src/app/card.rs:414:9:
assertion `left == right` failed: card border should consume the available transcript width
  left: 80
 right: 120
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::card::tests::wide_cards_expand_to_available_width

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 164 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Do not reintroduce a fixed 80-column cap for card rendering when the transcript area is wider.
- Preserve the minimum-width guard so very narrow terminals still render safely.
- Keep title truncation, body wrapping, border rows, and section dividers bounded by the actual available width.
- Keep existing narrow-width behavior covered by current tests.
