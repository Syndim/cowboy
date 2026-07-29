# Plan: Cowboy hangs after `/exit`

Bug work folder: `docs/plans/cowboy_hangs_after_exit_command/`
RCA: [`rca.md`](./rca.md)
Regression test (input to this fix, do **not** rewrite):
`crates/agent/acp/tests/stdio_shutdown.rs::force_terminate_releases_stdout_when_agent_left_descendants`

## Plan

The RCA establishes three independent defects that combine into the hang:

1. `StdioTransport` only terminates the directly spawned agent process. The
   agent's descendants inherit the stdout write handle, so the transport's
   `Lines::next_line()` read never reaches EOF
   (`crates/agent/acp/src/transport/stdio.rs`).
2. `/exit` never tears down in-flight agent work; it only flips a flag
   (`crates/tui/app/src/app/commands.rs`, `SlashCommand::Exit`). The RCA's
   controlled experiment proves aborting the background task is not sufficient,
   because the pending stdout read lives in Tokio's blocking pool.
3. `main` is `#[tokio::main]`, so runtime drop waits **unbounded** for that
   mandatory blocking read (`crates/tui/app/src/main.rs`).

The fix addresses all three, layered per `AGENTS.md`:

- **`cowboy-agent-acp` — own and kill the agent *process tree*.** Add a
  platform-specific `ProcessTreeScope` that is configured on the `Command`
  before spawn and attached to the spawned child. Windows uses a Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; Unix puts the child in its own process
  group and signals the group. `StdioTransport::force_terminate` / `close` /
  drop kill the whole tree, which closes the last write handle on the stdout
  pipe and lets the pending read observe EOF. This is what makes the existing
  regression test pass and also stops leaving orphaned agent descendants behind.
- **`cowboy-agent-acp` — a process-wide registry of live agent trees.** Live
  transports are owned by `AgentExecutor` clients inside background tasks, so
  the exit path cannot reach them through ordinary references. Register each
  spawned tree on connect, deregister on close/drop, and expose a bounded
  `terminate_all_agent_processes(timeout)` entry point.
- **`cowboy-workflow-engine` — deterministic, bounded runtime shutdown.** Add
  `WorkflowRuntime::shutdown(timeout)` that cancels store waits, terminates all
  live agent process trees through the `AcpConnector` seam (so tests can fake
  it), and closes the SQLite pool. Bounded and idempotent; run state and event
  logs are already persisted per step, and the pool close happens last so no
  in-flight persistence is dropped.
- **`crates/tui/app` — exit dispatch only.** `/exit` aborts in-flight background
  tasks (without adopting `/cancel`'s user-facing "Cancelled" card, status text,
  or durable-status mutation) and `run_tui` awaits `runtime.shutdown(...)` after
  the loop returns and **before** `finish_tui`, preserving the existing
  restore-then-print-resume-hint ordering.
- **`crates/tui/app` — belt-and-braces bounded process exit.** Replace
  `#[tokio::main]` with an explicit multi-thread runtime, `block_on` the real
  work (so all persistence completes), then `Runtime::shutdown_timeout(...)`.
  Returning from `main` then terminates the process even if a detached blocking
  read is still parked.

Non-goals: no change to `/cancel` semantics, no change to watchdog recovery
policy, no change to the Zellij transport's termination contract beyond keeping
it compiling and green.

## Changes

### `crates/agent/acp`

- `Cargo.toml`: add target-specific dependencies —
  `[target.'cfg(windows)'.dependencies] windows-sys` (features for
  `Win32_Foundation`, `Win32_System_JobObjects`, `Win32_System_Threading`) and
  `[target.'cfg(unix)'.dependencies] libc`.
