# Shorten local time display to hour:minute without timezone

## Plan

Workflow event cards prefix every title with a local wall-clock timestamp. Today
that prefix is built in `crates/tui/app/src/app/events.rs` by
`format_workflow_title_prefix`, which formats the local time with
`"%H:%M:%S %:z"` — for example `06:23:45 -07:00`. Users report this is too long.

Cumulative user direction, applied in order:

1. "shorten the local time display ... show the timezone in a shorter format"
2. "only show hour:minute and remove the timezone info"

The second direction supersedes the first. Final target format is `"%H:%M"`
(e.g. `06:23`): drop the seconds field and drop the timezone offset entirely.

Scope decision (single, minimal change):

- Only the wall-clock local time is shortened. The elapsed run-duration suffix
  `(+HH:MM:SS)` is a stopwatch, not a local time, so it is intentionally left
  unchanged. Changing it would exceed the stated request.
- The UTC-to-local conversion is preserved. `format_workflow_title_prefix`
  still receives a local `DateTime<FixedOffset>`; we simply stop rendering the
  offset. This keeps the card showing the viewer's local hour and minute.
- There is exactly one render site for this timestamp (verified by searching
  `crates/tui/app/src` for `%H` / `%:z` / `with_timezone`), so no other display
  path needs editing.

## Changes

- `crates/tui/app/src/app/events.rs`
  - `format_workflow_title_prefix` (around line 396): change the format string
    from `"%H:%M:%S %:z"` to `"%H:%M"`. No signature or control-flow change; the
    `Some(elapsed_ms)` branch keeps appending ` (+{elapsed})` and the `None`
    branch keeps returning the bare wall clock.

No changes to `format_elapsed_ms`, the card layout code, or any other crate.

## Tests to be added/updated

All in the `mod tests` block of `crates/tui/app/src/app/events.rs`. Two of the
three tests below contain assertions that hard-code the old `HH:MM:SS ±HH:MM`
shape and must be updated; the third (narrow-width) test does not assert the
timestamp shape and is only conditionally affected by the horizontal space the
shorter prefix frees. No new test file is needed because the existing tests
already cover this contract.

- `formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed`
  (around lines 589-590):
  - Change `assert_eq!(prefix, "06:23:45 -07:00 (+00:04:56)")` to
    `assert_eq!(prefix, "06:23 (+00:04:56)")`.
  - Replace the UTC-leak guard `assert!(!prefix.contains("13:23:45 +00:00"))`
    with `assert!(!prefix.contains("13:23"))` so it still proves the local
    (`06:23`), not UTC (`13:23`), hour/minute is rendered, and add
    `assert!(!prefix.contains("-07:00"))` to prove the offset is gone.
- `run_completed_title_omits_elapsed_when_no_elapsed_source_exists`
  (around line 669):
  - Change `assert_eq!(prefix.len(), "06:23:45 -07:00".len())` to
    `assert_eq!(prefix.len(), "06:23".len())`.
- `narrow_workflow_title_keeps_event_title_ahead_of_metadata`
  (around lines 689-701):
  - This test asserts that at width 36 the run-id metadata (`▶ 170dc431`) is
    truncated away. The shortened prefix frees roughly 10 columns, which may let
    the run-id fit again and break `assert!(!narrow_title.contains("▶ 170dc431"))`.
    If so, lower the narrow render width (the `36` argument) until the metadata
    is dropped again, preserving the test's intent (narrow title keeps the event
    title ahead of trailing metadata). Do not weaken the assertion itself.

## How to verify

- Unit tests: run the events test module and confirm all pass.
  `cargo test -p cowboy app::events::tests`
- Lints/build for the touched crate: `cargo clippy -p cowboy --all-targets` is
  clean (no new warnings), `cargo build -p cowboy` succeeds.
- Manual smoke test (deterministic, non-mutating): launch the TUI against the
  demo config with `cargo run -p cowboy -- --config demo-config.toml`, submit
  `/run --workflow 00-demo timestamp smoke` (a status-only workflow that needs
  no agent backend and does not touch the working tree), answer the `Apply the
  plan?` prompt with `yes`, and confirm each emitted card title begins with a
  `HH:MM` timestamp (five characters, e.g. `06:23`) with no seconds and no
  `±HH:MM` / timezone token, while the ` (+HH:MM:SS)` elapsed suffix still
  appears on lifecycle cards.

## TODO

- [x] TODO-01: Change the wall-clock format string in `format_workflow_title_prefix` from `"%H:%M:%S %:z"` to `"%H:%M"` in `crates/tui/app/src/app/events.rs`.
  - Procedure: Edit the `local_timestamp.format(...)` call in
    `format_workflow_title_prefix` (around line 396) to use `"%H:%M"`. Then run
    `cargo build -p cowboy`.
  - Expected result: Build succeeds. The function now yields prefixes like
    `06:23` (no elapsed) or `06:23 (+00:04:56)` (with elapsed).
  - Observed result: Edited line 396 to `local_timestamp.format("%H:%M")`.
    `cargo build -p cowboy` finished successfully (dev profile, 2.21s). The
    with-elapsed unit assertion now expects `06:23 (+00:04:56)` and the bare
    prefix is `06:23`.

