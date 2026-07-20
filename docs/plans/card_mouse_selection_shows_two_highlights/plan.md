## Plan

Use `docs/plans/card_mouse_selection_shows_two_highlights/rca.md` as the source of truth for the bug and keep the existing regression test `crates/tui/app/src/app/controls/transcript.rs::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection` as the primary failing input to the fix.

The fix should make terminal-native mouse selection the only visible selection highlight inside transcript cards. Cowboy should continue tracking transcript selection points and selected text for clipboard copy, but `crates/tui/app/src/app/controls/transcript.rs` must stop painting selected card text with `Modifier::REVERSED` during render.

Prefer the smallest cutover: remove the render-time call path that transforms viewport rows into highlighted rows, leave hit-testing and text extraction intact, and delete or adjust only helpers/tests that become obsolete because the product no longer draws an app-owned selection highlight.

## Changes

- In `crates/tui/app/src/app/controls/transcript.rs`, change transcript rendering so `render` passes the viewport rows directly to `Paragraph::new` instead of applying `selected_rows`/`line_with_selection` styling based on `AppState.transcript_selection()`.
- Preserve `selection_point_at`, `selected_text`, `selected_text_from_rows`, `row_selection_range_between`, and related display-width handling because mouse drag still needs accurate selection coordinates and clipboard text.
- Remove the now-obsolete render-only selection styling helpers from `transcript.rs` if no remaining test or production path needs them.
- Remove the `style_transcript_selection` import from `transcript.rs`; delete `style_transcript_selection` from `crates/tui/app/src/app/styles.rs` if it has no remaining callers.
- Update existing transcript selection tests that assert `Modifier::REVERSED` for selected text so they assert the retained text-extraction behavior instead. Do not rewrite or replace the existing RCA-added regression test.
- Avoid changes to mouse event handling in `crates/tui/app/src/app/input.rs` unless the render-only fix is insufficient; if touched, preserve the existing finalization and clipboard-copy behavior.

## Tests to be added/updated

- Keep `app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection` as the regression test. It currently fails before the fix because selected card cells contain `Modifier::REVERSED`; after the fix it must pass without changing its intent.
- Update `app::controls::transcript::tests::selected_text_extracts_single_row_and_highlights_range` or its successor so it no longer expects a rendered highlight, while still covering single-row selected text extraction.
- Keep `app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy` passing to prove selection state and clipboard text still work.
- No new screenshot-driven test is required because the existing buffer-level regression test verifies the Cowboy-rendered half of the duplicate highlight.

## How to verify

1. Run the primary regression test:

   ```bash
   cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection -- --exact
   ```

   Expected result: the test passes, and the selected card text cells do not contain `Modifier::REVERSED`.

2. Run transcript control tests:

   ```bash
   cargo test -p cowboy app::controls::transcript::tests::
   ```

   Expected result: all transcript rendering, wrapping, hit-testing, and selected-text tests pass.

3. Run the clipboard-preservation mouse test:

   ```bash
   cargo test -p cowboy app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy -- --exact
   ```

   Expected result: the test passes and still observes `Some("selectable".to_string())` in pending clipboard text after mouse-up finalization.

4. Run compiler and lint checks for the changed package:

   ```bash
   cargo check -p cowboy
   cargo clippy -p cowboy --all-targets -- -D warnings
   ```

   Expected result: both commands complete without compiler warnings, Clippy warnings, or unused-code/import failures from removed selection-rendering helpers.

## TODO

- [x] TODO-01: Stop rendering application-owned transcript selection highlights in card rows.
  - Procedure: Edit `crates/tui/app/src/app/controls/transcript.rs` so `render` creates the transcript `Paragraph` from unmodified viewport rows and no longer applies selection styling while drawing card content.
  - Expected result: rendering an active transcript selection no longer adds `Modifier::REVERSED` to selected card text cells, while the card text and surrounding card border still render normally.
  - Observed result: `render` now passes unmodified `viewport.rows` to `Paragraph::new`, and the unchanged RCA regression test passed with selected card cells free of `Modifier::REVERSED`.

- [x] TODO-02: Preserve transcript mouse selection text extraction and clipboard finalization.
  - Procedure: Keep the existing `selection_point_at` and `selected_text` paths intact, then run `cargo test -p cowboy app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy -- --exact`.
  - Expected result: the command passes and the test still reports the selected text as `selectable` during drag and queued clipboard text as `Some("selectable".to_string())` after mouse up.
  - Observed result: `cargo test -p cowboy app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy -- --exact` passed; the test assertions cover selected text `selectable` during drag and pending clipboard text `Some("selectable".to_string())` after mouse up.

- [x] TODO-03: Remove or update obsolete selection-highlight helpers and tests.
  - Procedure: Remove unused render-only highlight helpers/imports after the render path no longer uses them, and update existing tests that asserted `Modifier::REVERSED` so they assert retained selected-text behavior instead.
  - Expected result: `cargo check -p cowboy` reports no unused imports, unused functions, or compiler warnings related to transcript selection styling.
  - Observed result: removed the render-only selection styling helpers and `style_transcript_selection`; updated the single-row selected-text test to assert extraction only; `cargo check -p cowboy` passed.

- [x] TODO-04: Prove the RCA regression test passes without replacing it.
  - Procedure: Run `cargo test -p cowboy app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection -- --exact` after the code change.
  - Expected result: the existing test passes and confirms selected card cells do not contain `Modifier::REVERSED`.
  - Observed result: the unchanged regression test `app::controls::transcript::tests::card_mouse_selection_does_not_render_app_highlight_over_terminal_selection` passed after the render-path change.

- [x] TODO-05: Run the focused transcript and lint verification suite.
  - Procedure: Run `cargo test -p cowboy app::controls::transcript::tests::`, `cargo test -p cowboy app::input::tests::transcript_mouse_selection_updates_finalizes_and_queues_copy -- --exact`, and `cargo clippy -p cowboy --all-targets -- -D warnings`.
  - Expected result: all commands pass with no test failures, compiler warnings, or Clippy warnings.
  - Observed result: transcript control tests passed with 21 tests, the clipboard-preservation mouse test passed, and `cargo clippy -p cowboy --all-targets -- -D warnings` passed.
