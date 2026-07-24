# Plan

Replace the production redb backend with a SQLite backend while refining the core persistence boundary around Cowboy's actual domain operations. The current `RunStore` in `crates/workflow/core/src/traits.rs` is broader than its name because it stores workflow sources, step records, turns, role sessions, user prompts, and prompt windows in addition to runs. Rename the public composite abstraction to `WorkflowStore`, name the SQLite implementation `SqliteWorkflowStore`, and divide the interface into asynchronous, object-safe capabilities named for their responsibility: `WorkflowStateStore`, `WorkflowObjectStore`, `AgentSessionStore`, `TurnStore`, `UserPromptStore`, and `PromptWindowStore`. Define them with the repository's existing `async-trait` convention, and make each caller depend on the narrowest capability or capability combination it needs.

Define async typed operations for workflow source snapshots, step records, run snapshots/heads, role sessions, turns, user prompts, and agent prompt windows. Replace the current `put_object<T>`/`get_object<T>` API with explicit methods such as storing a workflow source snapshot, loading a step record, and atomically committing a completed step. Make `save_run` derive and update `RunHead` in the same transaction, and make the completed-step operation insert the immutable step record, set `run.head`, save the run snapshot, and update the run head in one transaction. Keep prompt-window methods as transaction-level domain operations because their run-status checks, sequence assignment, pending-prompt reads, and seal/abort updates must remain totally ordered. Convert persistence-calling core helpers and runtime APIs to async rather than hiding database work behind `block_on`, `spawn_blocking`, or synchronous wrappers.

Implement `SqliteWorkflowStore` in `cowboy-workflow-store` with `sqlx` 0.8 using the `runtime-tokio` and `sqlite` features and no `sqlite-unbundled` feature, so Cowboy uses SQLx's async SQLite driver without gaining a system SQLite installation requirement. `SqliteWorkflowStore::connect(...).await` first opens one async bootstrap `SqliteConnection` with `create_if_missing`, foreign keys, and zero busy timeout, but without changing journal mode. It rejects non-SQLite files and future `user_version` values before mutation. For version 0, it retries `BEGIN IMMEDIATE`, re-reads the version inside the transaction, applies idempotent schema DDL and `PRAGMA user_version = 1` only when still uninitialized, and commits; a competing initializer that observes version 1 validates and commits without recreating state. The bounded bootstrap retry uses the 25 ms backoff for at most five seconds. After version validation/bootstrap, set WAL with the same contention classification, close the bootstrap connection, and create the cloneable `SqlitePool`. Configure `SqlitePoolOptions` with `min_connections(0)`, `max_connections(4)`, and a five-second acquire timeout; make `WorkflowRuntime::new` async and fallible so schema/open failures are reported before a runtime is returned. Store durable Rust models as JSON blobs while using relational keys and indexes for identity and ordering:

- `runs(run_id PRIMARY KEY, data)`
- `run_heads(run_id PRIMARY KEY, data)`
- `objects(hash PRIMARY KEY, kind, data)` using the existing canonical envelope and BLAKE3 hash format
- `role_sessions(run_id, role_id, data, PRIMARY KEY(run_id, role_id))`
- `run_turns(run_id, step_record_id, position, object_hash, PRIMARY KEY(run_id, step_record_id, position))`
- `run_user_prompts(run_id, sequence, data, PRIMARY KEY(run_id, sequence))`
- `agent_prompt_windows(run_id PRIMARY KEY, window_id, data)`

Use `sqlx::Transaction<'_, Sqlite>` for every write group, `INSERT ... ON CONFLICT DO UPDATE` for mutable rows, and `INSERT OR IGNORE` for immutable content-addressed objects after verifying that an existing hash contains identical canonical bytes. Commit before returning success and let every error path roll back before retry or return. Preserve the current rule that deleting a run removes its mutable rows and indexes but leaves immutable objects for future garbage collection.

Preserve the current multi-instance behavior. SQLx exposes SQLite extended result codes, so centralize classification by parsing `DatabaseError::code()` as an integer and comparing `code & 0xff` with primary `SQLITE_BUSY` (`5`) or `SQLITE_LOCKED` (`6`); this includes extended values such as `SQLITE_BUSY_SNAPSHOT` (`517`) and `SQLITE_LOCKED_SHAREDCACHE` (`262`). Wrap each write transaction in an async retry helper that retries only those classified lock-contention results, drops/rolls back the failed transaction before sleeping, fires the sanitized store-wait observer once per blocked operation, and uses `tokio::select!` between the existing 25 ms backoff and a `tokio::sync::watch` cancellation-generation change so `WorkflowRuntime::cancel_store_waits` interrupts the wait promptly. Replace the current atomic-only cancellation snapshot with a watch sender/receiver generation while preserving the public cancellation behavior. Do not retry schema, constraint, serialization, corruption, pool-acquire timeout, malformed/non-numeric codes, or application errors. Keep `WorkflowStoreWaiting` events and the TUI/CLI rendering contract. Retain the engine's sidecar per-run file locks because SQLite serializes database writes but does not prevent two Cowboy processes from advancing the same run concurrently.

Use a clean persisted-store cutover. Keep the configuration key `workflow_store`, change the default filename and examples from `workflow.redb` to `data.db`, and do not overwrite or reinterpret an existing redb file. Opening an existing non-SQLite file must fail with an actionable error that tells the user to preserve the old file and choose or clear a SQLite store path. Existing redb data is not automatically migrated in this feature; document that boundary explicitly. Event JSON files and run lock files remain separate and unchanged apart from the configured database filename used to derive the lock-directory name.

# Changes

