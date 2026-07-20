# Bug behavior

`/runs` and the equivalent `cowboy runs` path become slow when the store contains many persisted runs. A focused regression test with 100 completed runs, each carrying an on-disk workflow source snapshot, observed `WorkflowRuntime::list_runs(None)` taking `5.057680528s` just to produce run summaries.

The command is expected to list summary data from local persisted state without repeatedly opening the store or reading full workflow snapshots for every run.

# Root cause

`WorkflowRuntime::list_runs` first reads run heads, then performs an N+1 scan over the store:

- `crates/workflow/engine/src/runtime.rs:436-458` calls `store.list_runs()?` and then calls `store.load_run(&head.run_id)` once for every matching head.
- `crates/workflow/store/src/redb_store.rs:95-108` implements `load_run` through `with_read`.
- `crates/workflow/store/src/redb_store.rs:521-542` opens the redb database for every `with_read` call.

Because `RedbRunStore` is path-backed and intentionally does not keep a database handle open between operations, the current listing path opens the database once for the head list and then once per run. It also deserializes every full `WorkflowRun`, including snapshotted workflow sources, even though `/runs` only needs summary fields.

There is a secondary slow path for legacy runs without `request_topic`: `summary_topic` in `crates/workflow/engine/src/runtime.rs:461-470` calls `load_events`, which reads and deserializes the whole per-run event log just to recover the first `RunStarted` topic.

# Reproduction steps

1. Add the regression test `list_runs_returns_many_disk_persisted_summaries_quickly` in `crates/workflow/engine/src/runtime.rs`.
2. Run the narrow test command:

```bash
cargo test -p cowboy-workflow-engine list_runs_returns_many_disk_persisted_summaries_quickly -- --nocapture
```

The test seeds 100 completed runs into a temp redb store. Each run has a persisted workflow source snapshot so the current implementation pays the full-run deserialization cost during listing.

# Regression test

- Test file path: `crates/workflow/engine/src/runtime.rs`
- Test name: `runtime::tests::list_runs_returns_many_disk_persisted_summaries_quickly`
- Command: `cargo test -p cowboy-workflow-engine list_runs_returns_many_disk_persisted_summaries_quickly -- --nocapture`
- Expected failure before the fix: the elapsed-time assertion fails because listing 100 summaries takes seconds instead of staying under the summary-listing budget.

# Current failing result

Observed command:

```bash
cargo test -p cowboy-workflow-engine list_runs_returns_many_disk_persisted_summaries_quickly -- --nocapture
```

Observed failure:

```text
running 1 test
thread 'runtime::tests::list_runs_returns_many_disk_persisted_summaries_quickly' (1207391) panicked at crates/workflow/engine/src/runtime.rs:3145:9:
listing 100 disk-persisted run summaries took 5.057680528s; /runs should read summary data without reopening the database and deserializing every full workflow snapshot
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test runtime::tests::list_runs_returns_many_disk_persisted_summaries_quickly ... FAILED

failures:

failures:
    runtime::tests::list_runs_returns_many_disk_persisted_summaries_quickly

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 123 filtered out; finished in 10.89s

error: test failed, to rerun pass `-p cowboy-workflow-engine --lib`
```

# Fix constraints

- Do not move runtime behavior into either TUI crate; keep `/runs` behavior in `cowboy-workflow-engine` and store access in `cowboy-workflow-store`.
- Avoid per-run database opens in the list path. The fix should read all required summary data in one store operation or from a denormalized summary/head record.
- Avoid deserializing full workflow source snapshots when rendering run summaries.
- Preserve existing `/runs [partial-run-id]` behavior and structured status rendering.
- Preserve legacy topic fallback behavior for old runs without `request_topic`, but avoid making the normal listing path scan every event log.
- Keep the regression test focused on the observed performance contract; it must fail before the fix and pass only when listing uses summary data efficiently.
