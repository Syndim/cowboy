# Plan

Allow independent Cowboy processes to share the same state directory while preventing two processes from advancing the same workflow run.

Repository inspection shows the lock is caused by handle lifetime. `RedbRunStore` currently owns `Arc<redb::Database>`, and `WorkflowRuntime` caches that store in `Arc<Mutex<Option<RedbRunStore>>>`. redb 4.1.0 takes an exclusive OS lock for writable `Database` handles and returns `DatabaseError::DatabaseAlreadyOpen` when another process opens the same database. A TUI process therefore keeps `workflow.redb` locked for its whole lifetime, so a second `cowboy` process cannot list runs or start a different run.

The fix has two parts:

- make database access transient and retryable, so all Cowboy instances serialize short redb operations through one `workflow.redb` file without holding the file lock while idle or waiting for an agent;
- add explicit per-run execution ownership, so concurrent instances are rejected only when they try to advance the same `run_id`.

Keep the configured `workflow_store` path and current redb schema. Do not shard the database for this bug fix. Refactor `RedbRunStore` into a cloneable path-backed store that opens redb inside each store method, performs one short transaction, commits, and drops the database handle before returning. Add bounded retry/backoff around `DatabaseAlreadyOpen` during open/create so overlapping quick operations wait briefly instead of surfacing a database-lock error.

Add a run execution guard in the engine layer. The store remains a persistence primitive; it should not know which calls are workflow execution. `WorkflowRuntime` should acquire a guard before operations that can advance a run: `start_run`, `start_run_stepwise`, `resume_run`, `step_run`, and `answer_run`. The guard should combine process-wide active-run tracking with an OS file lock under a sidecar lock directory derived from the configured `workflow_store`, held for the full execution window including agent calls.

Do not build lock paths from raw user input. `resume_run`, `step_run`, and `answer_run` accept user-supplied run ids, so the guard must first validate the id as the generated `run-<uuid>` format and then build the lock filename from the parsed UUID's canonical string, e.g. `<workflow_store>.locks/run-<canonical-uuid>.lock`. Invalid/path-like ids such as `../run-x`, `/tmp/run-x`, or `run-../../x` must fail before any lock path is created and before database loading. This prevents path traversal through lock filenames while preserving current generated run ids from `format!("run-{}", Uuid::new_v4())`.

Read-only operations should remain lock-free at the run-execution layer: `list_runs`, `load_run`, catalog loading, event loading, and display paths can open redb transiently but should not acquire per-run execution locks. Event log writes are per-run JSON files; the run execution guard prevents same-run event-log races during advancement.

# Changes

- Refactor `crates/workflow/store/src/redb_store.rs` so `RedbRunStore` stores the database `PathBuf` instead of `Arc<Database>`.
- Keep `RedbRunStore::create` and `RedbRunStore::open` public APIs, but make them initialize or validate the database and then drop the redb handle before returning the path-backed store.
- Use redb's create/open behavior deliberately:
  - prefer `Database::create(path)` for normal method access because redb can initialize a missing or empty file and open an existing valid database;
  - preserve immediate failures for corruption, invalid files, and non-lock database errors.
- Add private store helpers in `redb_store.rs`:
  - ensure the workflow-store parent directory exists;
  - open the database with bounded retry/backoff when the error is `redb::DatabaseError::DatabaseAlreadyOpen`;
  - run read or write closures and drop the database handle before returning.
- Convert every store method to transient database access:
  - `save_run`, `load_run`, `list_runs`;
  - `put_object`, `get_object`, `delete_object`;
  - `update_run_head`, `load_run_head`;
  - `save_role_session`, `load_role_session`, `delete_role_sessions`;
  - `append_turn`, `delete_run`.
- Keep each redb transaction scope short and synchronous. No redb handle or transaction may be held across an `.await` boundary.
- Update store error mapping so retry exhaustion on `DatabaseAlreadyOpen` reports the workflow store as temporarily busy instead of exposing raw redb wording where possible.
- Simplify `crates/workflow/engine/src/runtime.rs` store ownership:
  - remove `store: Arc<Mutex<Option<RedbRunStore>>>` from `WorkflowRuntime`;
  - replace `open_store()` with a cheap path-backed `store()` constructor that ensures the parent directory and returns `RedbRunStore::create(&config.workflow_store)`;
  - keep existing runtime call sites receiving a cloneable store value.
- Add a run execution lock type in `crates/workflow/engine`:
  - use process-wide active-run tracking keyed by the workflow-store lock namespace and canonical run id;
  - create a sidecar `<workflow_store>.locks` directory as needed;
  - validate run ids before path construction by accepting only `run-<uuid>` and parsing the UUID with the existing `uuid` dependency;
  - construct lock filenames from the parsed canonical UUID, never from raw user input;
  - acquire an exclusive non-blocking MSRV-compatible file lock on `<workflow_store>.locks/run-<canonical-uuid>.lock`;
  - release both the OS lock and in-process set entry on guard `Drop`.