- In `crates/workflow/core/src/traits.rs`, replace the monolithic generic `RunStore` contract with `#[async_trait]` capabilities `WorkflowStateStore`, `WorkflowObjectStore`, `AgentSessionStore`, `TurnStore`, `UserPromptStore`, `PromptWindowStore`, and the composite `WorkflowStore`. Include async transaction-oriented methods for saving a run with its derived head and committing a completed step atomically. Remove generic serde bounds from the public persistence interface.
- In `crates/workflow/core/src/engine.rs`, await previous-step and user-prompt loading through typed APIs, update async `apply_step_record` to await the atomic completed-step operation, and update async `apply_run_status` to await the single run/head persistence operation. Convert `ActiveRunClock::close`, retry reservation, resume routing, and other persistence-calling helpers to async so no code performs separate `save_run` plus `update_run_head` writes or blocks a Tokio worker.
- In `crates/workflow/agent`, make `AgentExecutor` depend only on the role-session, turn, prompt-history, and prompt-window capabilities. Remove full-store forwarding wrappers such as `StoreWithSessions` once the executor can receive the narrow interface directly.
- In `crates/workflow/store`, add `sqlite_store.rs` and `schema.rs`, retain `hash.rs`, rewrite `error.rs` for SQLite initialization/query/transaction/busy/cancellation errors, and export `SqliteWorkflowStore` plus backend-neutral wait observer/cancellation types. Remove `redb_store.rs`, `tables.rs`, `RedbRunStore`, and redb-specific error variants after all callers move. Do not introduce `SqliteRunStore` as an alias; the generic concrete name is the only public backend type.
- Add workspace `sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite"] }`, add the required Tokio `sync`/`time` features to the store crate, and remove `redb`. Use runtime `sqlx::query` calls rather than compile-time query macros, so builds do not require `DATABASE_URL` or checked-in SQLx offline metadata. Keep JSON serialization and the existing canonical object envelope so content hashes remain deterministic within the new store.
- Implement async bootstrap-connection and pool creation with forward-only version checks. Serialize first-open schema work with retryable `BEGIN IMMEDIATE`, re-check `user_version` inside the transaction, use idempotent DDL, and establish WAL only after the file is proven supported. A new or empty path creates schema version 1; concurrent first opens converge on the same schema; a supported SQLite database opens normally; and a newer schema version or non-SQLite/corrupt file fails without modifying the file.
- Implement run/head upsert, source-object storage, atomic completed-step commit, typed step-record loading, role-session CRUD, atomic turn append/indexing, ordered user-prompt loading, prompt-window open/append/compare-and-seal/abort/clear, and low-level run/object deletion for `store-cli`.
- Implement SQLx SQLite busy/locked classification using the primary low byte of extended result codes and an async whole-operation retry loop with WAL, one wait-start notification per blocked operation, 25 ms Tokio backoff, watch-channel cancellation-generation notification, and sanitized logging. Map cancellation to the existing store-wait cancellation path and keep genuine schema/data/serialization/pool errors terminal.
- In `crates/workflow/engine/src/runtime.rs`, asynchronously connect the SQLite store behind the composite interface, keep a separate cancellation handle for `cancel_store_waits`, make `WorkflowRuntime::new` return `Result<Self>` asynchronously, and remove imports, fields, helpers, and tests coupled to `RedbRunStore`. Await store calls in runtime operations, CLI dispatch, TUI tasks, and tests while continuing to emit run-bound `WorkflowStoreWaiting` events through the configured observer.
- Keep `crates/workflow/engine/src/run_lock.rs` as the same-run execution guard, but update fallback names and fixtures from `workflow.redb` to `data.db`. The lock namespace must continue to follow the configured `workflow_store` path, producing `data.db.locks` for the default database.
- Update in-memory test stores to implement only the capability traits used by each test. Add a reusable backend contract test module in `cowboy-workflow-store` so all SQLite behavior is exercised through the public interface rather than only through inherent methods.
- Update `crates/workflow/store/src/bin/store-cli.rs`, `crates/workflow/agent/src/bin/execute-agent.rs`, and `crates/workflow/engine/src/bin/engine-cli.rs` to run under Tokio, await SQLite connection and typed store operations, and use `data.db` in default-path examples and fixtures. Keep `store-cli` able to inspect, save, load, and delete the same domain records without exposing generic object serialization.
- Change the default store path in `crates/tui/app/src/config.rs`, `demo-config.toml`, CLI/TUI fixtures, and test helpers to `data.db`. Add configuration coverage proving an existing non-SQLite file is rejected without alteration.
- Update `README.md`, `docs/architecture.md`, `docs/module-map.md`, repository `AGENTS.md`, and directly affected workflow-store guidance to describe SQLite ownership, schema versioning, WAL/busy behavior, typed store capabilities, atomic persistence boundaries, clean-cutover instructions, and the unchanged event-log/run-lock layout.
- Add `scripts/run-exact-test.sh`, which accepts a package, fully qualified library-test name, and exact evidence marker; proves exactly one matching test is listed; runs it with `--exact --nocapture`; proves the result contains exactly `1 passed`, `0 failed`, and `0 ignored`; requires the marker; and only then prints `EXACT_TEST_OK <package> <test>`. Add `scripts/run-required-sqlite-tests.sh` plus `scripts/required-sqlite-tests.tsv` to execute the complete required regression manifest through that gate.

# Tests to be added/updated

