# Plan

Base the fix on the approved RCA in `docs/plans/db_busy_issue/rca.md` and keep the investigator-added regression test `crates/workflow/store/src/redb_store.rs::redb_store::tests::transient_database_contention_outlasting_retry_window_does_not_fail` unchanged as the red-capable input contract.

The store-level busy fix remains: Cowboy must wait for redb availability when `DatabaseAlreadyOpen` means another process currently owns the exclusive workflow-store lock. The replan adds the reviewer-requested observability requirement: when an open attempt first observes contention and enters its sleep/retry wait loop, Cowboy must emit exactly one waiting notification for that wait episode to the UI path and exactly one structured log entry. It must not emit on every 25 ms backoff tick.

Use a store wait-start observer as the seam. `cowboy-workflow-store` owns the redb open loop and can detect the first `DatabaseAlreadyOpen`; it should log there and notify an optional observer. `cowboy-workflow-engine` should construct run-bound stores with an observer that projects wait-start into a new `WorkflowEventKind::WorkflowStoreWaiting` event. The TUI and `engine-cli` already consume `WorkflowEventKind`; adding one event variant keeps the notification live in the transcript/progress stream without coupling the store crate to UI or engine types.

The UI message must be generic and sanitized, for example `Workflow store is busy; waiting for another Cowboy instance to finish a database operation.` Do not include absolute workflow-store paths, private state directories, raw user requests, or run ids in the message body. Standard event metadata may still carry the existing run id for normal event routing. The log entry may include the structured workflow-store path field for local diagnostics.

Preserve the path-backed `RedbRunStore` design: every operation must still open redb transiently, complete one short synchronous transaction, and drop the database handle before returning. Do not change the database schema, persisted run data, engine run-lock semantics, workflow retry policies, or the existing 750 ms regression test.

# Changes

- In `crates/workflow/store/src/redb_store.rs`:
  - keep the availability loop in `open_database_when_available`: retry only `redb::DatabaseError::DatabaseAlreadyOpen`, sleep for `OPEN_RETRY_BACKOFF`, and immediately return all other `DatabaseError` values;
  - add a small store wait-start observer type that can be cloned with `RedbRunStore` and called from the open helper on the first `DatabaseAlreadyOpen` in a single open wait episode;
  - keep `RedbRunStore::create` and `RedbRunStore::open` as no-observer constructors for existing callers, and add an observer-aware constructor or builder used by the engine;
  - call the observer exactly once before the first sleep for each contended open call and never on later sleeps in the same wait episode;
  - write a `tracing::info!` log from the same wait-start branch with a stable message such as `workflow store busy; waiting for availability`, including the store path only as a structured log field.
- In `crates/workflow/store/src/error.rs`:
  - keep the removed `TemporarilyBusy` path out of the implementation; contention should not become a workflow action error after this fix;
  - preserve terminal `WorkflowError::InvalidAction` mapping for all remaining store errors.
- In `crates/workflow/store/Cargo.toml`:
  - add the workspace `tracing` dependency if the store crate logs directly.
- In `crates/workflow/engine/src/events.rs`:
  - add `WorkflowEventKind::WorkflowStoreWaiting { message: String }` with stable serde tagging;
  - include the new variant in event serialization/round-trip coverage.
- In `crates/workflow/engine/src/runtime.rs`:
  - add a helper for constructing a run-bound `RedbRunStore` with the wait observer;
  - use that helper for run-advancing paths where a run id is known or has just been generated: run creation, resume/step, answer, resolve continuation, cancellation cleanup, and agent/store access inside `run_existing_with_events`;
  - emit `WorkflowStoreWaiting` through `EventBus` from the observer with sanitized message text and the relevant run id;
  - preserve log-only behavior for store use that has no run context, such as listing runs, unless a future app-level notification channel is introduced.
- In `crates/workflow/engine/src/bin/engine-cli.rs`:
  - render `WorkflowStoreWaiting` as a progress line so non-TUI live progress shows the same waiting state.
- In `crates/tui/app/src/app/events.rs` and `crates/tui/app/src/app/state.rs`:
  - render `WorkflowStoreWaiting` as a warning/waiting workflow card in the transcript;
  - update app metadata handling so the event updates the visible status/run state without marking the durable run as `WaitingForInput` or creating a pending prompt.

# Tests to be added/updated

