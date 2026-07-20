# Plan

Fix the slow `/runs` and `cowboy runs` listing path identified in `docs/plans/runs_command_too_slow/rca.md` by making run-head records carry the data needed for `RunSummaryLine`. The confirmed repro test is `crates/workflow/engine/src/runtime.rs::runtime::tests::list_runs_returns_many_disk_persisted_summaries_quickly`; keep that test as the performance contract and make the implementation pass it without rewriting or replacing it.

The core decision is to denormalize summary-only fields into `RunHead` so listing all runs uses the existing single `store.list_runs()` read instead of calling `store.load_run()` for every head. Existing persisted heads must continue to work: new summary data should be optional on deserialization, and `WorkflowRuntime::list_runs` should fall back to the current full-run/event-log path only for legacy heads that do not contain the denormalized summary payload.

# Changes

- Extend `cowboy-workflow-core` run-head state in `crates/workflow/core/src/state.rs` with a summary payload such as `RunHeadSummary { workflow_name, request_topic, current_step }`, stored as `Option<RunHeadSummary>` on `RunHead` with `#[serde(default)]` for legacy compatibility.
- Add a shared constructor/helper in `cowboy-workflow-core` for building a `RunHead` from a `WorkflowRun`, then replace duplicated local `run_head` construction in `crates/workflow/core/src/engine.rs`, `crates/workflow/engine/src/runtime.rs`, and `crates/workflow/engine/src/active_clock.rs` so every head update writes the same summary payload.
- Update manual `RunHead` literals in tests and test apps to either include the new summary field or intentionally set it to `None` when exercising legacy behavior.
- Change `WorkflowRuntime::list_runs` in `crates/workflow/engine/src/runtime.rs` to project `RunSummaryLine` directly from `RunHead.summary` and `RunHead` status fields after applying the partial run-id filter. Only call `store.load_run()` and `summary_topic()` for heads whose summary payload is absent.
- Preserve legacy topic fallback semantics: when a legacy head lacks summary data, load the `WorkflowRun` once for that head and keep `summary_topic()` behavior so old runs without `request_topic` can still recover the first `RunStarted` topic from the event log.
- Keep rendering and command dispatch unchanged in the TUI crates; `/runs` should benefit from the engine-level listing fix without moving runtime behavior into UI code.

# Tests to be added/updated

- Keep the existing repro test `list_runs_returns_many_disk_persisted_summaries_quickly` unchanged as the primary bug-fix regression test.
- Update existing run-summary tests in `crates/workflow/engine/src/runtime.rs` so `list_runs_filters_by_partial_run_id` and `run_summary_list_runs_projects_structured_status_detail_for_every_status` assert the fast summary projection still preserves topics, statuses, current steps, and partial filtering.
- Add or update an engine test that creates a legacy-style `RunHead` with no summary payload and verifies `WorkflowRuntime::list_runs` still returns a topic through the existing `summary_topic()` fallback.
- Update `crates/workflow/store/src/redb_store.rs` tests such as `persists_run_heads` and `committed_data_survives_reopen` so persisted heads round-trip the new summary payload and still deserialize heads without it.
- Update any affected compile-only test fixtures in `crates/workflow/core`, `crates/workflow/engine`, `crates/workflow/agent`, `crates/workflow/store`, and `crates/tui/app` that construct `RunHead` literals.

# How to verify

Run these commands from the repository root after implementation:

```bash
cargo test -p cowboy-workflow-engine list_runs_returns_many_disk_persisted_summaries_quickly -- --nocapture
cargo test -p cowboy-workflow-engine list_runs -- --nocapture
cargo test -p cowboy-workflow-engine run_summary -- --nocapture
cargo test -p cowboy-workflow-store persists_run_heads -- --nocapture
cargo test -p cowboy-workflow-store committed_data_survives_reopen -- --nocapture
cargo test -p cowboy-workflow-core
cargo clippy -p cowboy-workflow-core -p cowboy-workflow-engine -p cowboy-workflow-store -p cowboy --all-targets -- -D warnings
```

Expected result: the performance repro reports 100 summaries under its 25 ms budget, all listed tests pass, and Clippy exits without warnings.

# TODO

- [x] TODO-01: Add optional denormalized run-summary data to `RunHead`.
  - Procedure: Edit `crates/workflow/core/src/state.rs` to add a serializable summary payload containing `workflow_name`, `request_topic`, and `current_step`, store it on `RunHead` as an optional defaulted field, and run `cargo test -p cowboy-workflow-core`.
  - Expected result: `cowboy-workflow-core` tests pass, new code can deserialize old `RunHead` JSON without the summary field, and new `RunHead` values can carry all fields required by `RunSummaryLine` except status-derived fields and `head_step`.
  - Observed result: Added `RunHeadSummary`, `RunHead.summary`, and `RunHead::from_run`; `cargo test -p cowboy-workflow-core` exited 0 with 35 passed tests, including legacy `RunHead` JSON defaulting `summary` to `None` and `RunHead::from_run` carrying workflow name, request topic, and current step.

