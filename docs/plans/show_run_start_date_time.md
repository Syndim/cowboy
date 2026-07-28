# Plan

Show each run's start date and time in both `cowboy runs` and TUI `/runs` output. Use `WorkflowRun::created_at` as the authoritative start timestamp, carry it through the existing denormalized run-head summary so normal listings remain fast, and render it through the shared `crates/tui/app/src/run_summary.rs` formatter.

Render the new field as an offset-qualified local timestamp:

```text
  started_at: 2026-07-28 13:40:44 +08:00
```

The local conversion belongs in the presentation layer, not the workflow engine. Keep run ordering, partial run-id filtering, status expansion, per-run TUI cards, and empty-state behavior unchanged.

Existing persisted heads need compatibility handling. Add the creation timestamp as an optional, serde-defaulted field in `RunHeadSummary`; newly written heads populate it from `WorkflowRun::created_at`. When an older head has summary data but lacks the new timestamp, `WorkflowRuntime::list_runs` should selectively load that matching full run to recover `created_at`. If the full run is unavailable, retain the summary and expose an absent timestamp rather than dropping the run from the listing.

# Changes

- Update `crates/workflow/core/src/state.rs`:
  - add an optional `created_at: Option<DateTime<Utc>>` field to `RunHeadSummary` with `#[serde(default)]`;
  - populate it with `Some(run.created_at)` in `RunHeadSummary::from_run`;
  - preserve deserialization of both heads with no `summary` and pre-feature summary payloads with no `created_at`.
- Update `crates/workflow/engine/src/runtime.rs`:
  - add `started_at: Option<DateTime<Utc>>` to `RunSummaryLine`;
  - project it directly from `RunHeadSummary.created_at` on the normal fast path;
  - for a summary-bearing legacy head whose timestamp is absent, load only that run and backfill `started_at` from `WorkflowRun::created_at`;
  - keep the existing full-run/event-log fallback for heads with no summary, and keep returning the summary with `started_at: None` if legacy recovery cannot load the full run.
- Update `crates/tui/app/src/run_summary.rs`:
  - add a single timestamp-formatting helper that accepts a fixed-offset datetime and formats `%Y-%m-%d %H:%M:%S %:z`;
  - convert `RunSummaryLine.started_at` from UTC to `chrono::Local` before calling the deterministic helper;
  - append `  started_at: <formatted value>` to every summary with a known timestamp, immediately after the run id and before topic/workflow details;
  - use `  started_at: <unknown>` when the timestamp is absent so legacy or damaged head data remains visibly incomplete rather than silently omitting the requested field.
- Keep `crates/tui/app/src/main.rs` and `AppState::apply_runs_list` on the existing shared `render_run_summary_lines` path so CLI and TUI output cannot drift.
- Restore `crates/tui/app/src/app/controls/chrome.rs` to its pre-feature status-rendering behavior:
  - remove the special case that appends `state.status()` when it matches `<number> run(s)`;
  - remove the dedicated `run_list_count_is_visible_in_status_metadata` test;
  - do not otherwise alter status-strip metadata composition or tests.
- Update all `RunSummaryLine` test fixtures affected by the new public field; do not change command grammar, persistence schema, run ordering, or filter semantics.

# Tests to be added/updated

- Update `crates/workflow/core/src/state.rs::tests::run_head_defaults_legacy_summary_and_builds_from_run`:
  - verify a pre-feature `RunHeadSummary` without `created_at` deserializes with `None`;
  - verify `RunHead::from_run` stores the exact `WorkflowRun::created_at`.
- Update engine listing tests in `crates/workflow/engine/src/runtime.rs`:
  - assert normal summary-bearing heads expose the exact creation timestamp;
  - keep `list_runs_reads_persisted_head_summaries_without_full_runs` as the fast-path contract and assert the timestamp comes from the head-only payload;
  - add a legacy-summary case with no `created_at` that proves only the matching full run is used to recover the timestamp;
  - add or update a missing-full-run case proving the run still appears with `started_at: None`;
  - preserve current assertions for partial run-id filtering, topics, status detail, current step, and head step.