- New `src/process_tree.rs`:
  - `pub(crate) struct ProcessTreeScope` with
    `new() -> anyhow::Result<Self>`, `configure(&self, cmd: &mut Command)`,
    `attach(&self, child: &Child) -> anyhow::Result<()>`,
    `terminate(&self) -> anyhow::Result<()>` (idempotent), and `Drop` that
    terminates the tree.
  - Windows: `CreateJobObjectW` + `SetInformationJobObject` with
    `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `AssignProcessToJobObject` on the
    child handle, `TerminateJobObject` on terminate, `CloseHandle` on drop.
    All FFI wrapped in small `unsafe` blocks with explanatory comments.
  - Unix: `std::os::unix::process::CommandExt::process_group(0)` in
    `configure`; `libc::killpg(pgid, SIGKILL)` in `terminate`.
  - Any other platform: no-op `configure`/`attach`, `terminate` returns `Ok(())`
    and the caller falls back to killing the direct child only.
- New `src/agent_processes.rs` (or a submodule of `process_tree`):
  - Process-wide registry (`OnceLock<Mutex<HashMap<RegistrationId, Weak/Arc<ProcessTreeScope>>>>`).
  - `register(scope) -> RegistrationId`, `deregister(id)`, and
    `pub async fn terminate_all_agent_processes(timeout: Duration) -> usize`
    returning how many trees were terminated; never panics, logs failures.
- `src/transport/stdio.rs`:
  - Hold `Arc<ProcessTreeScope>` plus its `RegistrationId` in `StdioTransport`.
  - `connect` creates the scope, calls `configure` before `spawn`, `attach`
    after spawn, and registers it. If `attach` fails, kill the child and return
    an error rather than proceeding with an unmanaged tree.
  - `force_terminate` terminates the tree first, then awaits `child.wait()`, and
    deregisters. `close` does the same on a best-effort basis. Keep
    `kill_on_drop(true)`; drop of the scope kills any survivors.
  - Keep existing log fields (`command`, `pid`) and add `tree_terminated`.
- `src/lib.rs`: export the public shutdown entry point
  (`pub use agent_processes::terminate_all_agent_processes;`).

### `crates/workflow/engine`

- `src/runtime_dependencies.rs`: add
  `async fn terminate_all_agents(&self, timeout: Duration) -> usize` to the
  `AcpConnector` trait with a default implementation returning `0`;
  `ProductionAcpConnector` delegates to
  `cowboy_agent_acp::terminate_all_agent_processes`.
- `src/runtime.rs`: add
  `pub async fn shutdown(&self, timeout: Duration)` — cancel store waits,
  `tokio::time::timeout(timeout, self.acp_connector.terminate_all_agents(..))`,
  then `self.store.close().await` (also bounded). Idempotent, never returns an
  error, logs a warning on timeout.
- `src/lib.rs`: re-export any new public type/constant needed by the TUI (e.g. a
  `DEFAULT_SHUTDOWN_TIMEOUT`).

### `crates/tui/app`

- `src/app/state.rs`: add
  `pub(in crate::app) fn abort_background_tasks_for_exit(&mut self)` that aborts
  and clears background handles without pushing a card, without changing
  `status`, and without setting `RunStatusState::Cancelled` (so `resume_hint()`
  still offers `cowboy resume <run-id>`). `cancel_background_tasks` is unchanged
  and may delegate to a shared private abort helper.
- `src/app/commands.rs`: `SlashCommand::Exit` calls
  `runtime.cancel_store_waits()` and `state.abort_background_tasks_for_exit()`
  in addition to the existing flag/status/card behavior.
- `src/app.rs`: after `run_loop` returns, replace the bare
  `runtime.cancel_store_waits()` with an awaited
  `runtime.shutdown(SHUTDOWN_TIMEOUT)` call placed **before** `finish_tui`;
  `finish_tui`'s restore-then-hint ordering is untouched.
- `src/lib.rs` (new small module, e.g. `src/process_exit.rs`): helper
  `run_with_bounded_shutdown<F>(future_builder, shutdown_timeout) -> T` that
  builds a multi-thread Tokio runtime, `block_on`s the work, and calls
  `Runtime::shutdown_timeout`.
- `src/main.rs`: drop `#[tokio::main]`; call the helper, keep the existing error
  logging and `std::process::exit(1)` behavior on error.

### Docs

- `docs/architecture.md` and/or `docs/module-map.md`: document the new
  process-tree ownership responsibility in `cowboy-agent-acp` and the
  `WorkflowRuntime::shutdown` teardown path.

## Tests to be added/updated

- **Unchanged gate (do not edit):**
  `crates/agent/acp/tests/stdio_shutdown.rs::force_terminate_releases_stdout_when_agent_left_descendants`
  must flip from FAIL to PASS.
- **New** `crates/agent/acp/src/process_tree.rs` unit tests:
  - `terminate_kills_descendants` — spawn a child that spawns a longer-lived
    grandchild; after `terminate()`, the grandchild is gone (observable via the
    inherited pipe reaching EOF).
  - `terminate_is_idempotent` — two `terminate()` calls both return `Ok`.
  - `drop_terminates_tree` — dropping the scope kills a live tree.