- [x] TODO-02: Update the wall-clock assertions in the three affected unit tests to expect the `HH:MM` shape.
  - Procedure: In `crates/tui/app/src/app/events.rs` `mod tests`, update
    `formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed`
    (expect `"06:23 (+00:04:56)"`; guard `!contains("13:23")` and
    `!contains("-07:00")`) and
    `run_completed_title_omits_elapsed_when_no_elapsed_source_exists`
    (expect `prefix.len() == "06:23".len()`). Then run
    `cargo test -p cowboy app::events::tests`.
  - Expected result: Both updated tests pass (`cargo test -p cowboy
    app::events::tests` exits 0). No positive/expected prefix value asserts
    seconds or a timezone offset: the elapsed case expects exactly
    `06:23 (+00:04:56)` and the no-elapsed case expects a prefix of length
    `"06:23".len()` (5 characters). The negative guards that prove the rendered
    prefix dropped the UTC hour leak and the offset are required and retained —
    `!prefix.contains("13:23")` and `!prefix.contains("-07:00")`.
  - Observed result: `cargo test -p cowboy app::events::tests` exited 0 with 29
    passed, 0 failed.
    `formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed` now
    asserts the exact prefix `"06:23 (+00:04:56)"` plus the retained negative
    guards `!prefix.contains("13:23")` (UTC-hour leak) and
    `!prefix.contains("-07:00")` (offset removed);
    `run_completed_title_omits_elapsed_when_no_elapsed_source_exists` asserts the
    five-character bare prefix `assert_eq!(prefix.len(), "06:23".len())`. No
    positive/expected assertion references `06:23:45`, `-07:00`, or a
    15-character length.

- [x] TODO-03: Re-run the events test module and, if the narrow-width test regresses, lower its render width so run-id metadata is still truncated.
  - Procedure: Run `cargo test -p cowboy app::events::tests`. If
    `narrow_workflow_title_keeps_event_title_ahead_of_metadata` fails because
    `▶ 170dc431` now fits, reduce the `36` width argument in that test until the
    run-id is dropped, keeping both existing assertions
    (`narrow_title.contains("◔ Waiting for input")` and
    `!narrow_title.contains("▶ 170dc431")`).
  - Expected result: The full `app::events::tests` module passes, including the
    narrow-width truncation test with its original intent intact.
  - Observed result: `cargo test -p cowboy app::events::tests` reported 29
    passed, 0 failed. `narrow_workflow_title_keeps_event_title_ahead_of_metadata`
    passed unchanged at width `36` (both `contains("◔ Waiting for input")` and
    `!contains("▶ 170dc431")` still hold), so no width reduction was needed.

- [x] TODO-04: Confirm no new warnings and smoke-test the rendered card timestamp.
  - Procedure: (step 1) Run `cargo clippy -p cowboy --all-targets`. (step 2)
    Launch the TUI with exactly `cargo run -p cowboy -- --config demo-config.toml`
    — do not substitute the prebuilt `target/debug/cowboy` binary; the command
    recorded for procedure step 2 must be this exact `cargo run` invocation. In
    the TUI, submit `/run --workflow 00-demo timestamp smoke` (the status-only
    `00-demo` workflow needs no agent backend and does not modify the working
    tree; use the workflow id shown by `/workflows` if it differs). (step 3) When
    the card shows the `Apply the plan?` prompt, type `yes` and press Enter, then
    inspect the emitted plan, confirm, decide, apply, and Run completed cards.
  - Expected result: Clippy reports no new warnings. Every emitted card title
    starts with a `HH:MM` local time (five characters, no seconds, no timezone
    offset), and any lifecycle card carrying elapsed time still shows the
    ` (+HH:MM:SS)` suffix.
  - Observed result: `cargo clippy -p cowboy --all-targets` finished with no
    warnings (0 warning/error lines). Launched the TUI with exactly
    `cargo run -p cowboy -- --config demo-config.toml` (recorded at procedure
    step 2, not the prebuilt binary), submitted
    `/run --workflow 00-demo timestamp smoke`, and answered the `Apply the plan?`
    prompt with `yes`. Every emitted card (plan, confirm, decide, apply, and
    `✓ Run completed`) rendered `15:50 (+00:00:02) · …` — a five-character `HH:MM`
    local time with no seconds and no `+08:00` offset, while lifecycle cards kept
    the ` (+00:00:02)` elapsed suffix. `.cowboy/demo-state/` is gitignored, so the
    working tree stayed clean (only `events.rs` modified).