- Update `crates/tui/app/src/run_summary.rs` unit tests:
  - pass a deterministic non-UTC `FixedOffset` timestamp and assert the exact `started_at: 2026-07-28 13:40:44 +08:00` line;
  - prove the formatter does not accidentally display the UTC wall clock;
  - assert summaries with no timestamp render `started_at: <unknown>`;
  - preserve the existing structured completed, waiting, and failed status assertions and Rust-debug leak guards.
- Update TUI `/runs` tests in `crates/tui/app/src/app/commands.rs` so each seeded run card contains its own `started_at` line while filtered and empty results retain their current behavior.
- Update `crates/tui/app/tests/runs_cli.rs::cli_runs_filters_by_partial_run_id` so filtered CLI output contains exactly one `started_at` line for the matching run and still excludes the nonmatching run id.
- Remove `crates/tui/app/src/app/controls/chrome.rs::tests::run_list_count_is_visible_in_status_metadata`; no replacement status-strip test should be added because visible run-count metadata is outside this feature.

# How to verify

Run the focused tests from the repository root:

```bash
cargo test -p cowboy-workflow-core run_head_defaults_legacy_summary_and_builds_from_run
cargo test -p cowboy-workflow-engine list_runs
cargo test -p cowboy run_summary::tests
cargo test -p cowboy runs_submission
cargo test -p cowboy --test runs_cli cli_runs_filters_by_partial_run_id
cargo fmt --check
cargo clippy -p cowboy-workflow-core -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings
```

Then perform this reproducible cross-interface smoke check from the repository root in one Bash shell:

1. Build the product binary and create a clean, isolated workflow/config/state tuple under `target/`:

   ```bash
   cargo build -p cowboy
   export TZ=UTC
   SMOKE_DIR="$PWD/target/run-start-date-time-smoke"
   rm -rf "$SMOKE_DIR"
   mkdir -p "$SMOKE_DIR/workflows" "$SMOKE_DIR/state"

   cat >"$SMOKE_DIR/workflows/instant.lua" <<'LUA'
   local start = step("start")
   start.run = function(ctx)
     return action.status { status = "success", body = ctx.request }
   end
   return workflow("instant", start)
   LUA

   cat >"$SMOKE_DIR/config.toml" <<TOML
   state_dir = "$SMOKE_DIR/state"
   workflow_store = "$SMOKE_DIR/state/data.db"
   workflow_dirs = ["$SMOKE_DIR/workflows"]

   [config_sets.default]
   max_steps_per_run = 5
   max_visits_per_step = 5
   max_retries_per_run = 0
   max_retries_per_step = 0

   [[agents]]
   name = "default"
   command = "unused-agent"
   args = []
   TOML
   ```

   Exit criterion: `target/debug/cowboy` exists, and `test -f "$SMOKE_DIR/workflows/instant.lua" -a -f "$SMOKE_DIR/config.toml"` exits successfully.

2. Create exactly one completed run with the known catalog workflow id `instant`, list it, extract its generated id, and verify the unfiltered timestamp shape:

   ```bash
   target/debug/cowboy --config "$SMOKE_DIR/config.toml" \
     run --workflow instant "record run start timestamp" \
     >"$SMOKE_DIR/run-output.txt"

   target/debug/cowboy --config "$SMOKE_DIR/config.toml" runs \
     | tee "$SMOKE_DIR/runs-unfiltered.txt"

   RUN_ID="$(grep '^run-' "$SMOKE_DIR/runs-unfiltered.txt" | head -n 1)"
   test -n "$RUN_ID"
   test "$(grep -c '^run-' "$SMOKE_DIR/runs-unfiltered.txt")" -eq 1
   test "$(grep -c '^  started_at: ' "$SMOKE_DIR/runs-unfiltered.txt")" -eq 1
   grep -Eq '^  started_at: [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2} \+00:00$' \
     "$SMOKE_DIR/runs-unfiltered.txt"
   grep '^  started_at: ' "$SMOKE_DIR/runs-unfiltered.txt" \
     >"$SMOKE_DIR/expected-started-at.txt"
   ```

   Exit criterion: every assertion exits zero; the terminal shows one run block with one line shaped like `started_at: YYYY-MM-DD HH:MM:SS +00:00`.

