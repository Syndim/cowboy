## Plan

Base the fix on `docs/plans/tui_long_text_log_input_lag/rca.md` and keep `crates/tui/src/app/tests.rs::app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history` as the primary regression input. The bug is in TUI transcript rendering: typing marks the whole screen dirty, then `crates/tui/src/app/controls/transcript.rs::lines` renders and wraps every historical transcript entry before slicing visible rows.

Fix `transcript::lines` so redraw work for the normal follow-latest case is bounded by the visible transcript height, not by total transcript history. Preserve the existing visible output contract: chronological row order, tail visibility, manual scroll offsets, pending prompt rendering, wrapping behavior, blank separators, and span styles.

Prefer a bounded tail-collection implementation in `crates/tui/src/app/controls/transcript.rs` over moving state or workflow logic into `crates/tui/src/app.rs` or runtime crates. Keep the change local to TUI rendering unless implementation evidence shows a small AppState accessor is required.

## Changes

- Replace the full-history path in `transcript::lines` with a bounded suffix renderer for non-empty event logs.
- Keep the empty-transcript path small and behaviorally identical by reusing `empty_lines()`.
- Compute the number of visual rows needed as `max_visible_lines + scroll_offset`, using saturating arithmetic.
- Collect logical transcript content from newest to oldest until enough wrapped visual rows are available, then stop without rendering older entries.
- Preserve pending prompt behavior by appending `render_pending_prompt_lines(prompt)` only when the same prompt is not already the latest workflow entry, matching current `all_lines` semantics.
- Preserve existing blank separators by treating the separator after each transcript entry as part of that entry while collecting from the tail.
- Reuse `TranscriptEntry::render_lines`, `render_pending_prompt_lines`, and `wrap_line` so styles, event formatting, and wrapping stay consistent.
- Slice the collected visual rows with the existing scroll-offset semantics: follow-latest shows the tail, positive offsets show older wrapped rows, and offsets beyond available history clamp to the earliest available rows.
- Avoid adding workflow runtime behavior, persistence, or event mutation to the TUI crate.

## Tests to be added/updated

- Keep the investigator-added repro test unchanged: `crates/tui/src/app/tests.rs::app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history`.
- Add a focused unit test in `crates/tui/src/app/controls/transcript.rs` that renders a long transcript with a small viewport and asserts the tail marker remains visible while early filler entries are absent.
- Add or update a focused transcript unit test for positive `scroll_offset` over wrapped rows so the bounded collector still shows older visual rows and hides the latest tail while scrolled up.
- Keep existing transcript style and prompt tests passing; do not rewrite them around the new implementation.

## How to verify

- Run the pre-existing repro test after the fix:
  - `cargo test -p cowboy app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history -- --exact`
- Run the transcript rendering tests after the fix:
  - `cargo test -p cowboy app::controls::transcript::tests`
- If either focused command fails, inspect whether the bounded collector changed one of the preserved contracts: tail visibility, scroll offset slicing, pending prompt placement, wrapping, blank separators, or span styles.

## TODO

- [x] Read `docs/plans/tui_long_text_log_input_lag/rca.md` and the repro test before editing.
- [x] In `crates/tui/src/app/controls/transcript.rs`, refactor `lines` to avoid `visual_rows(all_lines(state), wrap_width)` for non-empty event logs.
- [x] Add a bounded tail visual-row collector that walks pending prompt lines and transcript entries from newest to oldest and stops once `max_visible_lines + scroll_offset` wrapped rows are available.
- [x] Preserve current prompt de-duplication semantics when appending pending prompt lines.
- [x] Preserve current blank separator behavior after transcript entries.
- [x] Preserve wrapping and span styles by reusing the existing rendering and wrapping helpers.
- [x] Apply the existing scroll-offset slicing semantics to the bounded row set, including clamping when the offset exceeds available history.
- [x] Add the focused long-history tail unit test in `crates/tui/src/app/controls/transcript.rs`.
- [x] Add or update the focused scrolled wrapped-rows unit test in `crates/tui/src/app/controls/transcript.rs`.
- [x] Run `cargo test -p cowboy app::tests::draw_with_typed_input_does_not_scale_with_full_transcript_history -- --exact`.
- [x] Run `cargo test -p cowboy app::controls::transcript::tests`.
- [x] Address reviewer feedback by removing the out-of-scope `crates/workflow/engine/src/run_lock.rs` working-tree modification.
