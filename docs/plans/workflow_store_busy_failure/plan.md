# Plan

Base the fix on `docs/plans/workflow_store_busy_failure/rca.md` and keep the investigator-added regression test `crates/workflow/store/src/error.rs::error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error` as the input contract. The current baseline is red: the exact repro command reports `TemporarilyBusy` converting to `invalid action: workflow store "workflow.redb" is temporarily busy; another Cowboy instance is using it`.

Fix the store error boundary so exhausted redb open retries caused by another Cowboy process are represented as a recoverable workflow error. Do not change redb retry timing, workflow step retry limits, run locking, or manual resolution behavior.

# Changes

- In `crates/workflow/store/src/error.rs`, change `impl From<Error> for cowboy_workflow_core::WorkflowError` from unconditional `WorkflowError::InvalidAction(...)` to a match that maps only `Error::TemporarilyBusy(_)` to `WorkflowError::RecoverableAction(...)`.
- Keep all non-temporary store errors mapped to `WorkflowError::InvalidAction(...)`, including missing objects/runs, database/table/storage/commit errors, JSON errors, and I/O errors.
- Preserve the existing user-facing temporary-busy message text, but remove the `invalid action:` prefix for the temporary-busy path.
- Audit direct store-open error mapping in `crates/workflow/engine/src/runtime.rs`; if `RedbRunStore::create(...)` is still manually wrapped as `WorkflowError::InvalidAction(...)`, route it through the same store-error conversion so store contention is not mislabeled before the runner is built.
- Do not modify `crates/workflow/engine/src/run_lock.rs`; its same-run active-instance error is separate from redb store contention and should remain terminal.

# Tests to be added/updated

- Keep the existing repro test unchanged: `crates/workflow/store/src/error.rs::error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error`.
- Add a narrow sibling test in `crates/workflow/store/src/error.rs` that converts a non-temporary store error, such as `Error::RunNotFound("run-1".to_string())`, and asserts it remains non-recoverable `WorkflowError::InvalidAction(_)`.
- If the runtime store-open mapping is updated, rely on the store conversion unit tests for the mapping contract and run the existing engine run-lock tests to confirm same-run locking behavior remains unchanged.

# How to verify

- Run the exact repro and confirm it passes:

```bash
cargo test -p cowboy-workflow-store error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error -- --exact
```

- Run all store error conversion tests:

```bash
cargo test -p cowboy-workflow-store error::tests
```

- Run the relevant engine locking tests if `crates/workflow/engine/src/runtime.rs` was touched:

```bash
cargo test -p cowboy-workflow-engine run_lock
```

- Confirm no user-facing plan or test output includes private absolute state paths; use relative paths or `[state_dir]/workflow.redb` when documenting the original failure.

# TODO

- [x] Update `crates/workflow/store/src/error.rs` so `Error::TemporarilyBusy(_)` converts to `WorkflowError::RecoverableAction(...)`.
- [x] Keep all other `cowboy_workflow_store::Error` variants converting to terminal `WorkflowError::InvalidAction(...)`.
- [x] Audit and, if needed, update `WorkflowRuntime::store` in `crates/workflow/engine/src/runtime.rs` to reuse the store-error conversion instead of manually wrapping store-open failures as invalid actions.
- [x] Leave `crates/workflow/engine/src/run_lock.rs` behavior unchanged.
- [x] Keep the investigator-added repro test unchanged and make it pass.
- [x] Add a sibling non-temporary store-error conversion test proving non-transient store errors remain terminal invalid actions.
- [x] Run the verification commands listed above and record the results.

# Verification results

- `cargo test -p cowboy-workflow-store error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error -- --exact` — passed, 1 test.
- `cargo test -p cowboy-workflow-store error::tests` — passed, 2 tests.
- `cargo test -p cowboy-workflow-engine run_lock` — passed, 5 tests.

# Follow-up verification results

- After reverting unrelated formatting-only changes, `cargo test -p cowboy-workflow-store error::tests::temporarily_busy_store_error_maps_to_recoverable_workflow_error -- --exact` — passed, 1 test.
- After reverting unrelated formatting-only changes, `cargo test -p cowboy-workflow-store error::tests` — passed, 2 tests.
- After reverting unrelated formatting-only changes, `cargo test -p cowboy-workflow-engine run_lock` — passed, 5 tests.