3. Exercise partial run-id filtering with the exact generated id and mechanically compare the timestamp:

   ```bash
   target/debug/cowboy --config "$SMOKE_DIR/config.toml" runs "$RUN_ID" \
     | tee "$SMOKE_DIR/runs-filtered.txt"

   test "$(grep -c "^$RUN_ID$" "$SMOKE_DIR/runs-filtered.txt")" -eq 1
   test "$(grep -c '^  started_at: ' "$SMOKE_DIR/runs-filtered.txt")" -eq 1
   grep '^  started_at: ' "$SMOKE_DIR/runs-filtered.txt" \
     >"$SMOKE_DIR/filtered-started-at.txt"
   diff -u "$SMOKE_DIR/expected-started-at.txt" "$SMOKE_DIR/filtered-started-at.txt"
   printf 'Expected TUI line: '
   cat "$SMOKE_DIR/expected-started-at.txt"
   ```

   Exit criterion: `diff` has no output and exits zero, proving filtered and unfiltered CLI listings display the identical persisted start timestamp.

4. Launch the TUI against the same isolated config:

   ```bash
   target/debug/cowboy --config "$SMOKE_DIR/config.toml" tui
   ```

   Perform these ordered actions:

   1. Wait for the composer to accept input.
   2. Type `/runs` exactly and press `Enter`.
   3. Wait until the `Run` transcript card appears; do not require or inspect status-strip text.
   4. Confirm the card contains the same run id printed in step 2.
   5. Compare the card's complete `started_at:` line character-for-character with the `Expected TUI line:` printed in step 3.
   6. Confirm the same card still contains `workflow: instant`, `current_step: start`, and `status: completed`.
   7. Type `/exit` exactly and press `Enter` to exit. Plain `q` intentionally remains composer input.

   Exit criterion: one run card is displayed; its `started_at` line exactly matches `expected-started-at.txt`; the existing workflow, step, and run-status fields inside the card remain present; the TUI exits normally. Status-strip content is not part of this criterion.

# TODO

- [x] TODO-01: Add the run creation timestamp to the denormalized `RunHeadSummary` with legacy serialization compatibility.
  - Procedure: Edit `crates/workflow/core/src/state.rs` to add serde-defaulted `created_at: Option<DateTime<Utc>>`, populate it in `RunHeadSummary::from_run`, update the existing legacy/from-run unit test, and run `cargo test -p cowboy-workflow-core run_head_defaults_legacy_summary_and_builds_from_run`.
  - Expected result: The focused core test passes; old summary JSON without `created_at` yields `None`; `RunHead::from_run` stores `Some(run.created_at)`.
  - Implementer observed result: The focused test passed (1 passed, 0 failed); the test verified both legacy summary forms and exact propagation of `run.created_at`.

- [x] TODO-02: Expose the start timestamp from `WorkflowRuntime::list_runs` without slowing the normal head-only path.
  - Procedure: Add `started_at: Option<DateTime<Utc>>` to `RunSummaryLine`, project new-head timestamps directly, selectively load full runs only for legacy summaries missing the field, retain summaries when recovery fails, update engine fixtures/tests, and run `cargo test -p cowboy-workflow-engine list_runs -- --nocapture`.
  - Expected result: All focused listing tests pass; new heads return timestamps without full-run records; legacy summaries recover timestamps when a full run exists; unrecoverable legacy summaries remain listed with `started_at: None`.
  - Implementer observed result: After correcting the `DateTime` import exposed by the first compile attempt, the focused listing suite passed (6 passed, 0 failed), covering the head-only path, legacy recovery, and missing-full-run retention.

