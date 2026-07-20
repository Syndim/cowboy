## Bug behavior

With the region-aware mouse implementation from repository commit `6bf5212`, rotating the mouse wheel while the pointer is over the composer changes the selected submitted input. Wheel-up replaces the current composer input with an older history entry; wheel-down replaces it with a newer entry. Keyboard `Up`/`Down` already provides history navigation, so mouse scrolling must leave the composer input unchanged.

The current branch contains commit `d7daba2`, which reverted the complete mouse feature. The behavioral reproduction therefore uses `6bf5212` as the pre-fix baseline and explicitly applies the investigator-added regression test to that baseline before running it.

## Root cause

`crates/tui/app/src/app/input.rs::handle_mouse_event` in `6bf5212` has two composer-specific match arms:

- `MouseEventKind::ScrollUp` inside `layout.composer` calls `state.history_previous()`.
- `MouseEventKind::ScrollDown` inside `layout.composer` calls `state.history_next()`.

Those calls directly replace `AppState.input`. Crossterm mouse capture delivers a `MouseEvent`, so this is explicit application routing rather than a terminal translating the wheel into keyboard arrow events. The regression dispatches actual `ScrollUp` and `ScrollDown` events at a coordinate inside `layout.composer`; both observed inputs are the adjacent history entries, which isolates the mutation to these handler branches.

The original feature test `composer_wheel_changes_only_guarded_input_history` treated the mutations as expected behavior. The defect was therefore encoded in both the implementation and its test expectations until the whole feature was reverted.

## Reproduction steps

From the current repository root, create a detached worktree at the mouse-enabled baseline and apply the exact investigator test patch stored with this RCA:

```text
BASELINE_DIR=/tmp/cowboy-mouse-scroll-repro
git worktree add --detach "$BASELINE_DIR" 6bf5212
git -C "$BASELINE_DIR" apply "$PWD/docs/plans/mouse_scrolling_switches_composer_input_history/regression_test.patch"
cd "$BASELINE_DIR"
cargo test -p cowboy composer_mouse_wheel_does_not_switch_input_history
```

The patch is necessary because commit `6bf5212` contains the defective mouse handler but does not contain the investigator-added test.

The test performs these behavioral steps for both vertical wheel directions:

1. Seed submitted-input history with `older input` and `newer input`.
2. Select the intended starting entry through keyboard `Up`, the supported history-navigation interaction.
3. Construct a Crossterm wheel event at the composer rectangle and dispatch it through `handle_mouse_event`.
4. Collect the composer input immediately after dispatch.
5. Compare both collected inputs with their unchanged pre-dispatch values.

Two consecutive focused runs failed identically in `0.00s` after compilation, demonstrating deterministic input mutation on the mouse-enabled baseline.

## Regression test

- Test patch: `docs/plans/mouse_scrolling_switches_composer_input_history/regression_test.patch`
- Target test file after patch application: `crates/tui/app/src/app/input.rs`
- Test name: `app::input::tests::composer_mouse_wheel_does_not_switch_input_history`
- Baseline: repository commit `6bf5212` after applying `regression_test.patch`
- Command: `cargo test -p cowboy composer_mouse_wheel_does_not_switch_input_history`
- Expected failure before the fix: the unchanged-input assertion receives `['older input', 'newer input']` after wheel-up and wheel-down, while the inputs immediately before dispatch were `['newer input', 'older input']`.

Unchanged composer input is the test's only post-dispatch contract. The test does not assert the boolean returned by `handle_mouse_event`; consuming the wheel event while preserving composer state remains valid.

The investigation checkout after `d7daba2` intentionally did not duplicate this test in its reverted `input.rs`, because that revert removed the APIs needed to compile it. `regression_test.patch` remains the retained test artifact for reproducing the defect on `6bf5212`; the implemented product branch applies the same test unchanged after restoring mouse support.

## Current failing result

On the patched `6bf5212` baseline, the focused command compiles, runs one test, and exits with code 101 because mouse dispatch changes both selected inputs:

```text
running 1 test
failures:

---- app::input::tests::composer_mouse_wheel_does_not_switch_input_history stdout ----

thread 'app::input::tests::composer_mouse_wheel_does_not_switch_input_history' panicked at crates/tui/app/src/app/input.rs:748:9:
assertion `left == right` failed: composer input changed after mouse scrolling
  left: ["older input", "newer input"]
 right: ["newer input", "older input"]

failures:
    app::input::tests::composer_mouse_wheel_does_not_switch_input_history

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 250 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

A second identical invocation produced the same left/right values and failure. `cargo fmt -p cowboy -- --check` succeeds both in the current checkout and in the patched baseline worktree.

## Fix constraints

- Preserve the composer input exactly across vertical mouse-wheel dispatch, regardless of which history entry is selected before the event.
- Do not call `history_previous()`, `history_next()`, or an equivalent input-replacement path from composer mouse-wheel routing.
- Preserve keyboard `Up`/`Down` as the submitted-input history navigation interaction.
- Do not require `handle_mouse_event` to return `false`; whether the application consumes the event is outside the reported contract.
- Keep the focused regression unchanged when evaluating a product fix against a mouse-enabled implementation.
- This investigation changes only test and investigation artifacts; it does not change product behavior.