- Add core compile-time/interface tests proving async `WorkflowStore` and each named capability are object-safe and that engine helpers accept the narrow capabilities rather than a concrete backend.
- Add reusable store contract tests covering run/head round trips, deterministic run listing, config-set fields, source and step object hashes, reopen durability, role-session CRUD, turn append ordering, run deletion, immutable-object retention, and low-level object deletion.
- Add atomicity tests proving `save_run` always updates the derived `RunHead`, and proving the completed-step transaction leaves no step object, run snapshot, or run head partially updated when an injected failure occurs before commit.
- Add schema tests for new-file initialization, empty-file initialization, two concurrent first-open connects synchronized before `BEGIN IMMEDIATE`, reopen, supported `user_version`, rejected future versions, and rejection of a non-SQLite file without changing its bytes.
- Add SQLite concurrency tests with two live SQLx pools for the same path: concurrent readers succeed under WAL, independent short async writes become visible to both pools, and pool connections remain reusable after transaction success, rollback, and cancellation.
- Add focused classification tests proving extended code `517` is BUSY, extended code `262` is LOCKED, primary codes `5`/`6` remain retryable, and constraint/malformed codes are not retryable. Port the wait observer and cancellation tests to SQLite by holding a `BEGIN IMMEDIATE` write transaction through one SQLx connection, starting a write through another pool, asserting exactly one observer notification after a busy/locked result, cancelling during Tokio backoff, and observing the cancellation error promptly without leaking a checked-out connection or open transaction.
- Port prompt-window tests unchanged at the behavior level: exact prompt content and millisecond timestamp preservation, monotonic sequences, total ordering between append and compare-and-seal, stale/sealed/terminal rejection, abort/clear behavior, and run deletion cleanup.
- Update core and engine in-memory stores for the async typed interfaces, convert affected unit tests to `#[tokio::test]`, then retain runner tests for retry persistence, step completion, status persistence, and previous-step loading.
- Update agent tests for session reuse, prompt delivery watermarks, turn persistence, prompt-window handoff, and cancellation cleanup through the narrower agent-store capabilities.
- Update runtime tests for async construction of two `WorkflowRuntime` instances with independent SQLx pools sharing one SQLite store, different-run coexistence, same-run sidecar lock rejection, store-wait event emission, store-wait cancellation, list/load/start/resume/answer/resolve paths, and event persistence.
- Update app/config, product CLI, diagnostic CLI, and TUI fixtures that currently name `workflow.redb`; retain rendering tests for the sanitized `WorkflowStoreWaiting` card.
- Add an executable clean-cutover test that places non-SQLite bytes at the configured path, starts Cowboy, asserts the actionable error, and verifies the file digest is unchanged.
- Make every required focused regression print one stable `EVIDENCE ...` line after its independent assertions pass. The marker summarizes the material observation but does not replace assertions.
- Add a checked required-test manifest covering interface shape, schema/bootstrap, all public store capabilities, transaction rollback, prompt ordering, WAL concurrency, extended result-code classification, contention/cancellation, agent session/prompt behavior, runtime lifecycle/locking/events, configuration, rendering, and clean cutover.

# How to verify

- Run `cargo test -p cowboy-workflow-core`.
- Run `cargo test -p cowboy-workflow-store`.
- Run `cargo test -p cowboy-workflow-agent`.
- Run `cargo test -p cowboy-workflow-actions`.
- Run `cargo test -p cowboy-workflow-engine`.
- Run `cargo test -p cowboy`.
- Execute the ordered Bash procedure in `TODO-10` to build test apps; create and clean a temporary workspace; exercise `store-cli`; start and synchronize two `engine-cli` processes; reproduce same-run lock contention and store-wait cancellation through exact tests; and prove a rejected non-SQLite file retains the same SHA-256 digest.
- Run `bash scripts/run-required-sqlite-tests.sh`; it must reject a missing, renamed, duplicated, ignored, zero-match, or marker-less required regression.
- Run `cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store -p cowboy-workflow-agent -p cowboy-workflow-actions -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`.

# TODO

- [x] TODO-01: Replace the generic `RunStore` API with object-safe typed store capabilities and a composite `WorkflowStore`.
  - Procedure: Edit `crates/workflow/core/src/traits.rs` to define `#[async_trait]` capabilities `WorkflowStateStore`, `WorkflowObjectStore`, `AgentSessionStore`, `TurnStore`, `UserPromptStore`, `PromptWindowStore`, and composite `WorkflowStore`; make every persistence method async; add the atomic run/head and completed-step contracts; add `traits::tests::workflow_store_capabilities_are_object_safe`, whose compile assertions cover all seven trait objects and which prints `EVIDENCE workflow-store-traits object_safe=7 async=true`; convert persistence-calling core helpers/tests to async; implement `scripts/run-exact-test.sh` with the exact list-count, execution-count, ignored-count, and evidence-marker contract described under Changes; then run:
    ```bash
    set -euo pipefail
    test -x scripts/run-exact-test.sh
    if bash scripts/run-exact-test.sh \
      cowboy-workflow-core \
      traits::tests::does_not_exist \
      'EVIDENCE impossible'; then
      echo "exact-test gate accepted a zero-match test" >&2
      exit 1
    fi
    if bash scripts/run-exact-test.sh \
      cowboy-workflow-core \
      traits::tests::workflow_store_capabilities_are_object_safe \
      'EVIDENCE wrong-marker'; then
      echo "exact-test gate accepted a missing evidence marker" >&2
      exit 1
    fi
    bash scripts/run-exact-test.sh \
      cowboy-workflow-core \
      traits::tests::workflow_store_capabilities_are_object_safe \
      'EVIDENCE workflow-store-traits object_safe=7 async=true'
    cargo test -p cowboy-workflow-core
    if rg -n 'pub trait RunStore|pub type RunStore|trait RunStore|put_object[<(]|get_object[<(]' \
      crates/workflow/core crates/workflow/engine crates/workflow/actions \
      crates/workflow/agent crates/workflow/store; then
      echo "obsolete RunStore or generic object API remains" >&2
      exit 1
    fi
    ```
  - Expected result: The helper is executable; its two negative probes reject a zero-match test and a wrong marker; the positive gate proves one non-ignored object-safety test was listed and exactly one passed and finds its seven-capability async marker; the full core suite passes; the guarded source search finds no `RunStore` trait/alias or generic object API; and core code awaits the async capability operations.
  - Implementer observed result: The exact Bash sequence exited `0`. Both negative probes were rejected, the positive gate executed exactly one non-ignored test and found `EVIDENCE workflow-store-traits object_safe=7 async=true`, all 37 core tests passed, and the obsolete-API guard found no matches.

