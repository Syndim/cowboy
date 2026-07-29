# RCA: Cowboy hangs after `/exit`

Run investigated: `run-8591d3bb-…` (event log + diagnostic log from the same
session). Paths below are generalized; user-specific directories are shown as
`<state-dir>` and secrets are omitted.

## Bug behavior

Typing `/exit` in the Cowboy TUI while a workflow run is executing an agent step
tears the TUI down correctly — the alternate screen is left and the previous
shell prompt reappears — but the `cowboy` process never exits. The terminal
looks usable yet the shell is still blocked on the foreground process; the user
must kill the process manually.

Observed on Windows with the stdio ACP transport (agent launched as a local
subprocess). The diagnostic log stops at the last workflow line and no further
records are written, consistent with a hang after the TUI event loop returned.

## Root cause

Cowboy's exit path never terminates the in-flight agent work; it relies on the
process teardown that follows `main` returning:

1. `SlashCommand::Exit` only flips a flag
   (`crates/tui/app/src/app/commands.rs:168-172`). Unlike `/cancel`
   (`commands.rs:164-167`), it does **not** call
   `AppState::cancel_background_tasks` (`crates/tui/app/src/app/state.rs:964`),
   so the background task running the agent step keeps running.
2. `run_loop` returns on `state.exit_requested()`
   (`crates/tui/app/src/app.rs:186-189`), `run_tui` restores the terminal and
   returns (`crates/tui/app/src/app.rs:40-49`) — this is the moment the user
   sees the TUI disappear.
3. `main` is `#[tokio::main]` (`crates/tui/app/src/main.rs:5-11`). Returning from
   it drops the Tokio runtime, and runtime drop waits **without a timeout** for
   in-flight *mandatory* blocking tasks.
4. The ACP stdio transport reads the agent's stdout with `Lines::next_line()`
   (`crates/agent/acp/src/transport/stdio.rs:116-118`) over a piped
   `tokio::process::ChildStdout`
   (`crates/agent/acp/src/transport/stdio.rs:26-33`). On Windows that read is
   serviced by a *mandatory* blocking task, so runtime drop blocks until it
   completes.
5. That read can only complete at EOF, i.e. when **every** handle to the pipe's
   write end is closed. `kill_on_drop(true)` (`stdio.rs:33`) and
   `force_terminate` (`stdio.rs:175-186`) only stop the directly spawned agent
   process (`self.child`). The agent's descendants (the CLI process the agent
   launcher spawns, plus its MCP proxies) inherit the same stdout write handle
   on Windows, survive, and keep the pipe open forever.

Net effect: after the terminal is restored, the process parks permanently inside
Tokio runtime shutdown waiting on a child-stdout read that can never reach EOF.

## Root cause evidence

### Flow reconstructed from the session's diagnostic log

Step 1 — the agent is spawned as a local subprocess with piped stdio, and its
output is read through the stdio transport:

```
2026-07-28T06:42:57.642890Z  WARN cowboy_agent_acp::transport::stdio:
  crates\agent\acp\src\transport\stdio.rs:82: Agent subprocess stderr
  command=<agent-launcher> pid=Some(<pid>) stderr=…
```

This proves the run used `StdioTransport`, i.e. the piped-`ChildStdout` reader
that later blocks shutdown.

Step 2 — the same launcher immediately spawns **separate long-lived processes**
of its own, which are exactly the descendants that inherit and hold the stdout
pipe:

```
… stderr=📦 Resolving Copilot CLI...
… stderr=🧠 Copilot CLI at <…>\copilot.exe
… stderr=✅ Copilot CLI resolved (in 0 ms)
… stderr=Launched 3 MCP proxies:
… stderr=  - <mcp-server>: http://127.0.0.1:<port> (log span: mcp{…})
```

So at any point in the session there are several live descendants of the agent
process, none of which Cowboy tracks or terminates.