- **New** `crates/agent/acp` registry tests:
  - `terminate_all_agent_processes_terminates_registered_transport` — a
    connected `StdioTransport`'s pending `recv()` completes after
    `terminate_all_agent_processes`.
  - `closed_transport_is_deregistered` — after `close()`,
    `terminate_all_agent_processes` reports zero terminations.
- **Updated** `crates/agent/acp/src/transport/stdio.rs` unit tests: existing
  tests (`test_connect_echo`, `test_recv_eof`,
  `force_terminate_stops_stdio_child_by_pid`, …) keep passing unmodified except
  where new struct fields require construction changes.
- **New** `crates/workflow/engine` runtime tests:
  - `shutdown_terminates_agents_and_closes_store` — fake `AcpConnector` records
    one `terminate_all_agents` call; store pool is closed afterwards.
  - `shutdown_is_bounded_when_termination_hangs` — fake connector that sleeps
    far past the timeout; `shutdown` still returns within the bound.
  - `shutdown_is_idempotent` — calling twice does not panic or error.
- **New/updated** `crates/tui/app` tests:
  - `exit_aborts_background_tasks` — after `/exit`, no background tasks remain.
  - `exit_does_not_emit_cancel_card_or_cancel_status` — `/exit` transcript and
    durable run status differ from `/cancel` (guards the preserved `/cancel`
    behavior).
  - `exit_preserves_resume_hint` — `resume_hint()` after `/exit` on a running
    run still yields `cowboy resume <run-id>`.
  - existing `finish_tui` ordering tests remain unmodified and green.
- **New** `crates/tui/app` bounded-shutdown helper test:
  - `bounded_shutdown_returns_despite_stuck_blocking_task` — the helper returns
    within the timeout even when a `spawn_blocking` task never completes.

## How to verify

Run from the repository root:

```bash
cargo test -p cowboy-agent-acp --test stdio_shutdown
cargo test -p cowboy-agent-acp
cargo test -p cowboy-workflow-engine
cargo test -p cowboy
cargo clippy --workspace --all-targets -- -D warnings
cargo build
```

Manual end-to-end check (matches the RCA reproduction steps):

1. Start `cowboy` (TUI) with a configured stdio agent.
2. Start a run that reaches an agent step and wait until agent output streams.
3. Type `/exit` and press `Enter`.
4. The shell prompt returns **and** the shell is no longer blocked; the resume
   hint (when applicable) is printed on the normal screen.
5. No agent-launcher / CLI / MCP-proxy descendants of the exited `cowboy`
   process remain running.

## TODO

- [x] TODO-01: Add the platform process-tree dependencies to `crates/agent/acp/Cargo.toml` (`windows-sys` for `cfg(windows)`, `libc` for `cfg(unix)`).
  - Procedure: edit `crates/agent/acp/Cargo.toml`, then run `cargo check -p cowboy-agent-acp`.
  - Expected result: `cargo check -p cowboy-agent-acp` exits 0 with no warnings, and `Cargo.lock` contains the new dependency for the host platform.
  - Observed result: `cargo check -p cowboy-agent-acp --all-targets` exited 0 with no warnings; `Cargo.lock` gained `windows-sys 0.61` (host platform, features `Win32_Foundation`/`Win32_Security`/`Win32_System_JobObjects`/`Win32_System_Threading`) and `libc 0.2` for `cfg(unix)`.

- [x] TODO-02: Implement `ProcessTreeScope` in a new `crates/agent/acp/src/process_tree.rs` with `new`, `configure`, `attach`, idempotent `terminate`, and a `Drop` that terminates the tree; Windows uses a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, Unix uses `process_group(0)` + `killpg(SIGKILL)`, other platforms are a documented no-op.
  - Procedure: add the module, wire it into `crates/agent/acp/src/lib.rs`, then run `cargo test -p cowboy-agent-acp process_tree`.
  - Expected result: the module compiles and the new `process_tree` unit tests from TODO-03 run (they may be added in the same change); no `unsafe` block is left without an explanatory comment.
  - Observed result: `crates/agent/acp/src/process_tree.rs` compiles and is wired into `lib.rs`; `cargo test -p cowboy-agent-acp process_tree` exited 0 with 3 passed. Every `unsafe` block carries an explanatory `// SAFETY:` comment.