- [x] TODO-02: Add the SQLite dependency, versioned schema bootstrap, connection policy, and store error model.
  - Procedure: Add workspace `sqlx` 0.8 with `default-features = false` and features `runtime-tokio` and `sqlite`, plus the Tokio `sync`, `time`, `macros`, and runtime features needed by the store/tests. Create `schema.rs`; implement `SqliteWorkflowStore::connect(...).await` so it opens a zero-busy-timeout bootstrap `SqliteConnection`, preflights non-SQLite/future-version files before mutation, and for version 0 retries `BEGIN IMMEDIATE` every 25 ms for at most five seconds, re-checks `user_version` inside the transaction, applies idempotent DDL plus `user_version = 1` only when needed, commits, establishes WAL after validation, closes the bootstrap connection, and creates a pool with foreign keys, `min_connections(0)`, `max_connections(4)`, and `acquire_timeout(Duration::from_secs(5))`. Add a test-only bootstrap barrier reached after both connections open and immediately before `BEGIN IMMEDIATE`. Make the five tests below print the specified marker after their assertions, then run:
    ```bash
    set -euo pipefail
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      schema::tests::initializes_new_and_empty_files \
      'EVIDENCE schema-init header=SQLite_format_3 user_version=1 wal=true foreign_keys=true max_connections=4'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      schema::tests::reopens_supported_schema_version \
      'EVIDENCE schema-reopen user_version=1 tables=7'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      schema::tests::rejects_future_schema_version_without_modifying_file \
      'EVIDENCE schema-future rejected=true bytes_unchanged=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      schema::tests::rejects_non_sqlite_file_without_modifying_file \
      'EVIDENCE schema-non-sqlite rejected=true bytes_unchanged=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      schema::tests::concurrent_first_connects_initialize_one_schema \
      'EVIDENCE schema-concurrent connects=2 user_version=1 tables=7'
    ```
  - Expected result: Each invocation proves exactly one named, non-ignored test exists and exactly one passed. The markers independently report the SQLite header/configuration, successful reopen, byte-preserving rejection cases, and two synchronized first connects converging on one complete seven-table version-1 schema.
  - Implementer observed result: The exact Bash sequence exited `0`. All five gates listed and executed exactly one non-ignored test and emitted their required schema initialization, reopen, byte-preservation, and synchronized-concurrent-bootstrap markers.

- [x] TODO-03: Implement `SqliteRunStore` for every typed persistence capability with transactional invariants.
  - Procedure: The retained TODO subject contains the earlier proposed type name; implement and publicly export the user-approved generic name `SqliteWorkflowStore` instead, with no `SqliteRunStore` alias. Implement every capability as async SQLx queries over `SqlitePool`; implement run/head persistence, source and step objects, atomic completed-step commit, role sessions, turns, prompts, prompt windows, and cleanup APIs in `sqlite_store.rs`; use `sqlx::Transaction<'_, Sqlite>` for each write group. Make the eight tests below print their specified markers after assertions, then run:
    ```bash
    set -euo pipefail
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      contract::tests::run_head_round_trip_and_deterministic_listing \
      'EVIDENCE contract-run-head round_trip=true deterministic=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      contract::tests::source_step_hashes_and_reopen_durability \
      'EVIDENCE contract-objects source_hash=true step_hash=true reopen=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      contract::tests::role_session_and_turn_ordering \
      'EVIDENCE contract-agent session_crud=true turn_order=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      contract::tests::run_deletion_retains_objects_and_low_level_delete_removes_them \
      'EVIDENCE contract-delete mutable_removed=true immutable_retained=true explicit_delete=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::save_run_updates_run_and_head_atomically \
      'EVIDENCE transaction-run-head committed=true snapshots_match=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::completed_step_transaction_rolls_back_on_injected_failure \
      'EVIDENCE transaction-step rollback=true pool_reusable=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::prompt_append_and_compare_and_seal_are_totally_ordered \
      'EVIDENCE prompt-order legal_outcomes=2 partial_state=false'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::immutable_object_collision_is_rejected_without_overwrite \
      'EVIDENCE object-collision rejected=true bytes_unchanged=true'
    if rg -n 'SqliteRunStore' crates README.md AGENTS.md docs/architecture.md docs/module-map.md; then
      echo "obsolete SqliteRunStore symbol remains" >&2
      exit 1
    fi
    ```
  - Expected result: All eight exact gates prove one non-ignored test executed and emitted the required material evidence. Together they cover every public capability, deterministic hashes/order, reopen durability, mutable deletion versus immutable retention, atomic run/head commit, rollback and pool reuse, prompt serialization, and collision safety; the final guard proves no `SqliteRunStore` alias/reference remains.
  - Implementer observed result: The exact Bash sequence exited `0`. All eight gates executed exactly one non-ignored test with the required capability, durability, deletion, transaction, prompt-ordering, and collision markers, and the final guard found no `SqliteRunStore` reference.

- [x] TODO-04: Preserve cancellable database-wait behavior and sanitized wait notifications on SQLite contention.
  - Procedure: Set the SQLx SQLite driver busy timeout to zero so contention returns promptly. Add a single `is_retryable_sqlite_code(Option<&str>)` helper that parses the value supplied by SQLx `DatabaseError::code()` as an integer, computes `code & 0xff`, and returns true only when that primary byte is `5` (`SQLITE_BUSY`) or `6` (`SQLITE_LOCKED`); parse failures and absent codes are non-retryable. Route production SQLx errors through that helper. Replace the atomic-only cancellation snapshot with a `tokio::sync::watch` generation; implement a whole-operation async retry loop that drops/rolls back failed transactions before `tokio::select!` waits on the 25 ms backoff or `watch::Receiver::changed()`; fire the shared observer only on the first retry. Make the seven tests below print their specified markers, then run:
    ```bash
    set -euo pipefail
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::store_wait_observer_fires_once_for_contended_write \
      'EVIDENCE store-wait observer_count=1 retries_at_least=1 path_leaked=false'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::store_wait_cancellation_interrupts_contended_write \
      'EVIDENCE store-cancel cancelled=true pool_reusable=true'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      runtime::tests::workflow_store_wait_event_emits_once_during_contended_resume \
      'EVIDENCE runtime-wait-event count=1 sanitized=true'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      runtime::tests::cancelling_runtime_store_wait_interrupts_contended_operation \
      'EVIDENCE runtime-wait-cancel cancelled=true bounded=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::extended_busy_code_is_retryable \
      'EVIDENCE sqlite-code value=517 primary=5 retryable=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::extended_locked_code_is_retryable \
      'EVIDENCE sqlite-code value=262 primary=6 retryable=true'
    bash scripts/run-exact-test.sh cowboy-workflow-store \
      sqlite_store::tests::non_locking_sqlx_errors_are_not_retried \
      'EVIDENCE sqlite-code non_locking=true attempts=1'
    ```
  - Expected result: Each gate proves exactly one non-ignored test executed. The markers prove one sanitized notification, prompt watch-channel cancellation and pool reuse, one sanitized runtime event, bounded runtime cancellation, extended BUSY/LOCKED classification by primary low byte, and one-attempt handling for constraint/malformed/absent codes.
  - Implementer observed result: The exact Bash sequence exited `0`. All seven gates executed exactly one non-ignored test and emitted the required single-notification, cancellation, pool-reuse, sanitized-event, bounded-runtime, extended-code, and one-attempt non-locking markers.