- [x] TODO-03: Render a shared offset-qualified local `started_at` line for CLI and TUI run summaries.
  - Procedure: Add the deterministic fixed-offset formatter and local conversion in `crates/tui/app/src/run_summary.rs`, place the new line directly after the run id, render `<unknown>` for `None`, update all local `RunSummaryLine` fixtures, and run `cargo test -p cowboy run_summary::tests`.
  - Expected result: The focused renderer tests pass; a fixed `+08:00` input renders exactly `started_at: 2026-07-28 13:40:44 +08:00`; unknown timestamps are explicit; existing status lines remain unchanged.
  - Implementer observed result: The focused renderer suite passed (5 passed, 0 failed), including exact `+08:00` formatting, local conversion, explicit `<unknown>`, and unchanged structured status coverage.

- [x] TODO-04: Extend `/runs` and `cowboy runs` regression coverage for the new field.
  - Procedure: Set deterministic creation timestamps in the seeded runs used by `crates/tui/app/src/app/commands.rs`, update card assertions, extend `crates/tui/app/tests/runs_cli.rs`, then run `cargo test -p cowboy runs_submission` and `cargo test -p cowboy --test runs_cli cli_runs_filters_by_partial_run_id`.
  - Expected result: Both commands pass; every matching run summary/card has one `started_at` line; filtered output excludes nonmatching runs; zero-run output remains unchanged.
  - Implementer observed result: Both focused commands passed (5 `/runs` tests and 1 CLI integration test); seeded run cards contain one known timestamp, filtering still excludes nonmatches, and empty-state tests remain unchanged.

