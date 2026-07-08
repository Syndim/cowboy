# Bug behavior

Workflow transcript card titles no longer show elapsed run time. A workflow event that carries `run_active_duration_ms = 296_000` should visibly render the active elapsed stamp `00:04:56`, but the card title currently renders only the status icon, event title, and compact metadata.

# Root cause

The card UI refactor changed TUI event rendering from the old `header_line` path, which prepended an `elapsed_stamp(event)` value, to `workflow_event_card(event)` plus `Card::title_line`. The current card title builder in `crates/tui/app/src/app/card.rs` joins only the status icon, title, and `CardMetadata` values. `crates/tui/app/src/app/events.rs` no longer computes or passes the elapsed stamp, so the active-duration data still present on `WorkflowEvent` is dropped at presentation time.

# Reproduction steps

1. Add the focused regression test `app::events::tests::run_completed_title_uses_active_elapsed_duration` in `crates/tui/app/src/app/events.rs`.
2. Run the narrow test command:

```text
cargo test -p cowboy run_completed_title_uses_active_elapsed_duration
```

# Regression test

- Test file path: `crates/tui/app/src/app/events.rs`
- Test name: `app::events::tests::run_completed_title_uses_active_elapsed_duration`
- Command: `cargo test -p cowboy run_completed_title_uses_active_elapsed_duration`
- Expected failure before the fix: the rendered card title does not contain `00:04:56` even though the event carries `run_active_duration_ms = 296_000`.

# Current failing result

```text
running 1 test
failures:

---- app::events::tests::run_completed_title_uses_active_elapsed_duration stdout ----

thread 'app::events::tests::run_completed_title_uses_active_elapsed_duration' panicked at crates/tui/app/src/app/events.rs:474:9:
✓ Run completed · ▶ 170dc431
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::events::tests::run_completed_title_uses_active_elapsed_duration

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 158 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

# Fix constraints

- Do not change workflow timing semantics; use `run_active_duration_ms` when present and keep the legacy `run_started_at`/`timestamp` fallback for older events.
- Keep elapsed formatting as `HH:MM:SS`, with hours allowed beyond 24 and negative fallback elapsed clamped to zero.
- Restore elapsed display within the card UI without reintroducing verbose metadata labels that the card refactor intentionally removed.
- Keep the regression test focused on TUI rendering; no product code should be changed during this investigation.