- [x] TODO-05: Migrate core, engine, actions, and agent callers to the refined interface and atomic persistence methods.
  - Procedure: Replace concrete/generic store bounds and separate run/head writes across `crates/workflow/core`, `engine`, `actions`, and `agent`; make persistence-calling helpers async and await all store futures; narrow each consumer's trait requirements; update in-memory stores with async-trait implementations and affected tests with `#[tokio::test]`; then run this exact Bash sequence:
    ```bash
    set -euo pipefail
    cargo test -p cowboy-workflow-core -p cowboy-workflow-actions -p cowboy-workflow-agent -p cowboy-workflow-engine
    if rg -n 'RedbRunStore|SqliteRunStore|trait RunStore|put_object::<|get_object::<|\.put_object\(|\.get_object\(' \
      crates/workflow/core crates/workflow/engine crates/workflow/actions crates/workflow/agent; then
      echo "obsolete concrete or generic store API remains" >&2
      exit 1
    fi
    if rg -n '\.update_run_head\(' \
      crates/workflow/core crates/workflow/engine crates/workflow/actions crates/workflow/agent; then
      echo "separate run-head update remains outside the store backend" >&2
      exit 1
    fi
    if rg -n 'block_on|spawn_blocking' \
      crates/workflow/core crates/workflow/engine crates/workflow/actions crates/workflow/agent \
      crates/workflow/store; then
      echo "synchronous adapter remains around async store work" >&2
      exit 1
    fi
    ```
  - Expected result: The test command exits `0`; all three guarded `rg` checks produce no matches and therefore do not enter their failure branches; production/test consumers contain neither obsolete generic object calls nor `RedbRunStore`, `SqliteRunStore`, or a `RunStore` trait declaration; no caller outside the backend can express a separate `save_run`/`update_run_head` sequence; and no persistence caller bridges async SQLx work through `block_on` or `spawn_blocking`.
  - Implementer observed result: The exact Bash sequence exited `0`. The combined core, actions, agent, and engine suites passed, and all three guarded searches found no obsolete store API, separate run-head update, or synchronous async bridge.

- [x] TODO-06: Switch `WorkflowRuntime` and diagnostic binaries to `SqliteRunStore` while retaining per-run locks and store-wait events.
  - Procedure: The retained TODO subject contains the earlier proposed type name; switch `WorkflowRuntime` and diagnostic binaries to `SqliteWorkflowStore`, not `SqliteRunStore`. Make `WorkflowRuntime::new` async and fallible, await `SqliteWorkflowStore::connect`, await every runtime store operation, update CLI/TUI construction and runtime helpers accordingly, run `store-cli`/`execute-agent`/`engine-cli` under Tokio with awaited calls, replace runtime's atomic-only store-wait generation with the watch sender consumed by per-operation cancellation receivers, and change default database and run-lock fixtures to exact `data.db` and `data.db.locks` paths. Make the tests below print their specified markers, then run:
    ```bash
    set -euo pipefail
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      runtime::tests::two_runtime_instances_share_sqlite_store_for_independent_runs \
      'EVIDENCE runtime-shared-store runtimes=2 runs=2 pools=independent'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      run_lock::tests::run_lock_rejects_same_run_in_process \
      'EVIDENCE run-lock same_run=rejected'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      run_lock::tests::run_lock_allows_different_runs \
      'EVIDENCE run-lock different_runs=allowed'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      run_lock::tests::run_lock_rejects_invalid_ids_before_creating_lock_dir \
      'EVIDENCE run-lock invalid_id=rejected lock_dir_created=false'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      run_lock::tests::run_lock_uses_canonical_uuid_filename_next_to_workflow_store \
      'EVIDENCE run-lock filename=canonical location=data.db.locks'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      run_lock::tests::run_lock_namespace_follows_workflow_store_not_state_dir \
      'EVIDENCE run-lock namespace=workflow_store'
    bash scripts/run-exact-test.sh cowboy-workflow-engine \
      events::tests::workflow_store_waiting_event_round_trips \
      'EVIDENCE wait-event round_trip=true'
    bash scripts/run-exact-test.sh cowboy \
      app::events::tests::renders_workflow_store_waiting_card_with_sanitized_message \
      'EVIDENCE wait-card rendered=true sanitized=true'
    just test-apps
    test -x target/debug/test-apps/workflow-chart
    test -x target/debug/test-apps/store-cli
    test -x target/debug/test-apps/execute-agent
    test -x target/debug/test-apps/acp-chat
    test -x target/debug/test-apps/catalog-cli
    test -x target/debug/test-apps/engine-cli
    if rg -n 'workflow\.sqlite3|workflow\.sqlite3\.locks' \
      crates/workflow/engine crates/workflow/store crates/workflow/agent crates/tui/app; then
      echo "stale workflow.sqlite3 default or fixture remains" >&2
      exit 1
    fi
    rg -q 'data\.db' crates/workflow/engine/src/run_lock.rs
    ```
  - Expected result: Eight exact-test gates each prove one non-ignored test executed and emitted material runtime, lock, event, or rendering evidence; all six diagnostic binaries exist and are executable; the runtime test observes two independent pools sharing one database; lock tests prove same-run rejection, different-run coexistence, safe invalid-id handling, and exact `data.db.locks` derivation; and the final guards reject every stale `workflow.sqlite3` fixture/default.
  - Implementer observed result: The exact Bash sequence exited `0`. Eight gates each executed one non-ignored test with their runtime, lock, event, and rendering markers; all six test-app binaries were executable; and the guards found no stale `workflow.sqlite3` path while confirming `data.db` lock derivation.

