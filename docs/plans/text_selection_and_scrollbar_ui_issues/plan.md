## Plan

Base the fix on `docs/plans/text_selection_and_scrollbar_ui_issues/rca.md` and the existing regression test `crates/tui/app/src/app/controls/transcript.rs::app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome`. Keep that repro test as an input to the fix; do not rewrite or replace it.

Fix the source of the duplicated visual highlight by removing transcript scrollbar chrome entirely. Overflowing transcript content should render across the full transcript `Rect`, and selection hit testing should treat the rightmost visible transcript column as selectable content instead of reserved scrollbar space. Preserve existing transcript scroll state, scroll key behavior, tail rendering, wrapping, and status/composer layout.

## Changes

- In `crates/tui/app/src/app/controls/transcript.rs`, remove the Ratatui `Scrollbar`, `ScrollbarOrientation`, and `ScrollbarState` rendering path from `render`.
- Stop shrinking overflowing transcript content by one column in `content_viewport`; build the viewport with the full `area.width` and return/render the full transcript area for both overflowing and non-overflowing content.
- Remove scrollbar-only support code that becomes unused, including the scrollbar position helper and scrollbar-specific imports/styles.
- Ensure `selection_point_at` continues using the content viewport returned by `content_viewport`, which should now include the rightmost column for overflowing transcript content.
- Update tests that encode the old scrollbar-column contract so they assert the new contract: no scrollbar thumb or track is drawn, the rightmost transcript column remains selectable, and existing scroll offset behavior still works without visible chrome.
- Update the app-level draw test that currently expects a transcript scrollbar so it instead verifies overflowing transcript rendering does not draw scrollbar chrome and still does not overwrite the status strip or composer.

## Tests to be added/updated

- Keep `app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome` unchanged and make it pass through product code changes.
- Update `selection_point_hit_testing_excludes_scrollbar_column` to the new selection contract, or rename it to describe rightmost-column selection for overflowing transcripts.
- Update or remove scrollbar-specific assertions in `overflowing_content_reserves_rightmost_scrollbar_column`, `scrollbar_thumb_moves_up_and_returns_to_bottom`, `one_long_stream_reports_unmeasured_older_overflow`, and `short_content_has_no_scrollbar` so tests cover retained behavior rather than deleted chrome.
- Update `crates/tui/app/src/app/tests.rs::draw_omits_transcript_scrollbar_without_overwriting_status_or_composer` to assert no scrollbar chrome appears while status and composer rows remain intact.

## How to verify

1. Confirm the existing repro test fails before the fix:
   `cargo test -p cowboy app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome -- --exact`
2. After implementation, rerun the repro test and confirm it passes:
   `cargo test -p cowboy app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome -- --exact`
3. Run the focused transcript-control test module:
   `cargo test -p cowboy app::controls::transcript::tests`
4. Run the focused app draw test after updating it:
   `cargo test -p cowboy app::tests::draw_omits_transcript_scrollbar_without_overwriting_status_or_composer -- --exact`
5. Run the crate-level check required for this UI crate change:
   `cargo test -p cowboy`

## TODO

- [x] Remove transcript scrollbar rendering and unused scrollbar imports/helpers from `crates/tui/app/src/app/controls/transcript.rs`.
- [x] Change transcript viewport sizing so overflowing content uses the full transcript width.
- [x] Keep rightmost-column selection hit testing enabled for overflowing transcript content.
- [x] Update transcript-control tests that assert the old scrollbar-column behavior while preserving the investigator-added repro test unchanged.
- [x] Update the app-level draw test to assert no scrollbar chrome and unchanged status/composer layout.
- [x] Run the focused repro test, focused transcript tests, updated app draw test, and `cargo test -p cowboy`.