Step 3 — a long-running agent step is active in a TUI background task, with an
outstanding ACP request (therefore an outstanding `recv()`/`next_line()` on the
child's stdout):

```
2026-07-28T06:45:26.766535Z  INFO cowboy_agent_acp::client:
  crates\agent\acp\src\client.rs:1359: ACP session loaded
  session_id="<redacted>" history_events=701
2026-07-28T06:45:26.766578Z  INFO cowboy_workflow_agent::executor:
  crates\workflow\agent\src\executor.rs:825: agent session loaded
  run_id=run-8591d3bb-… role=implementer session_id="<redacted>" history_events=701
```

Step 4 — the step ends in a recoverable failure, so the run stays active and the
agent process plus its transport stay alive for the retry:

```
2026-07-28T06:51:14.541298Z  WARN cowboy_agent_acp::client:
  crates\agent\acp\src\client.rs:994: Agent watchdog detected response inactivity
  event="agent_watchdog_timeout" session_id="<redacted>" id=2 timeout_seconds=100
2026-07-28T06:51:14.541835Z  WARN … event="agent_watchdog_cancel_sent" …
2026-07-28T06:51:14.878809Z ERROR cowboy_workflow_agent::executor:
  crates\workflow\agent\src\executor.rs:752: agent step: failed to parse frontmatter output
  run_id=run-8591d3bb-… step=implement reply=…
```

Step 5 — this is the **last line in the log**. The user then typed `/exit`.
`commands.rs:168-172` sets the exit flag only, `app.rs:186-189` returns from the
loop, and `app.rs:40-49` restores the terminal (matching “TUI disappeared and
the previous command line appeared”). Nothing aborts the background task and
nothing terminates the agent process tree, so no further log record is ever
written: the process is stuck in Tokio runtime drop, not in the workflow.

Corroborating environment evidence for surviving descendants: the machine still
holds agent-launcher and CLI processes started on previous days, i.e. Cowboy's
teardown routinely leaves the agent's descendants running.

### Mechanism confirmed by a controlled experiment

A standalone Tokio program shaped exactly like `StdioTransport` (piped stdio,
`kill_on_drop(true)`, a spawned task awaiting `next_line()` on the child's
stdout, then returning from `#[tokio::main] main`):

| Child behavior | Result |
| --- | --- |
| Child with no descendants | Process exits ~2.4 s after `main` returns |
| Child that leaves one descendant inheriting stdout | Process **never exits** (>25 s, killed manually) |
| Same, but `JoinHandle::abort()` before returning | Process **never exits** (>25 s, killed manually) |

The third row matters: aborting the background task is *not* sufficient, because
the blocking stdout read lives in Tokio's blocking pool and holds the pipe
handle independently of the task.

### Mechanism confirmed inside the repository

The regression test below isolates the same condition against the real
`StdioTransport`. With a surviving descendant, `recv()` never completes after
`force_terminate()` (the 5 s timeout trips). The identical test with the
descendant removed passes in 0.42 s — the only difference is the surviving
descendant. The failing test binary itself also took 30.8 s to finish (the
descendant's lifetime) even though the assertion tripped at 5 s, reproducing the
“process cannot exit while the pipe is held” behavior end to end.

## Reproduction steps

1. Configure a stdio agent whose launcher spawns longer-lived child processes
   (the shipped agent launcher does this: it resolves and spawns a CLI plus MCP
   proxies).
2. Start `cowboy` (TUI) and start a workflow run that reaches an agent step, so
   an ACP request is in flight on the agent's stdout pipe.
3. While the agent step is still active, type `/exit` and press `Enter`.
4. Observe: the alternate screen is left and the shell prompt is redrawn, but
   the `cowboy` process never terminates and the shell stays blocked.

A minimal, agent-free reproduction of the same shutdown condition is provided by
the regression test below.

## Regression test

- Test file: `crates/agent/acp/tests/stdio_shutdown.rs`
- Test name: `force_terminate_releases_stdout_when_agent_left_descendants`
- Command: `cargo test -p cowboy-agent-acp --test stdio_shutdown`
- Expected before the fix: **FAIL**. The transport spawns an agent that leaves a
  descendant inheriting its stdout pipe, waits for the agent's readiness line,
  calls `force_terminate()`, and then requires the pending stdout read to
  complete within 5 s. Only the directly spawned process is killed, so the
  descendant keeps the pipe open, `recv()` never returns, and the assertion
  fails with “stdout read never completed after force_terminate…”.

## Current failing result

```
$ cargo test -p cowboy-agent-acp --test stdio_shutdown

running 1 test
test force_terminate_releases_stdout_when_agent_left_descendants ... FAILED

failures:

---- force_terminate_releases_stdout_when_agent_left_descendants stdout ----

thread 'force_terminate_releases_stdout_when_agent_left_descendants' panicked at
crates\agent\acp\tests\stdio_shutdown.rs:61:5:
stdout read never completed after force_terminate; a surviving descendant still
holds the agent stdout pipe, so Cowboy blocks forever on shutdown

failures:
    force_terminate_releases_stdout_when_agent_left_descendants

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;
finished in 30.82s

error: test failed, to rerun pass `-p cowboy-agent-acp --test stdio_shutdown`
```

## Fix constraints

- Terminating an agent must release its stdio pipes, which means terminating the
  agent's **process tree**, not just the directly spawned child. Killing only
  `self.child` is what leaves the pipe open.
- Do not edit `crates/agent/acp/tests/stdio_shutdown.rs`; make product changes so
  that test passes.
- `/exit` should terminate in-flight work deterministically rather than leaving
  it to Tokio runtime drop. Aborting the background task alone is proven
  insufficient (see the controlled experiment) — the blocking stdout read
  survives task abort.
- Any belt-and-braces shutdown guard must be bounded (e.g. a bounded runtime
  shutdown wait) and must still let clean shutdown flush persisted run state,
  event logs, and the SQLite store; do not trade the hang for lost run state.
- Keep the layering rules from `AGENTS.md`: transport/process-tree termination
  belongs in `cowboy-agent-acp`, agent-session teardown in
  `cowboy-workflow-agent` / `cowboy-workflow-engine`, and only exit dispatch in
  `crates/tui/app`.
- Preserve current `/cancel` behavior and the existing resume-hint print ordering
  in `finish_tui` (`crates/tui/app/src/app.rs`).
- Existing stdio transport tests must keep passing, and the fix should stop
  leaving orphaned agent descendants behind, since orphan accumulation is a
  symptom of the same defect.