- [x] TODO-07: Apply the clean SQLite cutover to configuration defaults and remove the redb backend.
  - Procedure: Change defaults and fixtures to `data.db`; remove `redb_store.rs`, `tables.rs`, redb exports/errors/dependency; ensure `sqlx` is present and `rusqlite` is absent; make the named async config/cutover tests print the evidence markers below; and run this exact Bash sequence:
    ```bash
    set -euo pipefail
    bash scripts/run-exact-test.sh cowboy \
      config::tests::missing_config_uses_sqlite_store_default \
      'EVIDENCE config-default workflow_store=data.db'
    bash scripts/run-exact-test.sh cowboy \
      config::tests::non_sqlite_store_file_is_rejected_without_modification \
      'EVIDENCE clean-cutover rejected=true bytes_unchanged=true'
    deps_file="$(mktemp)"
    trap 'rm -f "$deps_file"' EXIT
    cargo tree --workspace --all-features --edges normal,build,dev >"$deps_file"
    if rg -q '(^|[[:space:]])redb v[0-9]' "$deps_file"; then
      echo "redb remains in the workspace dependency graph" >&2
      exit 1
    fi
    if rg -q '(^|[[:space:]])rusqlite v[0-9]' "$deps_file"; then
      echo "rusqlite remains in the workspace dependency graph" >&2
      exit 1
    fi
    rg -q '(^|[[:space:]])sqlx v0\.8\.' "$deps_file"
    ```
  - Expected result: Each exact gate proves one non-ignored test executed; the markers establish `data.db` as the default and byte-preserving rejection of a legacy/non-SQLite file; `cargo tree` succeeds; guarded searches find neither redb nor rusqlite; and SQLx 0.8 is present.
  - Implementer observed result: The exact Bash sequence exited `0`. Both exact gates executed one non-ignored test and emitted the default-path and byte-preservation markers; the workspace dependency graph contained SQLx 0.8 and no redb or rusqlite package.

