## Bug behavior

Cowboy does not match the input-editor behavior of OMP `16.4.5`:

- With submitted history available, pressing `Up` while a non-empty multiline draft is present replaces the entire draft with the previous history entry. The draft should remain unchanged and the cursor should move to the preceding visual line at the same display column.
- `PageUp` and `PageDown` do not move through a long draft. Earlier rows remain clipped while the cursor stays at the end.
- The long-draft viewport spends one of its limited content rows on an `earlier line(s) hidden` marker instead of retaining OMP's cursor-following viewport. For a six-row bordered composer, OMP exposes four input rows and moves three visual rows per page.
- OMP enters history only when the editor is empty (or when already browsing history at a boundary), uses `Up`/`Down` for visual-line cursor movement in non-empty input, uses page keys for page-sized cursor movement, and retains a cursor-following editor scroll offset. The reference implementation and its `setMaxHeight(6)` page-navigation test are in [`packages/tui/src/components/editor.ts`](https://github.com/can1357/oh-my-pi/blob/v16.4.5/packages/tui/src/components/editor.ts) and [`packages/tui/test/editor.test.ts`](https://github.com/can1357/oh-my-pi/blob/v16.4.5/packages/tui/test/editor.test.ts) at tag `v16.4.5`.

The deterministic regression test reproduces the history, exact page-step, rendered viewport, and transcript-scroll-isolation behavior with synthetic data and controlled dimensions.
## Root cause

`crates/tui/app/src/app/input.rs` dispatches `Up` directly to `AppState::history_previous()` whenever submission is enabled. It does not first test whether the input is empty, inspect the cursor's visual line, or provide a vertical-movement path. The same match has no `PageUp` or `PageDown` branch, so both keys fall through to the inert default arm. When a run accepts draft edits but temporarily disables submission, the submission guard also prevents `Up`/`Down` from performing any editor navigation.

`crates/tui/app/src/app/state.rs` wraps `tui_input::Input` and exposes character, word, and horizontal cursor movement only. Resolved dependency `tui-input 0.15.3` has no vertical or page request variants, so OMP-style navigation must be calculated from Cowboy's wrapped visual-line layout and applied with `InputRequest::SetCursor`; no existing state method does that.

`crates/tui/app/src/app/controls/composer.rs` computes wrapped rows and reconstructs a clipped window around the current cursor on every render. It does not retain a composer viewport offset. When input exceeds the content height, `rendered_input` reserves one visible row for a hidden-lines marker, so a four-row viewport at the draft tail shows the marker plus `l7` through `l9` rather than OMP's four-row `l6` through `l9` viewport. The only persisted `scroll_offset` in `AppState` belongs to the transcript, not the composer.

The strengthened regression verifies each boundary. `Up` changes `alpha\nbravo\ncharl` to `from history`. With a six-row composer and four visible content rows, both `PageUp` presses leave the cursor at global position `29` instead of moving by OMP's three-row page step to positions `20` and `11`. All three renders remain on the marker plus `l7` through `l9` instead of progressing from `l6` through `l9` to `l3` through `l6`. Transcript scroll state correctly remains `(10, false)`, isolating the defect to composer navigation and rendering.
## Reproduction steps

1. Run:

   ```text
   cargo test -p cowboy app::controls::composer::tests::nonempty_composer_navigation_matches_omp_up_and_page_up_behavior -- --exact
   ```

2. The test submits the synthetic history value `from history`, enters `alpha\nbravo\ncharl`, and sends `Up`.
3. In a fresh state, the test creates a scrollable synthetic transcript, scrolls it to offset `10`, and enters `l0` through `l9` as a ten-line draft.
4. The test fixes composer width at `20` and terminal height at `9`. Cowboy assigns the composer a total height of `6`, leaving exactly four bordered content rows. OMP's page step is therefore `visible_height - 1 = 3` visual rows.
5. The test renders before paging, sends `PageUp` twice, and renders after each key. The exact OMP cursor targets are line `l6` at global position `20`, then line `l3` at global position `11`. The expected viewport progresses from `l6`–`l9` to `l3`–`l6` while the transcript remains at offset `10` with follow mode disabled.
6. Observe that Cowboy replaces the non-empty draft on `Up`, leaves the page cursor at position `29`, and repeatedly renders only the hidden-lines marker plus `l7`–`l9`.
7. Re-running the command produces the same failure in under one second after compilation.
## Regression test

- Exact test file path: `crates/tui/app/src/app/controls/composer.rs`
- Exact test name: `app::controls::composer::tests::nonempty_composer_navigation_matches_omp_up_and_page_up_behavior`
- Exact command: `cargo test -p cowboy app::controls::composer::tests::nonempty_composer_navigation_matches_omp_up_and_page_up_behavior -- --exact`
- Expected failure before the fix: `Up` yields `(Continue, "from history", 12)` instead of preserving the draft at cursor position `11`; the controlled composer height is correctly `6`, but two `PageUp` presses leave the cursor at position `29` instead of the exact OMP targets `20` and `11`; rendering remains on `[hidden marker, l7, l8, l9]` instead of exposing `[l6, l7, l8, l9]` and then `[l3, l4, l5, l6]`. The draft and transcript state remain unchanged. The process exits with code `101` and reports one failed test.

The single test drives Cowboy's actual key dispatcher and composer renderer. Exact cursor positions reject one-character or one-row pseudo-fixes. Fixed dimensions and exact row assertions verify the cursor-following viewport. Before/after transcript tuples verify that input paging does not mutate transcript scrolling.
## Current failing result

The strengthened narrow command was run twice and failed identically. Captured result:

```text
running 1 test
failures:

---- app::controls::composer::tests::nonempty_composer_navigation_matches_omp_up_and_page_up_behavior stdout ----

assertion `left == right` failed
  left: ((Continue, "from history", 12), 6, ["> … 7 earlier line(s) hidden", "> l7", "> l8", "> l9"], Continue, 29, ["> … 7 earlier line(s) hidden", "> l7", "> l8", "> l9"], Continue, 29, ["> … 7 earlier line(s) hidden", "> l7", "> l8", "> l9"], "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9", (10, false), (10, false))
 right: ((Continue, "alpha\nbravo\ncharl", 11), 6, ["> l6", "> l7", "> l8", "> l9"], Continue, 20, ["> l6", "> l7", "> l8", "> l9"], Continue, 11, ["> l3", "> l4", "> l5", "> l6"], "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9", (10, false), (10, false))

failures:
    app::controls::composer::tests::nonempty_composer_navigation_matches_omp_up_and_page_up_behavior

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 215 filtered out

error: test failed, to rerun pass `-p cowboy --lib`
```

Exit code: `101`.
## Fix constraints

- Preserve history recall for an empty composer. Do not replace a non-empty draft merely because `Up` is pressed.
- Match OMP `16.4.5` visual-line behavior for `Up`/`Down`, including explicit newlines, soft-wrapped rows, preferred display column, Unicode grapheme boundaries, and wide characters.
- Match OMP `16.4.5` page behavior exactly: with total editor height `6` and four visible content rows, each page key moves three visual rows. The regression's ten equal-width lines must move `l9 → l6 → l3`, not merely backward by any amount.
- Retain a cursor-following composer viewport. At the controlled dimensions, the expected visible content is `l6`–`l9` at the tail and `l3`–`l6` after the second `PageUp`; a hidden-lines marker must not consume one of those four content rows.
- Keep transcript scrolling separate. Composer `PageUp`/`PageDown` must not mutate transcript follow state or transcript `scroll_offset`.
- Keep editor navigation available whenever composer edits are allowed, even when submission is temporarily disabled by an active run. Preserve the existing submission gate itself.
- Preserve current history persistence, slash completion, draft editing, wrapping, cursor rendering, and composer height limits.
- Keep the strengthened regression test unchanged during the fix; change product code so it passes.
- This investigation changes test code and documentation only. No product code was changed.
