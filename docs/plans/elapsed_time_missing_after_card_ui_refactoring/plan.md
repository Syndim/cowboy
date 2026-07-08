# Plan

Restore elapsed run time in TUI workflow card titles using the timing data already present on `WorkflowEvent`, as described in `docs/plans/elapsed_time_missing_after_card_ui_refactoring/rca.md`. Keep the investigator-added repro test `app::events::tests::run_completed_title_uses_active_elapsed_duration` as an input to the fix; do not rewrite or replace it.

Implement the fix in the presentation layer only. Workflow timing semantics, event emission, persistence, and runtime active-duration accounting are out of scope.

Expected mocked card UI after the fix, using the RCA repro event with `run_active_duration_ms = 296_000`:

```text
00:04:56 · ✓ Run completed · ▶ 170dc431
╭──────────────────────────────────────────────────────────────────────────────╮
╰──────────────────────────────────────────────────────────────────────────────╯
```

The elapsed stamp should be the first title element when timing data is available. It should appear before the status icon/title text and before the existing run/step/workflow metadata.


# Changes

- In `crates/tui/app/src/app/events.rs`, add a private elapsed-stamp helper for `WorkflowEvent`:
  - Prefer `event.run_active_duration_ms` when present.
  - Otherwise fall back to `event.timestamp - event.run_started_at` for older events.
  - Clamp negative fallback durations to zero.
  - Format as `HH:MM:SS`, with hours allowed to exceed 24.
  - Return `None` when neither active duration nor start timestamp is available.
- In `crates/tui/app/src/app/card.rs`, add compact elapsed title-prefix support, or an equivalent card API that lets event rendering put elapsed text before the status icon/title without verbose labels.
- In `crates/tui/app/src/app/events.rs`, route every workflow event card through a shared title-building helper so elapsed text is included consistently as the first title element when timing data exists, followed by the existing status/title and run/step/workflow metadata.
- Use the mocked UI above as the acceptance shape for title ordering: `00:04:56 · ✓ Run completed · ▶ 170dc431`.
- Preserve the current compact card title style: no `elapsed=`, `run=`, `step=`, or other verbose labels.
- Do not change `cowboy_workflow_engine::WorkflowEvent`, `ActiveRunClock`, runtime event emission, store schema, or workflow logic.

# Tests to be added/updated

- Keep `crates/tui/app/src/app/events.rs::app::events::tests::run_completed_title_uses_active_elapsed_duration` unchanged as the regression test proving active elapsed duration appears and wall-clock elapsed is not used when `run_active_duration_ms` is present.
- Add a focused TUI rendering test for the legacy fallback path: an event with `run_started_at` and no `run_active_duration_ms` renders the wall-clock elapsed stamp in `HH:MM:SS`.
- Add a focused TUI rendering test for negative fallback durations: an event whose timestamp is before `run_started_at` renders `00:00:00` instead of underflowing or showing a negative value.
- If any existing title assertions fail because elapsed metadata is now present for timed events, update only those expectations to include the compact elapsed stamp; keep assertions that reject verbose labels.
- Add or update assertions so at least one test verifies the mocked title ordering with elapsed time as the first element before the status/title and run metadata.

# How to verify

Run the narrow regression and new tests first:

```text
cargo test -p cowboy run_completed_title_uses_active_elapsed_duration
cargo test -p cowboy run_completed_title_falls_back_to_wall_clock_elapsed_duration
cargo test -p cowboy run_completed_title_clamps_negative_wall_clock_elapsed_duration
```

Then run the focused TUI event-rendering suite:

```text
cargo test -p cowboy app::events::tests
```

Finally run the relevant crate checks required for this Rust UI change:

```text
cargo test -p cowboy
cargo clippy -p cowboy --all-targets -- -D warnings
```

# TODO

- [x] Add a private elapsed-stamp helper in `crates/tui/app/src/app/events.rs` with active-duration preference, legacy fallback, negative clamping, and `HH:MM:SS` formatting.
- [x] Add compact elapsed title-prefix support to `crates/tui/app/src/app/card.rs` without reintroducing verbose metadata labels.
- [x] Thread elapsed text through workflow card title construction as the first title element when event timing data is available.
- [x] Match the mocked title ordering in the plan: elapsed stamp, status/title, then existing run/step/workflow metadata.
- [x] Preserve the investigator-added repro test unchanged and make it pass by fixing product code.
- [x] Add fallback elapsed rendering coverage for events with `run_started_at` but no `run_active_duration_ms`.
- [x] Add negative fallback clamping coverage for timestamps before `run_started_at`.
- [x] Run the narrow regression tests and focused TUI event tests.
- [x] Run the relevant `cowboy` crate test and Clippy checks, then fix any warnings or failures introduced by the change.