- [x] TODO-08: Add and port the full store, agent, runtime, concurrency, and clean-cutover regression coverage.
  - Procedure: Implement every test listed in “Tests to be added/updated”, including async-trait fakes, Tokio tests, concurrent first-open bootstrap, SQLx transaction rollback/pool reuse, two-pool WAL concurrency, extended busy/locked classification and retry cancellation, agent session/prompt behavior, awaited runtime construction/lifecycle, UI rendering, and cutover protection. Make each required test print its manifest marker after assertions. Implement `scripts/run-required-sqlite-tests.sh` to reject blank/malformed/duplicate rows and invoke `scripts/run-exact-test.sh` for every TSV row. Then run this exact Bash sequence, whose here-document is the complete required manifest:
    ```bash
    set -euo pipefail
    expected="$(mktemp)"
    required_out="$(mktemp)"
    trap 'rm -f "$expected" "$required_out"' EXIT
    cat >"$expected" <<'EOF'
    cowboy-workflow-core	traits::tests::workflow_store_capabilities_are_object_safe	EVIDENCE workflow-store-traits object_safe=7 async=true
    cowboy-workflow-store	schema::tests::initializes_new_and_empty_files	EVIDENCE schema-init header=SQLite_format_3 user_version=1 wal=true foreign_keys=true max_connections=4
    cowboy-workflow-store	schema::tests::reopens_supported_schema_version	EVIDENCE schema-reopen user_version=1 tables=7
    cowboy-workflow-store	schema::tests::rejects_future_schema_version_without_modifying_file	EVIDENCE schema-future rejected=true bytes_unchanged=true
    cowboy-workflow-store	schema::tests::rejects_non_sqlite_file_without_modifying_file	EVIDENCE schema-non-sqlite rejected=true bytes_unchanged=true
    cowboy-workflow-store	schema::tests::concurrent_first_connects_initialize_one_schema	EVIDENCE schema-concurrent connects=2 user_version=1 tables=7
    cowboy-workflow-store	contract::tests::run_head_round_trip_and_deterministic_listing	EVIDENCE contract-run-head round_trip=true deterministic=true
    cowboy-workflow-store	contract::tests::source_step_hashes_and_reopen_durability	EVIDENCE contract-objects source_hash=true step_hash=true reopen=true
    cowboy-workflow-store	contract::tests::role_session_and_turn_ordering	EVIDENCE contract-agent session_crud=true turn_order=true
    cowboy-workflow-store	contract::tests::run_deletion_retains_objects_and_low_level_delete_removes_them	EVIDENCE contract-delete mutable_removed=true immutable_retained=true explicit_delete=true
    cowboy-workflow-store	sqlite_store::tests::save_run_updates_run_and_head_atomically	EVIDENCE transaction-run-head committed=true snapshots_match=true
    cowboy-workflow-store	sqlite_store::tests::completed_step_transaction_rolls_back_on_injected_failure	EVIDENCE transaction-step rollback=true pool_reusable=true
    cowboy-workflow-store	sqlite_store::tests::prompt_append_and_compare_and_seal_are_totally_ordered	EVIDENCE prompt-order legal_outcomes=2 partial_state=false
    cowboy-workflow-store	sqlite_store::tests::immutable_object_collision_is_rejected_without_overwrite	EVIDENCE object-collision rejected=true bytes_unchanged=true
    cowboy-workflow-store	sqlite_store::tests::store_wait_observer_fires_once_for_contended_write	EVIDENCE store-wait observer_count=1 retries_at_least=1 path_leaked=false
    cowboy-workflow-store	sqlite_store::tests::store_wait_cancellation_interrupts_contended_write	EVIDENCE store-cancel cancelled=true pool_reusable=true
    cowboy-workflow-store	sqlite_store::tests::extended_busy_code_is_retryable	EVIDENCE sqlite-code value=517 primary=5 retryable=true
    cowboy-workflow-store	sqlite_store::tests::extended_locked_code_is_retryable	EVIDENCE sqlite-code value=262 primary=6 retryable=true
    cowboy-workflow-store	sqlite_store::tests::non_locking_sqlx_errors_are_not_retried	EVIDENCE sqlite-code non_locking=true attempts=1
    cowboy-workflow-store	sqlite_store::tests::two_pools_share_wal_reads_writes_and_reuse_connections	EVIDENCE wal-concurrency pools=2 readers=concurrent writes=visible reusable=true
    cowboy-workflow-store	sqlite_store::tests::prompt_windows_preserve_content_sequences_rejections_and_cleanup	EVIDENCE prompt-window content=exact sequence=monotonic cleanup=true
    cowboy-workflow-agent	executor::tests::loads_persisted_role_session	EVIDENCE agent-session persisted_load=true
    cowboy-workflow-agent	executor::tests::ensure_session_fresh_then_reused	EVIDENCE agent-session fresh=true reused=true
    cowboy-workflow-agent	executor::tests::reused_session_omits_role_and_advances_watermark	EVIDENCE agent-watermark advanced=true role_resent=false
    cowboy-workflow-agent	executor::tests::prompt_window_controls_cleanup_on_success_and_backend_error	EVIDENCE agent-prompt-cleanup success=true backend_error=true
    cowboy-workflow-agent	executor::tests::dropped_execution_removes_prompt_window_control_and_aborts_window	EVIDENCE agent-drop control_removed=true window_aborted=true
    cowboy-workflow-engine	runner::tests::lua_provider_exposes_previous_step_output	EVIDENCE runner-prev output_exposed=true
    cowboy-workflow-engine	runner::tests::retry_reservation_is_saved_before_dispatch	EVIDENCE runner-retry reserved_before_dispatch=true
    cowboy-workflow-engine	runtime::tests::two_runtime_instances_share_sqlite_store_for_independent_runs	EVIDENCE runtime-shared-store runtimes=2 runs=2 pools=independent
    cowboy-workflow-engine	runtime::tests::workflow_store_wait_event_emits_once_during_contended_resume	EVIDENCE runtime-wait-event count=1 sanitized=true
    cowboy-workflow-engine	runtime::tests::cancelling_runtime_store_wait_interrupts_contended_operation	EVIDENCE runtime-wait-cancel cancelled=true bounded=true
    cowboy-workflow-engine	runtime::tests::cancellation_cleanup_retains_run_lock_until_persistence_finishes	EVIDENCE runtime-cancel lock_retained_until_persisted=true
    cowboy-workflow-engine	runtime::tests::list_runs_reads_persisted_head_summaries_without_full_runs	EVIDENCE runtime-list source=heads full_run_loads=0
    cowboy-workflow-engine	runtime::tests::answer_run_persists_ask_user_completion_when_resumed_step_fails	EVIDENCE runtime-answer prompt_completion=persisted resumed_step=failed
    cowboy-workflow-engine	runtime::tests::resolve_run_restores_persisted_request_topic_and_exposes_fields_to_next_step	EVIDENCE runtime-resolve topic_restored=true fields_exposed=true
    cowboy-workflow-engine	runtime::tests::failed_runner_events_are_persisted_with_active_duration	EVIDENCE runtime-events persisted=true active_duration=true
    cowboy-workflow-engine	run_lock::tests::run_lock_rejects_same_run_in_process	EVIDENCE run-lock same_run=rejected
    cowboy-workflow-engine	run_lock::tests::run_lock_allows_different_runs	EVIDENCE run-lock different_runs=allowed
    cowboy-workflow-engine	run_lock::tests::run_lock_rejects_invalid_ids_before_creating_lock_dir	EVIDENCE run-lock invalid_id=rejected lock_dir_created=false
    cowboy-workflow-engine	run_lock::tests::run_lock_uses_canonical_uuid_filename_next_to_workflow_store	EVIDENCE run-lock filename=canonical location=data.db.locks
    cowboy-workflow-engine	run_lock::tests::run_lock_namespace_follows_workflow_store_not_state_dir	EVIDENCE run-lock namespace=workflow_store
    cowboy-workflow-engine	events::tests::workflow_store_waiting_event_round_trips	EVIDENCE wait-event round_trip=true
    cowboy	app::events::tests::renders_workflow_store_waiting_card_with_sanitized_message	EVIDENCE wait-card rendered=true sanitized=true
    cowboy	config::tests::missing_config_uses_sqlite_store_default	EVIDENCE config-default workflow_store=data.db
    cowboy	config::tests::non_sqlite_store_file_is_rejected_without_modification	EVIDENCE clean-cutover rejected=true bytes_unchanged=true
    EOF
    sed -i 's/^    //' "$expected"
    diff -u "$expected" scripts/required-sqlite-tests.tsv
    test "$(awk -F '\t' 'NF == 3 {count++} END {print count + 0}' scripts/required-sqlite-tests.tsv)" -eq 45
    test -z "$(cut -f1,2 scripts/required-sqlite-tests.tsv | sort | uniq -d)"
    bash scripts/run-required-sqlite-tests.sh | tee "$required_out"
    test "$(rg -c '^EXACT_TEST_OK ' "$required_out")" -eq 45
    while IFS=$'\t' read -r package test_name marker; do
      test -n "$marker"
      rg -qF "EXACT_TEST_OK $package $test_name" "$required_out"
    done <scripts/required-sqlite-tests.tsv
    cargo test -p cowboy-workflow-store
    cargo test -p cowboy-workflow-agent
    cargo test -p cowboy-workflow-actions
    cargo test -p cowboy-workflow-engine
    cargo test -p cowboy
    ```
  - Expected result: The manifest diff is empty and contains exactly 45 unique package/test pairs; the runner emits exactly 45 `EXACT_TEST_OK` records and every manifest package/test pair has one matching record, proving every row passed the exact list/execution/ignored/marker gates. Removing, renaming, ignoring, duplicating, or skipping a required regression makes the procedure fail before or during the full suites; all five affected suites also pass. This automated gate does not claim to measure assertion strength.
  - Implementer observed result: The exact Bash sequence exited `0`. The manifest matched the approved 45 unique rows, the runner emitted exactly 45 matching `EXACT_TEST_OK` records, every required test passed its list/execution/ignored/marker gate, and all five affected full suites passed; no assertion-strength claim was inferred from the automated gate.

