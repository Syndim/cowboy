## Plan

Use the approved root-cause analysis in `docs/plans/input_box_cuts_words_across_lines/rca.md` and the unchanged regression test `crates/tui/app/src/app/tests.rs::app::tests::draw_moves_whole_word_to_continuation_row` as the fix inputs. Replace the composer's character-only soft wrapping with one canonical, word-aware layout path that also owns row counting and cursor coordinates. A word that fits within one content row must move intact when it does not fit in the remaining cells; a word wider than a row must continue to fall back to display-width-aware character wrapping.

## Changes

- Update `crates/tui/app/src/app/controls/composer.rs` so `wrapped_input_lines` tracks breakable whitespace and word boundaries in addition to Unicode display-cell width. Match the existing `Wrap { trim: false }` behavior at a soft-wrap boundary, preserve explicit `\n` boundaries and continuation indentation, and retain character-level splitting for overlong unbroken words.
- Keep source cursor offsets associated with the text as it is reflowed so a cursor before, inside, or after a word moved to a continuation row resolves to the correct visual row and display-cell column, including double-width and zero-width characters.
- Make `height` derive its input-row count from the same composer-owned wrapping result used by `rendered_input`, removing the separate `Paragraph::line_count` wrapping policy. Keep the existing composer height cap, slash-suggestion row budget, latest-input clipping marker, first-row prompt, continuation prompt, and cursor visibility behavior unchanged.
- Remove imports or comments that become obsolete when the independent Ratatui line-counting path is removed; do not change workflow runtime behavior or any crate outside the TUI composer and its tests.

## Tests to be added/updated

- Keep `crates/tui/app/src/app/tests.rs::app::tests::draw_moves_whole_word_to_continuation_row` unchanged. It remains the end-to-end regression test proving `hello bananas` renders as `> hello` and `  bananas` at the constrained terminal width.
- Add focused tests in `crates/tui/app/src/app/controls/composer.rs` for the canonical wrapper: a fitting word moves intact, cursor coordinates follow a moved word (including a cursor inside it), and height/rendered rows agree with that layout.
- Preserve and run the existing coverage for overlong unbroken input, explicit newlines and clipping, slash-suggestion budgeting, moved cursors, and Unicode display widths. Add a boundary assertion only where those existing tests do not exercise the word-aware path; do not replace the investigator's full-TUI repro.

## How to verify

1. Run the unchanged repro and confirm it passes:
   `cargo test -p cowboy app::tests::draw_moves_whole_word_to_continuation_row -- --exact --nocapture`
2. Run the focused composer and app rendering tests:
   `cargo test -p cowboy app::controls::composer::tests`
   `cargo test -p cowboy app::tests`
3. Run Clippy for the touched crate and fix all warnings:
   `cargo clippy -p cowboy --all-targets -- -D warnings`
4. Confirm formatting and repository hygiene without changing the repro test:
   `cargo fmt --check`
   `git diff --check`
   `git status --short`

## TODO

- [x] Implement word-boundary-aware wrapping in `wrapped_input_lines` while retaining display-width-aware fallback for overlong words.
- [x] Preserve explicit newline, whitespace, prompt, continuation-indent, clipping, slash-suggestion, and cursor-mapping behavior in the canonical layout result.
- [x] Change composer height calculation to use the same wrapping policy as rendering and remove the obsolete independent line-counting path.
- [x] Add focused composer tests for intact word movement, moved-word cursor coordinates, and height/render consistency without modifying the investigator-added repro test.
- [x] Run the unchanged regression test and focused composer/app test suites.
- [x] Run Clippy, formatting, diff, and status checks and resolve any warnings or hygiene failures caused by the change.