- [x] TODO-03: Add `process_tree` unit tests `terminate_kills_descendants`, `terminate_is_idempotent`, and `drop_terminates_tree`.
  - Procedure: `cargo test -p cowboy-agent-acp process_tree`.
  - Expected result: all three tests pass; `terminate_kills_descendants` completes in under 10 s (it must not wait out the descendant's natural lifetime).
  - Observed result: `cargo test -p cowboy-agent-acp process_tree` exited 0 — `terminate_kills_descendants`, `terminate_is_idempotent`, `drop_terminates_tree` all passed; the whole binary finished in well under 10 s.

- [x] TODO-04: Make `StdioTransport` own a `ProcessTreeScope`: configure it before `spawn`, attach after spawn (failing the connect if attach fails), and terminate the tree in `force_terminate` and `close` before awaiting the child.
  - Procedure: edit `crates/agent/acp/src/transport/stdio.rs`, then run `cargo test -p cowboy-agent-acp --test stdio_shutdown`.
  - Expected result: `force_terminate_releases_stdout_when_agent_left_descendants` passes, the test binary finishes in well under the descendant's 30 s lifetime, and `crates/agent/acp/tests/stdio_shutdown.rs` is byte-for-byte unchanged (`git diff --stat -- crates/agent/acp/tests/stdio_shutdown.rs` prints nothing).
  - Observed result: `cargo test -p cowboy-agent-acp --test stdio_shutdown` exited 0 with `force_terminate_releases_stdout_when_agent_left_descendants` passing in 0.57 s (baseline before the fix: FAILED after 31.05 s with "stdout read never completed after force_terminate"). `git diff --stat -- crates/agent/acp/tests/stdio_shutdown.rs` printed nothing, i.e. the repro test is byte-for-byte unchanged.

- [x] TODO-05: Add the process-wide agent process registry with `register`, `deregister`, and `pub async fn terminate_all_agent_processes(timeout)`, registering in `StdioTransport::connect` and deregistering on `close`/`force_terminate`/drop; export it from `crates/agent/acp/src/lib.rs`.
  - Procedure: `cargo test -p cowboy-agent-acp` after implementing.
  - Expected result: the whole `cowboy-agent-acp` suite passes, including the pre-existing stdio and client tests.
  - Observed result: `cargo test -p cowboy-agent-acp` exited 0 in WSL (full suite green). On Windows three pre-existing failures remain unrelated to this change (`force_terminate_stops_stdio_child_by_pid` needs `sh`, plus two Zellij tests); a `git stash` baseline confirmed they fail identically without this change.

- [x] TODO-06: Add registry tests `terminate_all_agent_processes_terminates_registered_transport` and `closed_transport_is_deregistered`.
  - Procedure: `cargo test -p cowboy-agent-acp agent_processes`.
  - Expected result: both tests pass; the first shows a pending `recv()` completing within 5 s of `terminate_all_agent_processes`.
  - Observed result: `cargo test -p cowboy-agent-acp agent_processes` exited 0 with 2 passed; `terminate_all_agent_processes_terminates_registered_transport` observed the pending `recv()` completing well within 5 s.

- [x] TODO-07: Add `AcpConnector::terminate_all_agents(timeout)` (default returns `0`) in `crates/workflow/engine/src/runtime_dependencies.rs` and delegate from `ProductionAcpConnector` to `cowboy_agent_acp::terminate_all_agent_processes`.
  - Procedure: `cargo check -p cowboy-workflow-engine`.
  - Expected result: compiles with no warnings; existing fake connectors in engine tests continue to compile without modification.
  - Observed result: `cargo check -p cowboy-workflow-engine` exited 0 with no warnings; the pre-existing fake connectors in engine tests compiled unchanged thanks to the trait default returning `0`.

- [x] TODO-08: Implement `WorkflowRuntime::shutdown(timeout)` in `crates/workflow/engine/src/runtime.rs` — cancel store waits, bounded `terminate_all_agents`, then bounded SQLite pool close — and make it idempotent and non-failing.
  - Procedure: `cargo test -p cowboy-workflow-engine`.
  - Expected result: the engine suite passes, including the new tests from TODO-09.
  - Observed result: `cargo test -p cowboy-workflow-engine` exited 0 (run in WSL because the engine test target contains pre-existing unix-only code that does not compile on Windows — verified pre-existing via `git stash`).

- [x] TODO-09: Add engine tests `shutdown_terminates_agents_and_closes_store`, `shutdown_is_bounded_when_termination_hangs`, and `shutdown_is_idempotent`.
  - Procedure: `cargo test -p cowboy-workflow-engine shutdown`.
  - Expected result: all three pass; `shutdown_is_bounded_when_termination_hangs` completes within roughly the configured timeout even though the fake connector sleeps far longer.
  - Observed result: `cargo test -p cowboy-workflow-engine shutdown` exited 0 with 3 passed; `shutdown_is_bounded_when_termination_hangs` returned within roughly the configured timeout while the fake connector slept far longer.

- [x] TODO-10: Add `AppState::abort_background_tasks_for_exit` and call it (plus `runtime.cancel_store_waits()`) from `SlashCommand::Exit` in `crates/tui/app/src/app/commands.rs`, leaving `/cancel`'s card, status text, and durable-status behavior untouched.
  - Procedure: `cargo test -p cowboy`.
  - Expected result: the `cowboy` suite passes, including new tests `exit_aborts_background_tasks`, `exit_does_not_emit_cancel_card_or_cancel_status`, and `exit_preserves_resume_hint`, and all pre-existing `/cancel` tests.
  - Observed result: `cargo test -p cowboy --lib exit_` exited 0 with 5 passed, covering `exit_aborts_background_tasks`, `exit_does_not_emit_cancel_card_or_cancel_status`, and `exit_preserves_resume_hint`. The full `cargo test -p cowboy --lib` exited 101 with 342 passed / 2 failed. Both failures are pre-existing and unrelated to this change, confirmed via a `git stash` baseline (337 passed, same 2 failures), but they have **different scopes** — corrected after reviewer feedback: (a) `app::history::tests::concurrent_appends_are_not_lost_or_interleaved` is Windows-only (it passes in WSL); (b) `config::tests::documented_agent_watchdog_contract_is_unique_and_exact` is **not** Windows-only — it also fails under WSL because WSL reads the same `/mnt/c` working tree. Its cause is this repository's CRLF working-tree checkout (`git ls-files --eol` reports `i/lf w/crlf` for `README.md`, `docs/architecture.md`, and `docs/module-map.md`) while the test compares against an LF-only expected contract string. A clean LF clone of `HEAD` (`git -c core.autocrlf=false clone`) runs the same test green, proving the failure is a checkout artifact rather than a code or documentation defect, and `README.md` plus `crates/tui/app/src/config.rs` are byte-identical to `HEAD`.

- [x] TODO-11: Await `runtime.shutdown(...)` in `crates/tui/app/src/app.rs` after `run_loop` returns and before `finish_tui`, replacing the bare `cancel_store_waits` call.
  - Procedure: `cargo test -p cowboy finish_tui` and `cargo test -p cowboy`.
  - Expected result: existing `finish_tui` restore-then-print-resume-hint ordering tests still pass unmodified.
  - Observed result: `cargo test -p cowboy --lib finish_tui` exited 0 with 3 passed — the restore-then-print-resume-hint ordering tests still pass unmodified with `runtime.shutdown(SHUTDOWN_TIMEOUT)` awaited before `finish_tui`.

- [x] TODO-12: Replace `#[tokio::main]` in `crates/tui/app/src/main.rs` with an explicit multi-thread runtime plus `Runtime::shutdown_timeout`, extracted into a testable helper in the `cowboy` library, preserving the current error logging and exit-code-1 path.
  - Procedure: `cargo test -p cowboy bounded_shutdown` and `cargo build`.
  - Expected result: `bounded_shutdown_returns_despite_stuck_blocking_task` passes within its timeout and the workspace builds.
  - Observed result: `cargo test -p cowboy --lib bounded_shutdown` exited 0 with 2 passed, including `bounded_shutdown_returns_despite_stuck_blocking_task`; `cargo build` exited 0.

- [x] TODO-13: Update `docs/architecture.md` and `docs/module-map.md` to describe process-tree ownership in `cowboy-agent-acp` and the `WorkflowRuntime::shutdown` teardown path.
  - Procedure: `git diff -- docs/architecture.md docs/module-map.md`.
  - Expected result: both files mention the agent process-tree scope/registry and the bounded runtime shutdown sequence.
  - Observed result: `git diff -- docs/architecture.md docs/module-map.md` shows a new "### Shutdown" section in `docs/architecture.md` describing the `ProcessTreeScope`/registry and the bounded `WorkflowRuntime::shutdown` sequence, and updated `docs/module-map.md` rows for `cowboy-agent-acp` (`process_tree.rs`, `agent_processes.rs`, `transport/`), `cowboy-workflow-engine` (`runtime.rs`), and `cowboy` (`main.rs`, `lib.rs`, `process_exit.rs`, `app.rs`).

- [x] TODO-14: Run the full validation sweep and fix every compiler and Clippy warning.
  - Procedure: `cargo fmt --all -- --check`, then `cargo test -p cowboy-agent-acp`, `cargo test -p cowboy-workflow-engine`, `cargo test -p cowboy`, `cargo clippy --workspace --all-targets -- -D warnings`.
  - Expected result: every command exits 0 with no warnings emitted.
  - Gate narrowed after reviewer feedback: the sweep is run as independent commands (not `&&`-chained, which short-circuits and hides later results), it now includes `cargo fmt --all -- --check` as an explicit gate, and `config::tests::documented_agent_watchdog_contract_is_unique_and_exact` is excluded from the `cargo test -p cowboy` gate. That test fails identically at `HEAD` in this CRLF working tree on both Windows and WSL and passes in a clean LF clone of `HEAD`; it is a checkout artifact outside this change's scope (see TODO-10). Narrowed expected result: every sweep command exits 0 with zero warnings, excluding only that one pre-existing failure.
  - Observed result: a second reviewer pass found `cargo fmt --all -- --check` failing with 4 rustfmt violations in code this change added or edited — `crates/agent/acp/src/agent_processes.rs:53` and `crates/workflow/engine/src/runtime.rs:554/566/2662` — including a genuinely garbled line 2662 where the first statement of `workflow_runtime_propagates_dependency_factory_errors` had been collapsed onto the `fn` signature line. `cargo fmt --all` was run (exit 0) and `cargo fmt --all -- --check` now exits 0 on both Windows and WSL; only the four reported sites changed and no other file was touched. After reformatting, the sweep was rerun as separate commands in WSL against this working tree: `cargo fmt --all -- --check` → 0; `cargo test -p cowboy-agent-acp` → 0 (100 passed / 0 failed); `cargo test -p cowboy-workflow-engine` → 0 (161 passed / 0 failed); `cargo test -p cowboy` → **101**, failing solely on `config::tests::documented_agent_watchdog_contract_is_unique_and_exact`; `cargo test -p cowboy -- --skip documented_agent_watchdog_contract_is_unique_and_exact` → 0 with 344 passed / 0 failed; `cargo clippy --workspace --all-targets -- -D warnings` → 0 with 0 warnings. On Windows, `cargo clippy -p cowboy-agent-acp -p cowboy --all-targets -- -D warnings` → 0, `cargo test -p cowboy-agent-acp process_tree` → 0 (3 passed), `cargo test -p cowboy-agent-acp agent_processes` → 0 (2 passed), and the untouched repro test still passes (`cargo test -p cowboy-agent-acp --test stdio_shutdown` → 0 in 0.74 s, file SHA256 unchanged). An earlier recorded observation that the `&&`-chained WSL sweep exited 0 was incorrect — the chain short-circuited at `cargo test -p cowboy` — and this reformat is the only change made while addressing reviewer feedback.

- [x] TODO-15: Perform the manual end-to-end check from "How to verify" (start a run that reaches an agent step, type `/exit`).
  - Procedure: follow "How to verify" steps 1–5 on the host platform.
  - Expected result: the shell prompt returns and is immediately usable (the `cowboy` process has exited), the resume hint prints when applicable, and no agent-launcher, CLI, or MCP-proxy descendants of the exited process remain.
  - Observed result: with a stdio agent configured to spawn a long-lived descendant that inherits stdout, a run reached the agent step and two live agent-tree processes were observed. After `/exit`, the `cowboy` process exited in 0.74 s (previously it hung indefinitely after terminal restore) and zero agent-tree descendants survived; the log recorded `workflow runtime shutdown complete` followed by `TUI terminal session restored`.
