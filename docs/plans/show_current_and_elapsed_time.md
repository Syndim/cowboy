# Plan

Show each workflow transcript event with a compact local wall-clock timestamp and its run elapsed time in the card title prefix. The wall-clock value should be the event timestamp converted to the user's local timezone, not UTC and not a continuously ticking clock. The elapsed value should continue using the existing active run duration when `run_active_duration_ms` is present, falling back to `event.timestamp - event.run_started_at` when needed.

Use one readable prefix before the existing card title: `HH:MM:SS ±HH:MM (+HH:MM:SS)`, for example `09:41:12 -07:00 (+00:04:56) · ✓ Run completed · ▶ 170dc431`. The numeric timezone offset makes the local-time guarantee explicit without relying on ambiguous abbreviations. The leading `+` and parentheses distinguish elapsed duration from wall-clock time. Keep the existing middle-dot separator, title text, metadata, card tone, body rendering, and tool coalescing behavior unchanged.

Make the local-time formatting testable through a deterministic non-UTC seam before wiring it to `chrono::Local`. A UTC-only formatter must fail the focused renderer/formatter test even when CI or the reviewer's shell timezone is UTC. Legacy or synthetic events that lack both `run_active_duration_ms` and `run_started_at` should still show the local wall-clock time and omit the elapsed parenthetical rather than inventing a fake elapsed duration. Events with `run_started_at` but no active duration should keep the existing nonnegative wall-clock elapsed fallback and clamp clock-skew negatives to `00:00:00`.

# Changes

- Update `crates/tui/app/src/app/events.rs`:
  - replace `elapsed_stamp(event)` usage with a helper that builds a local-time title prefix;
  - extract a pure formatting seam that accepts an already-localized `chrono::DateTime<chrono::FixedOffset>` plus optional elapsed milliseconds, so tests can pass a fixed non-UTC offset without depending on the process timezone;
  - in production, convert `event.timestamp` from UTC to `chrono::Local`, freeze that localized value to a fixed offset for the seam, and format as `%H:%M:%S %:z`;
  - append `(+{elapsed})` only when elapsed duration can be computed from `run_active_duration_ms` or `run_started_at`;
  - keep `format_elapsed_ms` as the single elapsed duration formatter with hours allowed to exceed `23`;
  - keep the existing negative elapsed clamp for `event.timestamp - run_started_at` fallback.
- Update the existing `workflow_card` construction so every workflow event card receives the new combined prefix through `Card::title_prefix`; do not add new card metadata fields or change `Card` layout unless tests prove truncation is unacceptable.
- Keep `crates/workflow/engine/src/events.rs`, `crates/workflow/engine/src/active_clock.rs`, and runner/runtime timing behavior unchanged unless compilation exposes an existing constructor call that must be updated. The engine already supplies `timestamp`, `run_started_at`, and `run_active_duration_ms` needed by the UI.
- Do not change diagnostic logs, persisted event JSON, workflow run timestamps, run-list summaries, command output, status strip text, or non-workflow app cards.

# Tests to be added/updated

- Update `crates/tui/app/src/app/events.rs` renderer tests that currently assert bare elapsed prefixes so they assert the combined local wall-clock plus elapsed prefix.
- Add a deterministic non-UTC unit test for the pure prefix formatter using a fixed offset such as `FixedOffset::west_opt(7 * 3600)`. With a UTC event timestamp of `2026-07-05T13:23:45Z` converted through that fixed offset, the expected prefix must include `06:23:45 -07:00 (+00:04:56)` and must not include `13:23:45 +00:00`; this test must fail a UTC-only formatter even when the process timezone is UTC.
- Add a deterministic test for an event with `run_active_duration_ms = Some(296_000)` proving the rendered title includes `(+00:04:56)` and no longer starts with the bare elapsed-only `00:04:56 ·` prefix.
- Add a test for an event with `run_active_duration_ms = None` and `run_started_at = Some(...)` proving the fallback elapsed value still renders, including the existing negative-duration clamp to `(+00:00:00)`.
- Add a test for an event with neither `run_active_duration_ms` nor `run_started_at` proving the title includes local wall-clock time and omits the elapsed parenthetical.
- Add or update one width/truncation-oriented renderer assertion if an existing test proves the longer prefix hides essential title text at normal card widths; the expected behavior should preserve the title before lower-priority metadata when truncation happens.

# How to verify

- Run `TZ=UTC cargo test -p cowboy app::events::tests::<non_utc_fixed_offset_prefix_test_name> -- --exact` after adding the deterministic non-UTC formatter test. Expected result: the test passes with an expected prefix containing `06:23:45 -07:00 (+00:04:56)` and would fail if the formatter used the UTC wall-clock `13:23:45 +00:00`.
- Run `cargo test -p cowboy app::events::tests`.
- Run `cargo test -p cowboy app::card::tests` if card truncation expectations are touched.
- Run `cargo test -p cowboy` as the focused TUI regression pass.
- Manual TUI smoke check:
  1. Start `target/debug/cowboy` in a terminal.
  2. Run a simple workflow that emits at least two workflow transcript events over more than one second.
  3. Observe card titles such as `HH:MM:SS ±HH:MM (+00:00:00) · Run started` and later `HH:MM:SS ±HH:MM (+00:00:01) · Run completed`.
  4. Confirm the displayed numeric offset matches the terminal's local timezone, elapsed duration increases across events, and card titles/bodies/metadata remain readable. This smoke check is not the only local-time proof; the fixed-offset automated test is the falsifiable proof against UTC-only formatting.

