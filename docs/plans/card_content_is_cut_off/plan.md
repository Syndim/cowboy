# Card content is cut off

## Plan

Use the reviewed RCA in `docs/plans/card_content_is_cut_off/rca.md` as the source of truth. The defect is in Cowboy card rendering, not in agent output: `crates/tui/app/src/app/card.rs` wraps body rows to the full interior width, then `framed_body_line` has no remaining padding before the right border when a wrapped row fills that width.

Fix the card renderer so body content reserves a right-side padding cell when the card is wide enough, while preserving total rendered row width, markdown span styling, named sections, empty-card rendering, and panic-free behavior for zero-width or very narrow cards. Keep the investigator-added regression test `crates/tui/app/src/app/card.rs::app::card::tests::card_body_rows_keep_padding_before_right_border` unchanged and use it as the guard for the fix.

Do not change agent output, trim message text, rewrite transcript history, or implement scrollbar/transcript viewport work for this bug. The reported scrollbar-like text is sample card content; the RCA-owned fix is the card body border separation.

## Changes

- In `crates/tui/app/src/app/card.rs`, separate the card body wrapping width from the full border interior width.
- When `interior_width > 1`, wrap body section lines to `interior_width - 1` so `framed_body_line` always has at least one padding cell before the right border.
- When the card is too narrow to reserve padding, keep the existing saturation behavior and avoid producing rows wider than the requested card width after `width.max(MIN_CARD_WIDTH)` normalization.
- Leave title rendering, border lines, section dividers, metadata truncation, and empty-card no-border behavior unchanged.
- Update only existing test expectations that intentionally encoded body text filling the entire interior width. Do not weaken tests that verify all visual rows are retained.

## Tests to be added/updated

- Keep `card_body_rows_keep_padding_before_right_border` unchanged. It is the RCA regression test and must pass because product code changes, not because the test is rewritten.
- Update existing card rendering tests whose expected strings assume `│text│` with no right padding for full body rows. The updated expectations should assert `│text │` or equivalent right-padding behavior while preserving row count and width assertions.
- Add narrow-width card body coverage if existing tests do not already exercise body rendering at widths `0`, `1`, `2`, and `3`. The test should assert no panic and every rendered row stays within the normalized card width.

## How to verify

1. Run `cargo test -p cowboy card_body_rows_keep_padding_before_right_border`.
   - Expected result: the test passes without modifying the test body, proving full wrapped body rows keep a blank column before the right border.
2. Run `cargo test -p cowboy app::card`.
   - Expected result: all card module tests pass, including existing title, empty-card, wrapping, large-body, and named-section coverage.
3. Inspect any changed assertions in `crates/tui/app/src/app/card.rs`.
   - Expected result: assertion changes only reflect the new intentional right-padding column; no test drops row-retention, width, or styling coverage.

## TODO

- [x] TODO-01: Preserve the RCA regression test as the failing guard.
  - Procedure: run `cargo test -p cowboy card_body_rows_keep_padding_before_right_border` before changing product code and leave `crates/tui/app/src/app/card.rs::app::card::tests::card_body_rows_keep_padding_before_right_border` unchanged.
  - Expected result: before the fix, the command fails with a body row that has a non-space character immediately before the right border; after the fix, the same unchanged test passes.
  - Implementer observed result: before changing product code, `cargo test -p cowboy card_body_rows_keep_padding_before_right_border` failed with rows such as `"│saturation, and follow│"`; after the card wrapping fix, the unchanged regression test passed.

- [x] TODO-02: Reserve right-side body padding in card wrapping.
  - Procedure: update `crates/tui/app/src/app/card.rs` so body section lines are wrapped to a padding-aware content width when `interior_width > 1`, then rendered through the existing `framed_body_line` padding calculation.
  - Expected result: for a 24-column card, every body row produced by the repro test has a space before the closing `│`, and no rendered row exceeds 24 display columns.
  - Implementer observed result: `section_wrapped_lines` now wraps body rows to `interior_width - 1` when `interior_width > 1` and still renders through `framed_body_line`; the unchanged 24-column repro test passed, proving every framed body row keeps a blank column before `│` and stays within width.

- [x] TODO-03: Preserve existing card chrome and narrow-width behavior.
  - Procedure: keep `MIN_CARD_WIDTH`, `title_line`, `border_line`, `section_divider`, and the `width <= 2` branch in `framed_body_line` functionally unchanged; render cards with body content at widths `0`, `1`, `2`, and `3` during test coverage.
  - Expected result: rendering does not panic, rows stay within the normalized card width, title-only and empty-body cards still omit border chrome, and section dividers still span the full card width.
  - Implementer observed result: the padding change did not modify `MIN_CARD_WIDTH`, title rendering, border rendering, section dividers, or the `width <= 2` framed-body branch; `narrow_body_cards_stay_within_normalized_width` renders widths `0`, `1`, `2`, and `3`, and `cargo test -p cowboy app::card` passed the narrow, title-only, empty-body, and named-section coverage.

- [x] TODO-04: Update full-width body rendering expectations without weakening coverage.
  - Procedure: adjust only card tests whose expected body strings assumed no right padding, such as `wraps_body_lines_inside_borders_without_collapsing` and `retains_all_visual_rows_after_wrapping`, while keeping assertions for retained content, row counts, and maximum display width.
  - Expected result: tests verify the same body text is still present in order, all wrapped visual rows are retained, and changed expected strings differ only by the intentional right-padding column or consequent wrap point.
  - Implementer observed result: only the full-width body string expectations in `wraps_body_lines_inside_borders_without_collapsing` and `retains_all_visual_rows_after_wrapping` changed; row-retention, `more rows` absence, row-count, and width assertions remain, and `cargo test -p cowboy app::card` passed.

- [x] TODO-05: Add or confirm narrow-width regression coverage for padded body rows.
  - Procedure: add a focused card test if no existing test covers body rendering at widths `0`, `1`, `2`, and `3`; assert no panic and display width bounded by `requested_width.max(MIN_CARD_WIDTH)` for every row.
  - Expected result: very narrow cards remain panic-free and the padding change does not introduce overflow at minimum widths.
  - Implementer observed result: added `narrow_body_cards_stay_within_normalized_width`, which renders a body card at widths `0`, `1`, `2`, and `3` and asserts every row width is bounded by `requested_width.max(MIN_CARD_WIDTH)`; `cargo test -p cowboy app::card` passed.

- [x] TODO-06: Run the targeted card verification commands.
  - Procedure: run `cargo test -p cowboy card_body_rows_keep_padding_before_right_border` and `cargo test -p cowboy app::card` after implementation.
  - Expected result: both commands pass with no Rust compiler warnings emitted for the changed code.
  - Implementer observed result: after implementation, `cargo test -p cowboy card_body_rows_keep_padding_before_right_border` passed and `cargo test -p cowboy app::card` passed; neither command emitted Rust compiler warnings.
