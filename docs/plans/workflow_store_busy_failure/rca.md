# Bug behavior

A workflow run can fail on an otherwise valid agent step when the shared redb workflow store is temporarily locked by another Cowboy process. The user-visible failure is reported as an invalid workflow action, for example `invalid action: workflow store "[state_dir]/workflow.redb" is temporarily busy; another Cowboy instance is using it`, and the failed step remains current for manual `/resolve`.

The reported instance failed at the `review` step. The absolute user state path from the report is redacted here as `[state_dir]/workflow.redb`.

# Root cause

`RedbRunStore` correctly represents exhausted redb open retries as `cowboy_workflow_store::Error::TemporarilyBusy(PathBuf)` in `crates/workflow/store/src/redb_store.rs`.

The root cause is the store-to-workflow error conversion in `crates/workflow/store/src/error.rs`: `impl From<Error> for cowboy_workflow_core::WorkflowError` maps every store error, including `TemporarilyBusy`, to `WorkflowError::InvalidAction(value.to_string())`.

`WorkflowError::InvalidAction` is not recoverable. During an agent step, role-session reads/writes and other store operations can therefore turn transient database lock contention into a terminal workflow-step failure. The runner persists `RunStatus::Failed { reason }`, emits `RunFailed`, and the UI shows manual resolve guidance even though the workflow action itself was valid.

# Reproduction steps

1. Use the focused regression test added in `crates/workflow/store/src/error.rs`.
2. Convert `Error::TemporarilyBusy(PathBuf::from("workflow.redb"))` into `WorkflowError` through the production `From<Error>` implementation.
3. Assert that temporary workflow-store lock contention remains recoverable and is not prefixed with `invalid action:`.
4. Run:

```bash
cargo test -p cowboy-workflow-store error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error -- --exact
```

# Regression test

- Test file path: `crates/workflow/store/src/error.rs`
- Test name: `error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error`
- Command: `cargo test -p cowboy-workflow-store error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error -- --exact`
- Expected failure before the fix: the assertion that `workflow_error.recoverable()` is true fails because the converted error is `WorkflowError::InvalidAction` with text beginning `invalid action: workflow store "workflow.redb" is temporarily busy; another Cowboy instance is using it`.

# Current failing result

```text
running 1 test
failures:

---- error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error stdout ----

thread 'error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error' panicked at crates/workflow/store/src/error.rs:64:9:
temporary workflow-store lock contention must remain retryable, got invalid action: workflow store "workflow.redb" is temporarily busy; another Cowboy instance is using it
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy-workflow-store --lib`
```

# Fix constraints

- Do not treat temporary redb lock contention as an invalid workflow action.
- Preserve invalid-action semantics for genuine workflow authoring or action validation errors.
- Preserve existing same-run execution locking behavior and its clear `run <id> is already active in another Cowboy instance` error.
- Keep absolute user paths, secrets, and private environment details out of user-facing investigation documents.
- The regression test should pass only when `TemporarilyBusy` converts to a recoverable workflow error or an equivalent non-terminal workflow error path that lets the runner retry instead of persisting a failed step.