- [x] TODO-09: Update authoritative documentation and examples for SQLite and the refined store architecture.
  - Procedure: Update `README.md`, `demo-config.toml`, `docs/architecture.md`, `docs/module-map.md`, and `AGENTS.md`, then run:
    ```bash
    set -euo pipefail
    if rg -n 'workflow\.redb|workflow\.sqlite3|redb-backed|RedbRunStore|SqliteRunStore|currently `redb`' \
      README.md demo-config.toml AGENTS.md \
      docs/architecture.md docs/module-map.md crates; then
      echo "stale active redb documentation or code remains" >&2
      exit 1
    fi
    rg -n 'workflow\.redb|workflow\.sqlite3|redb-backed|RedbRunStore|SqliteRunStore|currently `redb`' docs/plans \
      || test "$?" -eq 1
    require() {
      file="$1"
      pattern="$2"
      label="$3"
      if ! rg -qi "$pattern" "$file"; then
        echo "$file is missing required documentation: $label" >&2
        exit 1
      fi
    }
    require README.md 'data\.db' 'default database filename'
    require README.md 'workflow_store' 'configuration key'
    require README.md 'SQLx|sqlx' 'SQLx backend'
    require README.md 'not automatically migrated|no automatic migration|clean cutover' 'redb cutover boundary'
    require docs/architecture.md 'SqlitePool|SQLx' 'async SQLx pool'
    require docs/architecture.md 'async|Tokio' 'asynchronous runtime integration'
    require docs/architecture.md 'WorkflowStore' 'refined store interface'
    require docs/architecture.md 'user_version|schema version' 'schema versioning'
    require docs/architecture.md 'WAL' 'WAL policy'
    require docs/architecture.md 'SQLITE_BUSY|busy/locked|SQLITE_LOCKED' 'contention retry'
    require docs/architecture.md 'cancel' 'cancellable contention wait'
    require docs/architecture.md 'WorkflowStoreWaiting' 'wait event'
    require docs/architecture.md 'data\.db\.locks' 'sidecar lock namespace'
    require docs/architecture.md 'transaction|atomic' 'atomic persistence'
    require docs/module-map.md 'sqlite_store\.rs' 'SQLite implementation module'
    require docs/module-map.md 'schema\.rs' 'schema module'
    require docs/module-map.md 'SqliteWorkflowStore' 'concrete store name'
    require docs/module-map.md 'SQLx|sqlx' 'SQLx backend'
    require docs/module-map.md 'WorkflowStateStore|WorkflowObjectStore' 'capability traits'
    require AGENTS.md 'data\.db' 'default database filename'
    require AGENTS.md 'SQLx|sqlx' 'SQLx backend'
    require AGENTS.md 'WorkflowStore' 'generic store name'
    require AGENTS.md 'WAL' 'WAL policy'
    require AGENTS.md 'data\.db\.locks' 'sidecar locks'
    require AGENTS.md 'not automatically migrated|no automatic conversion|clean cutover' 'cutover boundary'
    require demo-config.toml 'workflow_store[[:space:]]*=[[:space:]]*".*data\.db"' 'example database path'
    ```
  - Expected result: The negative active-source guard finds no stale redb or `workflow.sqlite3` names; historical-plan matches are only printed; every positive `require` call succeeds, proving the authoritative files explicitly document `data.db`, the configuration key, SQLx/Tokio persistence, generic/capability store names, pool and transaction ownership, schema versioning, WAL, BUSY/LOCKED cancellation, wait events, `data.db.locks`, atomic persistence, and clean cutover.
  - Implementer observed result: The exact Bash sequence exited `0` after adding the missing explicit wait-event, default `data.db.locks`, and capability-trait documentation. The active-source guard found no stale names, historical matches were only printed, and every positive documentation requirement passed.

- [x] TODO-10: Complete focused tests, multi-process smoke checks, test-app builds, and warning-free linting.
  - Procedure: Update `scripts/verify-sqlite-store.sh` so its focused-test phase calls `scripts/run-required-sqlite-tests.sh` instead of raw `cargo test ... -- --exact` filters. From the repository root, run:
    ```bash
    set -euo pipefail
    bash scripts/verify-sqlite-store.sh
    if rg -n -- '-- --exact' scripts/verify-sqlite-store.sh; then
      echo "verification script still contains an unguarded exact-test filter" >&2
      exit 1
    fi
    rg -qF 'run-required-sqlite-tests.sh' scripts/verify-sqlite-store.sh
    ```
  - Expected result: The script exits `0` and its `EXIT` trap removes only the uniquely created `tmp_root`; its required-test phase proves all 45 manifest tests exist, execute once, are not ignored, and emit evidence; Tokio-based `store-cli` asynchronously connects through SQLx, observes the SQLite header, round-trips the run and step, appends a turn, removes the run, and still reads the immutable step; two `engine-cli` PIDs with independent SQLx pools overlap deterministically, both runs are visible and complete after release, and Python reports `PRAGMA integrity_check = ok`; a separate process-held sidecar lock rejects a second process; the non-SQLite file digest is unchanged; all crate suites pass; Clippy emits no warnings; and the final guards prove no unguarded exact filter remains.
  - Implementer observed result: The final exact Bash sequence exited `0`. The verifier built all test apps, passed all 45 required exact gates, completed the typed store smoke and immutable-retention checks, overlapped two independent engine processes, obtained `ok` from `PRAGMA integrity_check`, rejected a same-run second process, preserved the rejected non-SQLite digest, passed all crate suites and Clippy with `-D warnings`, and the final guards found no unguarded exact filter.
