## Plan

Use the RCA in `docs/plans/empty_card_shows_border/rca.md` as the source of truth. Fix empty card rendering in `crates/tui/app/src/app/card.rs` at the card-rendering layer, not in event/card call sites. Keep the investigator-added regression test `crates/tui/app/src/app/card.rs::app::card::tests::renders_empty_card_without_border_chrome` unchanged as the primary repro input.

The fix should make `Card::render` return only the title line when no section produces visible wrapped content rows. Cards with at least one wrapped content row must keep the existing rounded border, section divider, framing, title metadata, truncation, and wrapping behavior.

## Changes

- In `Card::render`, pre-render each section with `section_wrapped_lines(section, interior_width)` before emitting border chrome.
- Determine whether the card has content from the pre-rendered section rows, not from the number of sections or labels.
- If no pre-rendered section has rows, return the title row immediately and do not emit top border, bottom border, vertical borders, or section dividers.
- If at least one section has rows, emit the existing top border, section dividers, framed body rows, and bottom border using the pre-rendered rows so wrapping work is not duplicated.
- Preserve current behavior for non-empty cards, including labels on sections, metadata rendering, truncation markers, and width clamping.

## Tests to be added/updated

- Keep `renders_empty_card_without_border_chrome` unchanged; it is the investigator-added regression test and should pass after the product fix.
- Add a focused card-rendering test for a card with an empty `CardSection::body(Vec::new())` to confirm empty sections also suppress border chrome.
- Keep existing non-empty card tests passing, especially the tests that assert rounded chrome, section dividers, wrapping, and truncation markers.

## How to verify

Run the narrow regression and card-rendering checks after implementing the fix:

```bash
cargo test -p cowboy renders_empty_card_without_border_chrome
cargo test -p cowboy app::card::tests
cargo clippy -p cowboy --all-targets -- -D warnings
```

Expected result: all commands pass. The first command should change from the RCA-confirmed failure to a pass because the rendered empty card contains `● Idle tool` and no rounded or vertical border characters.

## TODO

- [x] Pre-render card sections in `Card::render` and compute whether any wrapped content rows exist.
- [x] Return only the title line from `Card::render` when no wrapped content rows exist.
- [x] Reuse pre-rendered section rows for non-empty card rendering while preserving existing border, divider, wrapping, metadata, and truncation behavior.
- [x] Add a regression test covering an explicitly empty body section without rewriting or replacing `renders_empty_card_without_border_chrome`.
- [x] Run the narrow regression test, card module tests, and package Clippy check listed in How to verify.
- [x] Update stale app transcript test expectation for title-only empty cards.
- [x] Run reviewer-reported full package and focused app rendering tests.
