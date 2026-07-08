## Bug behavior

Empty TUI cards render border chrome even when there are no content rows to frame. A card constructed with only a status and title currently renders the title plus a top border and bottom border:

```text
● Idle tool
╭──────────────────────────────────────────────────────────────────────────────╮
╰──────────────────────────────────────────────────────────────────────────────╯
```

The expected behavior is that a card with no content should render the title without card border chrome.

## Root cause

`Card::render` in `crates/tui/app/src/app/card.rs` unconditionally pushes `border_line('╭', '╮', width)` before rendering sections and `border_line('╰', '╯', width)` after rendering sections. It does not first determine whether any section would produce wrapped content rows.

This affects cards built with no sections, such as status-only workflow event cards, and cards whose body section has an empty `lines` vector. In the empty case, the section loop contributes no framed body lines, leaving orphan top and bottom borders.

## Reproduction steps

1. Add the focused regression test `renders_empty_card_without_border_chrome` in `crates/tui/app/src/app/card.rs`.
2. Run the narrow test command:

```bash
cargo test -p cowboy renders_empty_card_without_border_chrome
```

3. Observe that the rendered empty card still includes rounded border characters.

## Regression test

- Test file path: `crates/tui/app/src/app/card.rs`
- Test name: `app::card::tests::renders_empty_card_without_border_chrome`
- Command: `cargo test -p cowboy renders_empty_card_without_border_chrome`
- Expected failure before the fix: the test fails because the rendered empty card contains rounded border chrome (`╭`, `╮`, `╰`, `╯`) even though the card has no content rows.

## Current failing result

```text
running 1 test
failures:

---- app::card::tests::renders_empty_card_without_border_chrome stdout ----

thread 'app::card::tests::renders_empty_card_without_border_chrome' panicked at crates/tui/app/src/app/card.rs:397:9:
● Idle tool
╭──────────────────────────────────────────────────────────────────────────────╮
╰──────────────────────────────────────────────────────────────────────────────╯
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::card::tests::renders_empty_card_without_border_chrome

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 158 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Do not remove borders from cards that have at least one visible content row.
- Preserve the existing title line, metadata rendering, truncation, wrapping, section labels, and truncation markers for non-empty cards.
- Treat both cards with no sections and cards with empty body sections as having no content for border rendering.
- Keep the fix in the card rendering layer so event and app card call sites do not need one-off border decisions.
