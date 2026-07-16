## Plan

Implement the input editor behavior documented in `docs/plans/input_box_navigation_and_scrolling/rca.md` by making cursor navigation and rendering consume one shared visual-row layout. Match the OMP `v16.4.5` rules for history boundaries, visual-line movement, sticky display columns, page-sized movement, and cursor-following viewport state without coupling composer paging to transcript scrolling.

Keep the investigator-added regression `crates/tui/app/src/app/controls/composer.rs::nonempty_composer_navigation_matches_omp_up_and_page_up_behavior` unchanged. It is the primary red/green input to the fix and must pass through product-code changes.

## Changes

- In `crates/tui/app/src/app/state.rs`, add a private, copyable `ComposerViewState` held in `Cell<ComposerViewState>`. Limit this interior-mutable state to the last content width, visible input-row budget, viewport start row, and optional preferred display column; keep it separate from the existing transcript `scroll_offset`. Expose narrow `AppState` methods that record layout metrics, read the current snapshot, update the cursor-following viewport, preserve/update the preferred column for vertical moves, and invalidate it after non-vertical moves. Callers must not access the `Cell` directly.
- Keep the existing immutable render interfaces and unchanged regression call sites. After computing the final composer height, slash-suggestion allowance, and content width, `height(&AppState, ...)` must record the resolved content width and input-row budget through the composer-view methods. `rendered_input(&AppState, ...)` (and therefore test-only `lines(&AppState, ...)`) must refresh those metrics and persist the clamped cursor-following viewport through the same seam. Production draws already call `height` before `render`, while the unchanged regression calls `height` and `lines` before `PageUp`, so navigation can read the exact four-row budget and derive its required three-row step without changing any of those signatures. Initialize the snapshot with an OMP-compatible fallback width and a minimum one-row page step for input received before the first draw.
- In `crates/tui/app/src/app/controls/composer.rs`, extend the existing grapheme-aware wrapping pass so each visual row retains its source character range and display width. Use that same layout for rendering, cursor-to-row lookup, target-row-to-cursor mapping, soft wraps, explicit newlines, skipped wrap whitespace, combining graphemes, and wide characters; do not introduce a second wrapping convention.
- Add OMP-compatible vertical movement over those visual rows. `Up` and `Down` move one visual row while retaining the preferred display column across shorter rows and snapping only to valid grapheme boundaries. At non-history top/bottom boundaries, follow OMP by moving to the current logical-line start/end; while browsing history, crossing the first/last visual row selects the older/newer entry, with recalled entries anchored at the top for backward history movement and at the bottom for forward movement.
- Centralize preferred-column invalidation in the `AppState` cursor-editing interface. Preserve it only across consecutive `Up`/`Down`/`PageUp`/`PageDown` moves; clear it after `Left`/`Right`, word-left/word-right, character or newline insertion, backspace/delete, completion replacement, submission/reset, history replacement, test/helper cursor replacement, every current or future line-start/line-end action, and any content-width change that rebuilds visual rows. A vertical boundary action that jumps to a logical-line start/end also establishes a new column and must clear it.
- Add OMP-compatible page movement using `max(1, visible_input_rows - 1)` visual rows from the recorded composer-view snapshot. `PageUp` on an empty editor may enter history, history-boundary page movement may change entries, and non-empty page movement must remain available whenever composer edits are accepted, including while an active run disables submission.
- In `crates/tui/app/src/app/input.rs`, route `Up`, `Down`, `PageUp`, and `PageDown` through the composer navigation path instead of unconditional history recall or inert branches. Keep history selection and slash completion behind the existing submission gate, but do not use that gate to block cursor/page movement inside a non-empty draft.
- Replace the hidden-lines marker branch in `rendered_input` with the viewport slice stored by `ComposerViewState`. Move the viewport only enough to keep the cursor visible, clamp it against the current wrapped-row count and input-row budget after edits or terminal-size changes, preserve all rows available after slash suggestions, and leave transcript follow mode and transcript `scroll_offset` untouched.

## Tests to be added/updated