- Add a helper such as `parse_run_lock_id(run_id: &str) -> Result<Uuid>` or `safe_run_lock_path(run_id: &str) -> Result<PathBuf>` in the engine lock module. Keep it private unless tests need `pub(crate)` visibility.
- Map same-run lock contention to a domain error message such as `run <id> is already active in another Cowboy instance`; it must not mention redb.
- Map invalid run id formats to a clear validation error before creating any lock file or loading from the store.
- Hold the run execution guard around run-advancing runtime paths:
  - in `start_with`, generate the new `run-<uuid>` first, acquire its guard, then persist and execute the run;
  - in `resume_with`, validate and acquire the guard before loading or executing the run;
  - in `answer_run`, validate and acquire the guard before mutating the waiting run and keep it through any automatic resume;
  - keep `list_runs`, `load_run`, `catalog`, `load_events`, and read-only display paths guard-free.
- Ensure `AgentExecutor` continues to receive a cloneable store, but that store opens redb only inside role-session methods.
- Preserve persisted table names and serialized `WorkflowRun`, `RunHead`, object, role-session, and turn formats; no migration should be required.
- Update comments and module docs that currently imply `RedbRunStore` owns an open redb handle.

# Tests to be added/updated

- Add a red-capable store regression test in `crates/workflow/store/src/redb_store.rs` proving one live `RedbRunStore` value does not keep the database locked:
  - create `store_a`, save a run/head, and keep `store_a` alive;
  - create/open `store_b` for the same path;
  - load/list through `store_b` and write a second run;
  - assert both stores can read both runs.
- Add a store test proving `RedbRunStore::create` handles an existing valid database path without a pre-check race.
- Add store coverage for retry helper behavior if it can be unit-tested without sleeping; otherwise rely on the live two-store regression for the lock symptom and keep the retry helper small/private.
- Add an engine runtime test with two `WorkflowRuntime` instances sharing one `RuntimeConfig` and state directory:
  - use a deterministic status-only Lua workflow;
  - call `runtime_a.start_run("first")` and keep `runtime_a` alive;
  - call `runtime_b.start_run("second")`;
  - assert both complete and `list_runs()` returns both runs.
- Add an engine lock test proving two guards for the same valid `run-<uuid>` cannot be held at once in the same process.
- Add an engine lock test proving guards for two different valid run ids can be held at the same time.
- Add an engine lock test proving path-like or malformed run ids are rejected before lock path construction:
  - `../run-00000000-0000-0000-0000-000000000000`;
  - `/tmp/run-00000000-0000-0000-0000-000000000000`;
  - `run-../../00000000-0000-0000-0000-000000000000`;
  - `run-not-a-uuid`.
- Add an engine/runtime test proving `step_run` or `answer_run` with an invalid/path-like run id returns the validation error and does not create files outside the workflow-store sidecar lock directory.
- Add an engine/runtime same-run contention test returning the clear `already active` workflow error instead of `Database already open`. If full async runtime contention is awkward, test the guard directly and one runtime path's error mapping.
- Update existing store tests only where they assumed `RedbRunStore::open` owns a long-lived database handle. `committed_data_survives_reopen` should keep the same behavioral assertions.
- No TUI rendering tests should need changes unless the user-facing error status formatting changes; if it does, update only focused affected assertions.

# How to verify

- Run `cargo test -p cowboy-workflow-store redb_store`.
- Run `cargo test -p cowboy-workflow-store committed_data_survives_reopen`.
- Run `cargo test -p cowboy-workflow-engine run_lock` if the lock guard tests live in a separate module with that filter.
- Run `cargo test -p cowboy-workflow-engine runtime`.
- Run `cargo test -p cowboy-workflow-agent session` to verify path-backed store access still supports role session reuse.
- Manually smoke test multiple instances against one workflow store:
  - start one Cowboy instance and leave it open;
  - from another shell run an independent single-step run;
  - from a third shell run another independent single-step run;
  - confirm both commands create different run ids without `Database already open. Cannot acquire lock.`;
  - attempt to advance the same running run while its sidecar lock is held and confirm the caller gets the clear same-run active error.
- Manually smoke test invalid run ids:
  - run `cowboy step ../run-00000000-0000-0000-0000-000000000000`;
  - confirm it returns a validation error and no escaped lock file appears outside the workflow-store sidecar lock directory.

# Manual verification evidence

The manual smoke checks were run with deterministic `engine-cli` against one shared workflow store at `/tmp/cowboy-review-evidence-smoke/state/workflow.redb`.