- [x] TODO-05: Complete formatting, linting, and cross-interface smoke verification.
  - Procedure: First remove the out-of-scope status rendering introduced during implementation: in `crates/tui/app/src/app/controls/chrome.rs`, delete the `state.status().strip_suffix(" run(s)")` block from `status_metadata_text` and delete the `run_list_count_is_visible_in_status_metadata` test. Confirm the corrective diff contains no other `chrome.rs` changes with `git diff -- crates/tui/app/src/app/controls/chrome.rs`. Then run the following exact commands in order. Plain `q` intentionally remains composer input.

    ```bash
    cargo fmt --check
    cargo clippy -p cowboy-workflow-core -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings
    ```

    ```bash
    set -e
    cargo build -p cowboy
    export TZ=UTC
    SMOKE_DIR="$PWD/target/run-start-date-time-smoke"
    rm -rf "$SMOKE_DIR"
    mkdir -p "$SMOKE_DIR/workflows" "$SMOKE_DIR/state"
    cat >"$SMOKE_DIR/workflows/instant.lua" <<'LUA'
    local start = step("start")
    start.run = function(ctx)
      return action.status { status = "success", body = ctx.request }
    end
    return workflow("instant", start)
    LUA
    cat >"$SMOKE_DIR/config.toml" <<TOML
    state_dir = "$SMOKE_DIR/state"
    workflow_store = "$SMOKE_DIR/state/data.db"
    workflow_dirs = ["$SMOKE_DIR/workflows"]

    [config_sets.default]
    max_steps_per_run = 5
    max_visits_per_step = 5
    max_retries_per_run = 0
    max_retries_per_step = 0

    [[agents]]
    name = "default"
    command = "unused-agent"
    args = []
    TOML
    test -x target/debug/cowboy
    test -f "$SMOKE_DIR/workflows/instant.lua" -a -f "$SMOKE_DIR/config.toml"
    target/debug/cowboy --config "$SMOKE_DIR/config.toml" run --workflow instant "record run start timestamp" >"$SMOKE_DIR/run-output.txt"
    target/debug/cowboy --config "$SMOKE_DIR/config.toml" runs | tee "$SMOKE_DIR/runs-unfiltered.txt"
    RUN_ID="$(grep '^run-' "$SMOKE_DIR/runs-unfiltered.txt" | head -n 1)"
    test -n "$RUN_ID"
    test "$(grep -c '^run-' "$SMOKE_DIR/runs-unfiltered.txt")" -eq 1
    test "$(grep -c '^  started_at: ' "$SMOKE_DIR/runs-unfiltered.txt")" -eq 1
    grep -Eq '^  started_at: [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2} \+00:00$' "$SMOKE_DIR/runs-unfiltered.txt"
    grep '^  started_at: ' "$SMOKE_DIR/runs-unfiltered.txt" >"$SMOKE_DIR/expected-started-at.txt"
    target/debug/cowboy --config "$SMOKE_DIR/config.toml" runs "$RUN_ID" | tee "$SMOKE_DIR/runs-filtered.txt"
    test "$(grep -c "^$RUN_ID$" "$SMOKE_DIR/runs-filtered.txt")" -eq 1
    test "$(grep -c '^  started_at: ' "$SMOKE_DIR/runs-filtered.txt")" -eq 1
    grep '^  started_at: ' "$SMOKE_DIR/runs-filtered.txt" >"$SMOKE_DIR/filtered-started-at.txt"
    diff -u "$SMOKE_DIR/expected-started-at.txt" "$SMOKE_DIR/filtered-started-at.txt"
    { sleep 1; printf '/runs\033[13u'; sleep 5; printf '/exit\033[13u'; } | TERM=xterm-256color script -qefc "stty rows 40 cols 120; exec target/debug/cowboy --config '$SMOKE_DIR/config.toml' tui" "$SMOKE_DIR/tui-reviewer-smoke.txt"
    EXPECTED_LINE="$(cat "$SMOKE_DIR/expected-started-at.txt")"
    ACTUAL_LINE="$(grep -aoE '  started_at: [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2} \+00:00' "$SMOKE_DIR/tui-reviewer-smoke.txt" | tail -n 1)"
    test "$ACTUAL_LINE" = "$EXPECTED_LINE"
    test "$(grep -aoF "$RUN_ID" "$SMOKE_DIR/tui-reviewer-smoke.txt" | wc -l)" -eq 1
    test "$(grep -aoF '  started_at: ' "$SMOKE_DIR/tui-reviewer-smoke.txt" | wc -l)" -eq 1
    grep -aqF 'workflow: instant' "$SMOKE_DIR/tui-reviewer-smoke.txt"
    grep -aqF 'current_step: start' "$SMOKE_DIR/tui-reviewer-smoke.txt"
    grep -aqF 'status: completed' "$SMOKE_DIR/tui-reviewer-smoke.txt"
    printf 'Verified clean TUI timestamp line: %s\n' "$ACTUAL_LINE"
    ```
  - Expected result: `git diff -- crates/tui/app/src/app/controls/chrome.rs` shows no feature-related change after the corrective removal; formatting and Clippy exit zero with no warnings; every shell assertion and `diff` exits zero; unfiltered CLI, filtered CLI, and TUI each show the same single `started_at: YYYY-MM-DD HH:MM:SS +00:00` value for the generated run; the TUI shows exactly one run card with the expected workflow, step, and completed run-status fields and exits normally. No assertion depends on visible status-strip text.
  - Implementer observed result: The corrective `chrome.rs` diff was empty; formatting and Clippy exited zero with no warnings; the clean-state CLI and TUI smoke displayed the identical `  started_at: 2026-07-28 09:48:04 +00:00` line, preserved the run id, workflow, current step, and completed run-status fields, and exited normally through `/exit` without inspecting status metadata.