# TODO

- [x] TODO-01: Implement combined local wall-clock and elapsed title prefix in workflow event cards.
  - Procedure: Edit `crates/tui/app/src/app/events.rs`, replace the elapsed-only prefix path with a helper that converts `event.timestamp` through `chrono::Local`, freezes the localized value to `chrono::FixedOffset`, and formats it as `%H:%M:%S %:z` before appending `(+{format_elapsed_ms(...)})` when elapsed data is available.
  - Expected result: Rendering a timed `WorkflowEvent` title produces one prefix containing local wall-clock time, numeric timezone offset, and elapsed duration before the existing card title.
  - Observed result: `crates/tui/app/src/app/events.rs` now builds every workflow card title prefix from the event UTC timestamp converted through `chrono::Local` and frozen to `FixedOffset`; the deterministic formatter proof passed with `06:23:45 -07:00 (+00:04:56)`.
- [x] TODO-02: Preserve elapsed fallback and legacy event behavior.
  - Procedure: In the new helper, prefer `run_active_duration_ms`, fall back to nonnegative `event.timestamp - run_started_at`, and omit only the elapsed parenthetical when both elapsed sources are absent.
  - Expected result: Active timed events show elapsed duration, fallback timed events show clamped elapsed duration, and legacy/synthetic events still show local wall-clock time without a fabricated elapsed value.
  - Observed result: `elapsed_ms(event)` prefers `run_active_duration_ms`, falls back through `timestamp.signed_duration_since(run_started_at).num_milliseconds().max(0)`, and returns `None` when both elapsed sources are absent; event renderer tests passed for active, fallback, clamped negative, and no-elapsed legacy cases.
- [x] TODO-03: Update workflow event renderer tests for the combined prefix.
  - Procedure: Add a pure prefix formatter test that passes `FixedOffset::west_opt(7 * 3600)` and expects `06:23:45 -07:00 (+00:04:56)` for a `2026-07-05T13:23:45Z` event; update or add renderer tests in `crates/tui/app/src/app/events.rs` for active elapsed duration, wall-clock fallback duration, negative fallback clamp, and no-elapsed legacy behavior.
  - Expected result: `TZ=UTC cargo test -p cowboy app::events::tests::<non_utc_fixed_offset_prefix_test_name> -- --exact` fails for a UTC-only wall-clock formatter and passes only when the formatting seam honors the supplied non-UTC offset; `cargo test -p cowboy app::events::tests` passes after the combined-prefix behavior is implemented.
  - Observed result: Added `formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed` plus renderer tests for active elapsed, fallback elapsed, negative clamp, no-elapsed legacy, and narrow-title priority; `TZ=UTC cargo test -p cowboy app::events::tests::formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed -- --exact` and `cargo test -p cowboy app::events::tests` passed.
- [x] TODO-04: Check card width behavior with the longer prefix.
  - Procedure: Run the affected renderer tests, inspect any failing truncation assertion, and update or add a focused test only if the longer prefix changes visible title priority.
  - Expected result: Normal-width workflow card titles keep the event title readable while lower-priority metadata remains the first content allowed to truncate.
  - Observed result: Full TUI tests exposed narrow-title truncation hiding `Waiting for input`; `Card::title_line` now drops metadata, suffixes, and then prefixes before truncating the required status/title, and `narrow_workflow_title_keeps_event_title_ahead_of_metadata` plus the previously failing transcript/draw smoke tests passed.
- [x] TODO-05: Run focused automated and manual verification.
  - Procedure: Run the deterministic non-UTC test under `TZ=UTC`, run `cargo test -p cowboy app::events::tests`, run `cargo test -p cowboy`, then perform the manual TUI smoke check described in How to verify.
  - Expected result: The fixed-offset test proves a UTC-only formatter fails reproducibly; all focused tests pass; the live TUI shows local wall-clock time plus increasing elapsed time on workflow transcript cards with unchanged card body and metadata layout.
  - Observed result: Final verification passed with `TZ=UTC cargo test -p cowboy app::events::tests::formats_fixed_offset_title_prefix_with_non_utc_wall_clock_and_elapsed -- --exact`, `cargo test -p cowboy app::events::tests`, `cargo test -p cowboy`, and `cargo clippy -p cowboy --all-targets -- -D warnings`; the TUI smoke workflow displayed local `+08:00` wall-clock prefixes with elapsed values increasing from `(+00:00:13)` to `(+00:00:14)` and preserved card body/metadata layout.
