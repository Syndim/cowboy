# Bug behavior

A workflow can remain on an agent-backed step indefinitely when the newly spawned ACP backend never returns an `initialize` response. The reported run entered step `test`, retained `running` status and the run lock, and emitted no session-ready, progress, completion, failure, retry, or watchdog event afterward.

The persisted state remained unchanged after the stall:

```text
current_step  run_status  updated_at
test          running     2026-07-24T08:43:17.203936088Z
```

# Root cause

The initial ACP handshake has no timeout. `Client::connect_with_transport_and_options` in `crates/agent/acp/src/client.rs` awaits `client.initialize()` directly. `initialize()` sends the JSON-RPC request and then loops on `recv_message_direct()` until a matching response arrives. If the transport remains open but produces no response, that future never resolves.

The configured agent watchdog does not protect this phase. Its response inactivity deadline is only used after a session exists, inside prompt-turn processing. Replacement initialization is explicitly timeout-wrapped, but first-time initialization is not. Therefore a backend startup or forwarding failure leaves `AgentExecutor::execute` blocked while creating the client, and the workflow runner cannot convert the condition into a failed or retryable step.

# Root cause evidence

The run and backend logs provide this flow. Paths, process identifiers, and session identifiers are generalized.

1. Cowboy advances from the completed implementation step into the reported step and begins creating its tester client:

   ```text
   2026-07-24T08:43:17.264401Z ... agent step: starting run_id=<run-id> step=test role=tester agent=None
   2026-07-24T08:43:17.264500Z ... agent client missing; creating run_id=<run-id> role=tester agent=None
   ```

   These lines show the workflow is not still executing the previous step. It has entered `test` and is blocked before an agent session is ready.

2. The runtime resolves and spawns the configured stdio ACP backend:

   ```text
   2026-07-24T08:43:17.264536Z ... resolving ACP client for role role=tester agent=default command=<agent-wrapper> args=[...]
   2026-07-24T08:43:17.265784Z ... Agent subprocess spawned command=<agent-wrapper> ... pid=Some(<pid>)
   ```

   Process creation succeeds, so the stall is not a spawn error.

3. Cowboy sends request id `0` and begins waiting for the handshake response:

   ```text
   2026-07-24T08:43:17.265857Z ... ACP initialize starting id=0 protocol_version=1 client="cowboy"
   2026-07-24T08:43:17.265908Z ... ACP >>> request id=0 method="initialize" payload={...}
   ```

   No later `ACP connection initialized` line exists for this subprocess, although prior clients in the same run logged that line after successful handshakes.

4. The spawned backend's own logs show that its child process started, but its wrapper never completed the startup path needed to return the response to Cowboy:

   ```text
   2026-07-24T08:43:21.809Z [INFO] ACP server started (stdio mode)
   2026-07-24T08:43:21.810Z [INFO] ACP server started, waiting for requests
   2026-07-24T08:43:30.777812Z ... Still waiting for session file (10s elapsed, max 60s): <session-state>/events.jsonl
   2026-07-24T08:44:20.816123Z ... Session file not created after 60s: <session-state>/events.jsonl
   ```

   This is the concrete trigger: the backend remains alive without producing the initialize response. Cowboy must handle this external non-response rather than wait forever.

5. The source path explains why the trigger becomes an indefinite workflow stall:

   - `crates/workflow/agent/src/executor.rs`, `AgentExecutor::execute`, awaits `self.factory.create_client(role)` before it can emit `SessionReady`.
   - `crates/workflow/engine/src/runtime_dependencies.rs`, `ProductionAcpConnector::connect`, awaits `AcpClient::connect_with_options`.
   - `crates/agent/acp/src/client.rs`, `Client::connect_with_transport_and_options`, directly awaits `client.initialize()`.
   - `crates/agent/acp/src/client.rs`, `Client::initialize`, loops on `recv_message_direct()` with no timeout.
   - The same file's `hard_recover_and_continue` wraps replacement `initialize()` in `tokio::time::timeout`, proving timeout handling exists for recovery but is missing from the initial connection path.

   Because the initial wait has neither a deadline nor a watchdog branch, the runner remains inside client creation. It cannot persist a failure, spend retry budget, release the run lock, or advance the step.

# Reproduction steps

1. Construct an ACP transport whose `send` succeeds and whose `recv` remains pending without closing.
2. Call `Client::connect_with_transport_and_options` with a one-second recovery operation timeout.
3. Allow two seconds for the connection attempt to return.
4. Observe that the outer test deadline expires because initial ACP initialization ignores the configured timeout.

The live equivalent is an agent subprocess that remains alive after Cowboy sends `initialize` but never returns the matching JSON-RPC response.

# Regression test

- Test file: `crates/agent/acp/src/client.rs`
- Test name: `client::tests::connect_times_out_when_initialize_never_responds`
- Command: `cargo test -p cowboy-agent-acp client::tests::connect_times_out_when_initialize_never_responds -- --exact --nocapture`
- Expected failure before the fix: the test's two-second guard expires instead of receiving an initialization timeout error from `Client`.

The test uses paused Tokio time, so it reproduces the indefinite logical wait without delaying the test suite.

# Current failing result

```text
running 1 test

thread 'client::tests::connect_times_out_when_initialize_never_responds' panicked at crates/agent/acp/src/client.rs:1790:10:
ACP initialize should honor the recovery operation timeout: Elapsed(())
test client::tests::connect_times_out_when_initialize_never_responds ... FAILED

failures:
    client::tests::connect_times_out_when_initialize_never_responds

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 86 filtered out
```

# Fix constraints

- Bound first-time ACP initialization; do not limit the fix to prompt-turn watchdog handling.
- Honor the configured timeout consistently with replacement initialization, or introduce an explicitly configured handshake timeout without silently changing unrelated prompt behavior.
- On timeout, terminate or close the stalled transport so the backend process and pipes are not orphaned.
- Return an explicit connection error so the workflow runner can apply its existing recoverable-failure and retry policy instead of leaving the run `running`.
- Preserve successful initialization, session creation/loading, prompt watchdog recovery, cancellation, and progress-event behavior.
- Product code is intentionally unchanged by this investigation; only the focused failing test and this RCA were added.