- Multi-instance independent run commands:
  - `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- run-step first independent run`
    - result: `run=run-d1d8bf5c-0f6e-436e-8b75-c0e8c008772a workflow=aaa status=Completed step=start steps_executed=1`
  - `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- run-step second independent run`
    - result: `run=run-83f59550-6c39-4961-85ca-807f709c0b27 workflow=aaa status=Completed step=start steps_executed=1`
  - confirmed: different run ids; no `Database already open`; no `Cannot acquire lock`.
- Same-run contention command while holding `/tmp/cowboy-review-evidence-smoke/state/workflow.redb.locks/run-669b8ebb-8061-4007-bcfb-2ac4b00714c4.lock`:
  - seed command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- run-step same run seed`
    - result: `run=run-669b8ebb-8061-4007-bcfb-2ac4b00714c4 workflow=aaa status=Running step=done steps_executed=1`
  - contention command: `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- step run-669b8ebb-8061-4007-bcfb-2ac4b00714c4`
    - result: `error: invalid action: run run-669b8ebb-8061-4007-bcfb-2ac4b00714c4 is already active in another Cowboy instance`
  - confirmed: clear same-run active error; no redb lock wording.
- Invalid/path-like run id command:
  - `cargo run -p cowboy-workflow-engine --quiet --bin engine-cli -- step ../run-00000000-0000-0000-0000-000000000000`
    - result: `error: invalid action: invalid run id "../run-00000000-0000-0000-0000-000000000000"; expected run-<uuid>`
  - confirmed: sidecar lock dir was `/tmp/cowboy-review-evidence-smoke/state/workflow.redb.locks`; escaped lock file existed: `false`.

# TODO

- [x] Refactor `RedbRunStore` to store the workflow database path instead of `Arc<redb::Database>`.
- [x] Add transient redb open helpers with bounded `DatabaseAlreadyOpen` retry/backoff.
- [x] Add transient read/write transaction helpers that drop the database handle before returning.
- [x] Update `RedbRunStore::create` and `RedbRunStore::open` to initialize or validate without retaining the redb handle.
- [x] Convert `save_run` to open, write, commit, and drop the database inside the method.
- [x] Convert `load_run` to open, read, and drop the database inside the method.
- [x] Convert `list_runs` to open, read heads, and drop the database inside the method.
- [x] Convert object storage methods `put_object`, `get_object`, and `delete_object` to transient database access.
- [x] Convert run-head methods `update_run_head` and `load_run_head` to transient database access.
- [x] Convert role-session methods `save_role_session`, `load_role_session`, and `delete_role_sessions` to transient database access.
- [x] Convert `append_turn` and `delete_run` to transient database access.
- [x] Improve retry-exhaustion error text for temporary workflow-store lock contention.
- [x] Remove the cached store field from `WorkflowRuntime`.
- [x] Replace `WorkflowRuntime::open_store()` with cheap path-backed store construction.
- [x] Add an engine run execution lock type with same-process active-run tracking.
- [x] Add run id validation for the generated `run-<uuid>` format before creating lock paths.
- [x] Build lock filenames from parsed canonical UUIDs instead of raw run id strings.
- [x] Add OS file-lock acquisition under `<workflow_store>.locks/run-<canonical-uuid>.lock` for cross-process same-run protection.
- [x] Map invalid run id formats to a clear workflow validation error before database loading.
- [x] Map same-run lock contention to a clear workflow error that does not mention redb.
- [x] Acquire the run execution guard for newly generated run ids in `start_with`.
- [x] Acquire the run execution guard before loading or executing existing runs in `resume_with`.
- [x] Acquire the run execution guard around answer mutation and automatic resume in `answer_run`.
- [x] Keep list/load/catalog/event read paths free of run execution locks.
- [x] Keep `AgentExecutor` role-session access working with the path-backed store.
- [x] Update comments and module documentation that describe a long-lived open redb handle.
- [x] Add the two-live-store regression test in `cowboy-workflow-store`.
- [x] Add store coverage for existing-path create/open behavior after the refactor.
- [x] Add engine runtime coverage for two `WorkflowRuntime` instances starting independent runs against one state dir.
- [x] Add engine lock coverage for same-run rejection and different-run coexistence.
- [x] Add engine lock coverage for invalid/path-like run ids and lock path containment.
- [x] Add runtime coverage proving invalid/path-like run ids do not create escaped lock files.
- [x] Run focused store and engine verification commands listed above.
- [x] Run the focused agent session verification command listed above.
- [x] Run the manual multi-instance smoke test against one workflow store.
- [x] Run the manual same-run contention smoke test while the sidecar lock is held.
- [x] Run the manual invalid/path-like run id smoke test.