- Do not modify `nonempty_composer_navigation_matches_omp_up_and_page_up_behavior`; make its exact history, cursor, viewport, page-step, and transcript-isolation assertions pass. Its immutable `height`/`lines` calls must be sufficient to publish the four-row composer geometry used by subsequent key dispatch.
- Add a focused composer-view seam test that calls `height` and `rendered_input` through `&AppState`, then verifies navigation reads the recorded width/row budget and that a width change invalidates the preferred column and clamps the viewport.
- Update the input/history tests in `crates/tui/app/src/app/input.rs` for the new dispatch rules: empty input still recalls history, non-empty input navigates visual rows, history changes only at OMP boundaries, and an active run still blocks submission/history selection while allowing movement within a multiline draft.
- Add focused composer tests for `Down` and `PageDown`, top/bottom boundary behavior, and page-step calculation at more than one visible height so the implementation is not hard-coded to the regression's three-row step.
- Add visual-layout navigation cases covering explicit newlines, soft-wrapped rows, short-row sticky columns, combining graphemes, emoji, and wide characters. Assert exact source cursor positions so movement cannot land inside a grapheme.
- Add preferred-column invalidation coverage. Include the concrete sequence “move vertically onto a short row, move horizontally, then move vertically again” and assert the second vertical move uses the new horizontal column rather than the old sticky column; table-drive the remaining reset actions for word movement, insertion/deletion, completion replacement, history replacement, submission/reset, line-boundary jumps, and layout-width changes.
- Add viewport cases that move both directions through a long draft and exercise input/layout shrinkage, confirming the cursor remains visible, the viewport clamps correctly, no marker consumes an input row, and transcript scrolling remains unchanged.

## How to verify

1. Run the unchanged regression test:

   ```text
   cargo test -p cowboy app::controls::composer::tests::nonempty_composer_navigation_matches_omp_up_and_page_up_behavior -- --exact
   ```

2. Run the affected input and composer test modules:

   ```text
   cargo test -p cowboy app::input::tests
   cargo test -p cowboy app::controls::composer::tests
   ```

3. Run formatting and warning checks required by the repository:

   ```text
   cargo fmt -p cowboy -- --check
   cargo clippy -p cowboy --all-targets -- -D warnings
   ```

4. Smoke-test the TUI with a narrow terminal and a draft containing more rows than the composer can display. Confirm empty-editor `Up` recalls history; non-empty `Up`/`Down` move by visual row; `PageUp`/`PageDown` move by one page minus one row; earlier and later draft rows become visible as the cursor moves; and transcript scroll/follow state does not change.

## Interactive TUI smoke evidence

- Command: `env XDG_STATE_HOME=<temporary-state> XDG_CONFIG_HOME=<temporary-config> cargo run -p cowboy`, run in a 50×20 tmux pane.
- Seeded synthetic history, then confirmed empty-editor `Up` rendered `history smoke`.
- Entered `alpha\nbravo\ncharl`; the terminal cursor moved `(8,18) → (8,17) → (8,18)` across `Up`/`Down` while the draft remained unchanged.
- Entered `l0`–`l24` with ten visible input rows. The cursor and viewport progressed after each key as follows: initial `l24` / `l15`–`l24`; first `PageUp` `l15` / `l15`–`l24`; second `PageUp` `l6` / `l6`–`l15`; first `PageDown` `l15` / `l6`–`l15`; second `PageDown` `l24` / `l15`–`l24`. Each key moved exactly nine visual rows (`visible_rows - 1`) before viewport or document-boundary clamping. No hidden-lines marker consumed an input row.
- Rendered `/help` to create a scrollable transcript and scrolled it with `Ctrl+U`. The transcript region remained unchanged through all four composer page keys. Pressing `Esc` then appended the known `no active background task` Notice, which remained off-screen while the historical `/help` viewport stayed selected; pressing `End` restored follow-latest and revealed the Notice. This demonstrates that composer paging preserved both transcript scroll position and disabled follow mode.

## TODO

- [x] Add the private `Cell<ComposerViewState>` seam and narrow `AppState` methods for immutable layout publication, viewport updates, page metrics, and preferred-column state.
- [x] Make `height` and `rendered_input` publish resolved content width/input-row budget and maintain the viewport through that seam without changing immutable render or investigator-test call signatures.
- [x] Extend the existing wrap result with source-row metadata and implement grapheme-safe OMP vertical/page cursor movement from that shared layout.
- [x] Centralize preferred-column invalidation and apply it to every specified non-vertical cursor, edit, replacement, reset, boundary, and width-change path.
- [x] Route arrow/page keys through cursor movement and history-boundary rules without weakening the submission or history-selection gates.
- [x] Replace marker-based clipping with the persistent cursor-following composer viewport while preserving slash-suggestion and height limits.
- [x] Keep the investigator regression unchanged and update/add the focused seam, input, preferred-column reset, Unicode/wrapping, page, boundary, viewport, and active-run tests described above.
- [x] Run the narrow regression, affected module tests, formatter check, Clippy check, and interactive TUI smoke test.