- Do not rewrite, shorten, or replace `redb_store::tests::transient_database_contention_outlasting_retry_window_does_not_fail`. It must retain the real 750 ms redb lock holder and pass after the production helper waits for release.
- Add a focused store test in `crates/workflow/store/src/redb_store.rs` that holds a real redb `Database`, opens the same path through an observer-aware `RedbRunStore`, and asserts the wait-start observer fires exactly once during the wait episode.
- Keep `redb_store::tests::live_store_value_does_not_keep_database_locked` passing to prove the observer work does not regress into lifetime-long database ownership.
- Preserve `crates/workflow/store/src/error.rs` coverage that remaining store failures map to terminal workflow errors.
- Add an engine/runtime test that starts or resumes a deterministic workflow while another handle temporarily owns the workflow-store lock, subscribes to `runtime.events()`, and asserts exactly one `WorkflowStoreWaiting` event is emitted before the run completes.
- Add event serde coverage for `WorkflowEventKind::WorkflowStoreWaiting`.
- Add `engine-cli` event-render coverage if the existing tests cover event formatting; otherwise rely on compiler exhaustiveness for the match and the engine event tests.
- Add a TUI rendering/projection test proving `WorkflowStoreWaiting` appears as a warning/waiting card with sanitized text and without a pending-prompt state.
- Run the complete `cowboy-workflow-store` test suite to cover create/open behavior, committed data, tables, and existing path-backed store contracts.

# How to verify

1. Run the exact RCA repro:
   `cargo test -p cowboy-workflow-store transient_database_contention_outlasting_retry_window_does_not_fail -- --nocapture`
   It must wait past the former retry window and pass after the holder releases the lock; it must not return a temporary-busy workflow-store error.
2. Run the new store observer test:
   `cargo test -p cowboy-workflow-store store_wait_observer_fires_once_when_contention_starts -- --nocapture`
   It must show exactly one observer notification for one contended open wait episode.
3. Run the path-backed lifetime guard:
   `cargo test -p cowboy-workflow-store live_store_value_does_not_keep_database_locked`
4. Run the affected store crate suite:
   `cargo test -p cowboy-workflow-store`
5. Run focused engine event/runtime tests for the new store-wait event:
   `cargo test -p cowboy-workflow-engine workflow_store_wait`
6. Run focused TUI rendering/state tests for the new event:
   `cargo test -p cowboy workflow_store_wait`
7. Check warnings and formatting:
   `cargo clippy -p cowboy-workflow-store -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`
   `cargo fmt --all -- --check`
8. Manual smoke check in the TUI or `engine-cli`: hold the workflow store briefly with a second redb handle, start or resume a run, and confirm one waiting notification appears when waiting starts and the log contains one matching wait-start entry. Confirm the UI text does not include absolute paths or private state details.
9. Inspect the focused diff to confirm the regression test's 750 ms contention setup is unchanged, no redb handle survives a store operation, and the wait notification is one-per-wait-episode rather than one-per-backoff-sleep.

# TODO

- [x] Replace the bounded redb open retry loop with availability waiting for `DatabaseAlreadyOpen` while preserving the 25 ms backoff and immediate propagation of other errors.
- [x] Rename the private helper to `open_database_when_available` and update the create, open, and per-operation call sites without changing public store APIs or handle lifetimes.
- [x] Remove the unreachable `TemporarilyBusy` store error path and simplify the remaining store-to-workflow error conversion.
- [x] Remove only the obsolete `TemporarilyBusy` conversion test and preserve the remaining error-mapping coverage.
- [x] Keep the investigator-added 750 ms contention regression test unchanged and make it pass through the production fix.
- [x] Verify the live-store lifetime regression and the complete `cowboy-workflow-store` test suite.
- [x] Run warnings-denied Clippy and the workspace formatting check for the store-only fix.
- [x] Review the store-only focused diff for scope, unchanged persistence contracts, and absence of sensitive paths or identifiers.
- [x] Add a cloneable store wait-start observer seam to `RedbRunStore` without coupling `cowboy-workflow-store` to engine or TUI types.
- [x] Emit one `tracing::info!` log entry from the first-contended-open branch for each store wait episode.
- [x] Add an observer-aware store constructor or builder and use it only where the engine needs run-bound UI notifications.
- [x] Add `WorkflowEventKind::WorkflowStoreWaiting` and serde coverage.
- [x] Wire runtime run-bound store construction to emit one sanitized `WorkflowStoreWaiting` event on wait start.
- [x] Render `WorkflowStoreWaiting` in `engine-cli` live progress.
- [x] Render `WorkflowStoreWaiting` as a warning/waiting TUI transcript card.
- [x] Update TUI state metadata so store waiting is visible but does not create a pending prompt or durable waiting status.
- [x] Add the store observer exactly-once regression test.
- [x] Add the engine/runtime store-wait event regression test.
- [x] Add the TUI rendering/state regression test for the store-wait event.
- [x] Re-run the RCA repro, live-store guard, store suite, focused engine/TUI tests, warnings-denied Clippy, and rustfmt check.
- [x] Manually smoke test one contended run and confirm exactly one UI notification and one log entry when waiting begins.
- [x] Review the final diff for scope, no private data in UI messages, unchanged persistence/schema contracts, and no per-backoff notification spam.