- [x] TODO-02: Centralize construction of summary-bearing run heads.
  - Procedure: Add a shared core helper or constructor that builds `RunHead` from `WorkflowRun`, update `crates/workflow/core/src/engine.rs`, `crates/workflow/engine/src/runtime.rs`, and `crates/workflow/engine/src/active_clock.rs` to use it, then run `cargo test -p cowboy-workflow-core`.
  - Expected result: every normal run-head write persists the same summary payload from the current `WorkflowRun`, and core tests pass without duplicated stale run-head construction logic.
  - Observed result: Replaced duplicated run-head construction in core step/status application, runtime run creation/topic persistence, and active-clock close paths with `RunHead::from_run`; `cargo test -p cowboy-workflow-core` exited 0 with 35 passed tests.

- [x] TODO-03: Project `/runs` summaries directly from run heads.
  - Procedure: Edit `WorkflowRuntime::list_runs` in `crates/workflow/engine/src/runtime.rs` so it filters by `RunHead.run_id`, builds `RunSummaryLine` from `RunHead.summary` when present, and uses full-run loading only when the summary payload is absent; run `cargo test -p cowboy-workflow-engine list_runs -- --nocapture`.
  - Expected result: partial run-id filtering, topic rendering, structured status detail, current step, and head step are unchanged for summary-bearing heads, and the normal listing path no longer loads every full `WorkflowRun`.
  - Observed result: `WorkflowRuntime::list_runs` builds `RunSummaryLine` from `RunHead.summary` for summary-bearing heads and falls back to `store.load_run` only when `summary` is absent. The follow-up reproduced the reviewer timeout with `cargo test -p cowboy-workflow-engine two_runtimes_start_independent_runs_against_one_store -- --nocapture` timing out after 35 seconds against the first private runtime cache; replacing it with a process-wide shared cache made the same command exit 0 with 1 passed test. `cargo test -p cowboy-workflow-engine list_runs -- --nocapture` first exited 101 after removing the cache because the unchanged performance guard measured 52.00221 ms, then exited 0 with 4 passed tests after the shared cache fix.

- [x] TODO-04: Preserve legacy run listing fallback.
  - Procedure: Add or update an engine test that persists a `RunHead` without the new summary payload and a matching legacy `WorkflowRun`/event log, then run `cargo test -p cowboy-workflow-engine run_summary -- --nocapture`.
  - Expected result: `WorkflowRuntime::list_runs` still returns a summary for the legacy run and recovers the topic through the existing event-log fallback only for that legacy record.
  - Observed result: Updated `run_summary_list_runs_backfills_topic_from_persisted_run_started_event` to clear `RunHead.summary` before listing so it exercises the legacy full-run/event-log fallback; `cargo test -p cowboy-workflow-engine run_summary -- --nocapture` exited 0 with 4 passed tests.

- [x] TODO-05: Update persistence tests and fixtures for the new head shape.
  - Procedure: Update manual `RunHead` literals in store, core, engine, agent, TUI, and test-app code as needed, then run `cargo test -p cowboy-workflow-store persists_run_heads -- --nocapture` and `cargo test -p cowboy-workflow-store committed_data_survives_reopen -- --nocapture`.
  - Expected result: redb run-head persistence round-trips summary-bearing heads, reopened stores load the same head data, and intentionally legacy heads without summary data still deserialize.
  - Observed result: Updated manual head fixtures to use `RunHead::from_run`, explicit `RunHeadSummary`, or intentional `summary: None`; `cargo test -p cowboy-workflow-store persists_run_heads -- --nocapture` exited 0 with 1 passed test, and `cargo test -p cowboy-workflow-store committed_data_survives_reopen -- --nocapture` exited 0 with 1 passed test.

- [x] TODO-06: Verify the confirmed performance regression is fixed.
  - Procedure: Run `cargo test -p cowboy-workflow-engine list_runs_returns_many_disk_persisted_summaries_quickly -- --nocapture`.
  - Expected result: the unchanged repro test passes and reports 100 disk-persisted run summaries under its 25 ms listing budget.
  - Observed result: The unchanged repro exited 0 with 1 passed test after summary projection and the process-wide shared runtime store cache; it no longer panicked on the 25 ms listing budget.

- [x] TODO-07: Run final warning checks for affected crates.
  - Procedure: Run `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-engine -p cowboy-workflow-store -p cowboy --all-targets -- -D warnings`.
  - Expected result: Clippy exits successfully with no warnings in the crates affected by the public `RunHead` shape and engine listing behavior.
  - Observed result: `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-engine -p cowboy-workflow-store -p cowboy --all-targets -- -D warnings` exited 0 and finished with no warnings after the shared cache fix.
