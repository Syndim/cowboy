# Bug behavior

A user-supplied screenshot shows transcript text being selected while a separate block at the far right of the transcript row is also visually emphasized. The far-right block is scrollbar chrome, not selected text. The same UI also reserves the rightmost transcript column for scrollbar rendering, so that column is not selectable text content.

# Root cause

`crates/tui/app/src/app/controls/transcript.rs` renders scrollbar chrome whenever transcript content overflows. `render` calls `content_viewport`; when overflow is present, `content_viewport` shrinks the transcript content area by one column and `render` draws a `Scrollbar` in the rightmost column with `█` as the thumb and `│` as the track.

`selection_point_at` uses the same shrunken `content_area` for hit testing. As a result, the rightmost transcript column is excluded from text selection and occupied by a scrollbar glyph. During selection, the scrollbar thumb looks like a second highlighted region away from the selected text.

# Reproduction steps

1. Use a transcript with enough rows to overflow the visible transcript viewport.
2. Render the transcript in a narrow terminal area.
3. Select transcript text near the overflowing content.
4. Observe that the rightmost transcript column is excluded from transcript selection hit testing and contains scrollbar chrome: a `█` thumb or extra track after normal transcript borders.

The focused regression test reproduces the underlying repository behavior without requiring a human terminal drag: it renders an overflowing transcript and asserts that the last visible column remains selectable, no scrollbar thumb appears in the final column, and no extra scrollbar track is appended after normal transcript borders.

# Regression test

- Test file path: `crates/tui/app/src/app/controls/transcript.rs`
- Test name: `app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome`
- Command: `cargo test -p cowboy app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome -- --exact`
- Expected failure before the fix: the test fails because `selection_point_at` returns `None` for the rightmost column; the rendered rows also show a final-column scrollbar thumb (`█`) and extra track appended after normal transcript borders (`││` / `╯│`).

# Current failing result

```text
running 1 test
failures:

---- app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome stdout ----

thread 'app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome' (869798) panicked at crates/tui/app/src/app/controls/transcript.rs:712:9:
overflowing transcripts should keep the last visible column selectable instead of reserving it for scrollbar chrome: ["│selectable row 16    ││", "│selectable row 17    ││", "│selectable row 18    ││", "│selectable row 19    ││", "╰─────────────────────╯│", "                       █"]
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::controls::transcript::tests::overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 277 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

# Fix constraints

- Do not keep a separate scrollbar column in the transcript viewport; overflowing transcript content should use the full transcript area width.
- Keep transcript scrolling behavior available through existing scroll input state, but remove visible scrollbar chrome from transcript rendering.
- Update or remove existing tests that intentionally assert scrollbar rendering or scrollbar-column hit-test exclusion, including `selection_point_hit_testing_excludes_scrollbar_column`, `overflowing_content_reserves_rightmost_scrollbar_column`, `scrollbar_thumb_moves_up_and_returns_to_bottom`, `one_long_stream_reports_unmeasured_older_overflow`, and `draw_shows_transcript_scrollbar_without_overwriting_status_or_composer`.
- Preserve current transcript wrapping, tail rendering, and scroll offset behavior while changing only the visible scrollbar/hit-test contract.
