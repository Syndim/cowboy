## Bug behavior

When one Cowboy instance holds the workflow database lock for longer than the store's retry window, a concurrent instance fails instead of waiting for the finite contention to clear. The surfaced error is:

```text
recoverable action failure: workflow store "<workflow-store-path>" is temporarily busy; another Cowboy instance is using it
```

The workflow runner can then persist or report the run as failed and direct the user to resolution, even though the database becomes available shortly afterward. The supplied report's private store path and run identifier are intentionally omitted.

## Root cause

Cowboy uses redb 4.1.0. redb obtains a non-blocking exclusive file lock when a writable `Database` is opened and returns `DatabaseAlreadyOpen` when another handle owns that lock.

`RedbRunStore` is path-backed and opens a new redb `Database` inside each operation. `open_database_with_retry` retries `DatabaseAlreadyOpen` 20 times with a fixed 25 ms sleep. The resulting contention allowance is approximately 500 ms. If the current owner releases the valid lock after that fixed window, the competing operation has already returned `Error::TemporarilyBusy`.

Run execution locks do not prevent this collision: they are keyed by run ID, so separate runs using the same workflow database may execute concurrently. `Error::TemporarilyBusy` is converted to `WorkflowError::RecoverableAction`; the runner's retry reservation and failure-status persistence also use the same contended store. Consequently, database coordination failure escapes through workflow action error handling and can fail the run.

The existing `live_store_value_does_not_keep_database_locked` test passes, ruling out an idle `RedbRunStore` value retaining the database handle. The defect is the fixed, short handling window for a live exclusive redb lock.

## Reproduction steps

1. Create an isolated temporary workflow database path.
2. Open that path directly as a writable redb `Database`, which acquires the same exclusive lock Cowboy uses.
3. Signal that the lock is held and retain the handle for 750 ms.
4. Concurrently call `RedbRunStore::create` for the same path.
5. Observe that Cowboy exhausts its approximately 500 ms retry allowance and returns `TemporarilyBusy` before the first handle releases the lock.

This is the minimum deterministic reproduction: one real redb lock owner, one Cowboy store opener, and a hold duration beyond the fixed retry window.

## Regression test

- Test file: `crates/workflow/store/src/redb_store.rs`
- Test name: `redb_store::tests::transient_database_contention_outlasting_retry_window_does_not_fail`
- Command: `cargo test -p cowboy-workflow-store transient_database_contention_outlasting_retry_window_does_not_fail -- --nocapture`
- Expected failure before the fix: the final `expect` panics with `TemporarilyBusy("<temporary-database-path>")` because the second opener gives up before the finite 750 ms contention ends. After the fix, the same call must wait for release and return `Ok(RedbRunStore)`.

## Current failing result

The focused command was run twice and failed deterministically both times. One run produced exit code 101:

```text
running 1 test
thread 'redb_store::tests::transient_database_contention_outlasting_retry_window_does_not_fail' panicked at crates/workflow/store/src/redb_store.rs:947:15:
transient database contention should wait for the lock to be released: TemporarilyBusy("<temporary-database-path>")
test redb_store::tests::transient_database_contention_outlasting_retry_window_does_not_fail ... FAILED

failures:
    redb_store::tests::transient_database_contention_outlasting_retry_window_does_not_fail

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 19 filtered out; finished in 0.76s

error: test failed, to rerun pass `-p cowboy-workflow-store --lib`
```

## Fix constraints

- Do not weaken redb's exclusive-access guarantee or permit concurrent writable handles.
- Finite workflow-store contention longer than the current approximately 500 ms window must not fail a run; the regression test must pass without reducing its 750 ms lock duration.
- Database availability handling must not consume or exhaust workflow action retry budgets, because lock acquisition is storage coordination rather than action execution.
- Preserve the path-backed store property verified by `live_store_value_does_not_keep_database_locked`: idle Cowboy instances must not retain the redb lock.
- Preserve existing stored data and the current `RunStore` behavioral contract.
- The investigation changed only the focused test and this RCA; product code remains unchanged until fix planning and implementation.
