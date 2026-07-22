# Plan

When the interactive TUI exits cleanly while its active run is **resumable**
(not terminal), print a one-line hint to the restored terminal telling the user
which command picks the run back up. Print nothing when there is no active run
or when the active run is in a terminal state.

Rationale and grounding:

- `crates/tui/app/src/app.rs::run_tui` runs the event loop via `run_loop`, then
  calls `runtime.cancel_store_waits()`, logs any error, and calls
  `terminal_mode.restore()?`. The hint must be printed **after** `restore()` so
  it lands on the normal terminal, not the alternate screen that teardown
  discards (`TerminalModeGuard::restore` at
  `crates/tui/terminal/src/lib.rs:125`).
- `run_loop` returns `Result<()>` and exits cleanly at exactly two points:
  `if state.exit_requested() { return Ok(()) }` (driven by `/exit`, see
  `commands.rs:164` `SlashCommand::Exit -> mark_exit_requested`) and the
  `KeyHandling::Exit => return Ok(())` arm (driven by `Ctrl+C`, see
  `input.rs:20`). Both hold a live `AppState`. The error paths use
  `return Err(...)` and must keep their current behavior (no hint).
- `AppState` already carries the needed data: `active_run_id: Option<String>`,
  `durable_run_status: Option<RunStatusState>`, and
  `pending_prompt: Option<PendingPrompt>` (which exposes `run_id()` and
  `prompt_id()`).
- `RunStatusState` (`cowboy-workflow-engine`) variants: `Running`,
  `WaitingForInput`, `Completed`, `Failed`, `Cancelled`.

## Status mapping (authoritative)

Runtime resumability is defined by `WorkflowRuntime::resume_with`
(`crates/workflow/engine/src/runtime.rs:764-791`): only `Running`, `Failed`, and
`WaitingForInput` are resumable; `Completed` and `Cancelled` are non-resumable
no-ops. The hint therefore maps:

| durable status | hint command | notes |
|---|---|---|
| `Running` | `cowboy resume <run-id>` | |
| `WaitingForInput` | `cowboy answer <run-id> <prompt-id> <answer>` when a pending prompt for **that** run id is known; otherwise `cowboy resume <run-id>` | fallback when prompt is absent or belongs to a different run |
| `Failed` | `cowboy resolve <run-id>` | |
| `Completed` | none (terminal) | |
| `Cancelled` | none (terminal, non-resumable per `runtime.rs:786`) | |
| `None` (no known status) | none | |
| no active run id | none | |

Full rendered line format (printed verbatim), e.g.:
`Run <id> is not complete. Resume with: cowboy resume <id>`
with the `answer`/`resolve` command substituted per the table. The feature does
**not** expand scope to make cancelled runs resumable; `Cancelled` is terminal.

## Test-filter collision avoidance (authoritative)

The `resume_hint()` unit tests live in module `app::state::tests`
(`crates/tui/app/src/app/state.rs:1534` `#[cfg(test)] mod tests`), while the
`print_resume_hint` helper test lives in module `app::tests`
(`crates/tui/app/src/app.rs:354` `#[cfg(test)] mod tests`). A bare substring
filter `resume_hint` also matches `print_resume_hint_*` (substring collision),
so it must **not** be used as an exact-count acceptance filter. All acceptance
commands that must select exactly the state-module tests use the
module-qualified filter **`state::tests::resume_hint`**, whose full-path
substring cannot match `app::tests::print_resume_hint_*`. This keeps the
exact-count proof satisfiable in the final tree.

## Testable teardown seam

`run_loop` reads global terminal input through crossterm
(`event::poll`/`event::read`), which is not injectable through the ratatui
`Backend` abstraction, so the full loop is not deterministically drivable in a
unit test. To obtain behavioral (not source-text) proof of the teardown
contract, the print/restore step is extracted behind injected dependencies:

- `fn print_resume_hint(out: &mut impl io::Write, hint: Option<&str>) -> io::Result<()>`
  writes `"{hint}\n"` for `Some` and nothing for `None`.
- `trait TerminalRestore { fn restore(&mut self) -> anyhow::Result<()>; }`
  implemented for `TerminalModeGuard`, plus
  `fn finish_tui(loop_result: Result<Option<String>>, guard: &mut impl TerminalRestore, out: &mut impl io::Write) -> Result<()>`
  that (1) calls `guard.restore()?`, (2) then calls
  `print_resume_hint(out, loop_result.as_deref().ok().flatten())?`, (3) then
  returns `loop_result.map(|_| ())`. This preserves the original precedence: a
  restore error propagates first; the hint prints only on `Ok(Some)`; the
  original `run_loop` error is returned unchanged when the loop failed.

These seams are unit-testable with a fake restore recorder and a `Vec<u8>`
writer, proving: the hint reaches output, restoration happens before any output,
and error/`None` cases suppress the hint.

# Changes

- `crates/tui/app/src/app/state.rs`
  - Add `pub(in crate::app) fn resume_hint(&self) -> Option<String>` on
    `AppState`. Returns `None` when `active_run_id` is `None`, or when
    `durable_run_status` is `None`, `Some(Completed)`, or `Some(Cancelled)`.
    Otherwise returns the full user-facing line per the status-mapping table:
    - `Running` -> `resume` command.
    - `Failed` -> `resolve` command.
    - `WaitingForInput` -> `answer` command **only if** `pending_prompt` is
      `Some` and its `run_id()` equals the active run id, using its
      `prompt_id()`; otherwise the `resume` command.

- `crates/tui/app/src/app.rs`
  - Add `print_resume_hint`, the `TerminalRestore` trait + `TerminalModeGuard`
    impl, and `finish_tui` as described in "Testable teardown seam".
  - Change `run_loop` to return `Result<Option<String>>`. Replace both clean
    `return Ok(())` sites with `return Ok(state.resume_hint())`
    (the `exit_requested` branch and the `KeyHandling::Exit` branch). Leave every
    `return Err(...)` path unchanged.
  - In `run_tui`, keep the existing `cancel_store_waits()` and error-logging
    lines, then replace the final `terminal_mode.restore()?; result` tail with
    `finish_tui(result, &mut terminal_mode, &mut io::stdout())` and return its
    `Result<()>`, so the print happens after restoration and `run_tui` still
    yields `Result<()>`.

# Tests to be added/updated

- `crates/tui/app/src/app/state.rs` unit tests (module `app::state::tests`) —
  `resume_hint()` full-branch coverage. Each test drives `AppState` through
  `apply_workflow_event` with the exact deterministic event sequences below
  (distinct run ids/kinds so `try_coalesce_active_event` never merges them),
  asserting the **complete rendered hint string** or `None`. Name every test
  `resume_hint_*` so the module-qualified filter `state::tests::resume_hint`
  selects exactly these nine:
  - `Running`: `RunStarted{...}` on `run-a` -> `Run run-a is not complete. Resume with: cowboy resume run-a`.
  - `WaitingForInput` matching prompt: `WaitingForInput{step,prompt_id:"approval",...}` on `run-a` -> `Run run-a is not complete. Resume with: cowboy answer run-a approval <answer>`.
  - `WaitingForInput` no pending prompt: set durable status to `WaitingForInput`
    with no prompt by emitting `RunStatusChanged{status:"waiting_for_input"}` on
    `run-a` (arm at `state.rs:1386`; `run_status_state_from_str("waiting_for_input")` -> `WaitingForInput`, and this arm never sets `pending_prompt`) -> `resume` fallback line for `run-a`.
  - `WaitingForInput` prompt belongs to a different run: (1) emit
    `WaitingForInput{step,prompt_id:"approval",...}` on `run-a` (sets
    `pending_prompt{run_id:"run-a"}`, `durable=WaitingForInput`,
    `active_run_id="run-a"`); (2) emit `StepStarted{step_id:"start"}` on `run-b`
    (arm at `state.rs:1252` sets `active_run_id="run-b"` via the unconditional
    assignment at `state.rs:1227` and leaves `durable_run_status` and
    `pending_prompt` untouched) -> `resume` fallback line for `run-b`
    (`pending_prompt.run_id "run-a"` != active `run-b`).
  - `Failed`: `RunFailed{...}` on `run-a` -> `... cowboy resolve run-a`.
  - `Cancelled`: `RunCancelled` on `run-a` -> `None`.
  - `Completed`: `RunCompleted` on `run-a` -> `None`.
  - Unknown durable status (`None`) with an active run: emit
    `RunStatusChanged{status:"zzz_unmapped"}` on `run-a`
    (`run_status_state_from_str` returns `None` for unmapped strings at
    `state.rs:362`; `active_run_id` is still set) -> `None`.
  - No active run: fresh `AppState` -> `None`.
  - Expected selection: exactly 9 tests under `state::tests::resume_hint`; the
    app-module `print_resume_hint` helper test is **not** selected by that
    filter.

- `crates/tui/app/src/app.rs` unit tests (module `app::tests`) — behavioral
  teardown proofs (owned by TODO-06 and TODO-08, see TODO section):
  - `print_resume_hint` content: writes exactly `"<line>\n"` into a `Vec<u8>`
    for `Some(line)` and zero bytes for `None`. Test name
    `print_resume_hint_writes_line_for_some_and_nothing_for_none`.
  - `finish_tui` ordering/error policy (three `finish_tui_*` tests): a fake
    `TerminalRestore` pushes `"restore"` onto a shared `Vec<String>` when called;
    a wrapping writer pushes `"write"` on first write. Assert that for
    `Ok(Some(line))` the recorded order is `["restore","write"]` and the writer
    received `"<line>\n"`; for `Ok(None)` only `["restore"]` is recorded and
    nothing is written; for `Err(e)` only `["restore"]` is recorded, nothing is
    written, and `finish_tui` returns the same error.

# How to verify

Automated:

- `cargo test -p cowboy state::tests::resume_hint -- --list` then
  `cargo test -p cowboy state::tests::resume_hint` — confirm the module-qualified
  filter lists exactly the 9 `resume_hint_*` tests and all pass (collision-free:
  `print_resume_hint_*` is in `app::tests`, not `state::tests`, so it is not
  selected).
- `cargo test -p cowboy print_resume_hint` — print helper content proof.
- `cargo test -p cowboy finish_tui` — teardown ordering/error-suppression proof.
- `cargo test -p cowboy` — full TUI-crate suite regression (the `run_loop`
  signature change must not break other tests).
- `cargo clippy -p cowboy --all-targets` — must exit 0 and print no `warning:`
  lines in its own output (read clippy's stderr directly; no separate grep proof
  command is required or recorded).

Manual smoke test (deterministic, non-mutating, isolated temp state; no agent).
Run these literal commands from the repo root; substitute the printed `$T` path
into the fixture files:

```sh
T="$(mktemp -d)"
mkdir -p "$T/state" "$T/workflows"

cat > "$T/workflows/ask.lua" <<'LUA'
local confirm = step("confirm")
confirm.run = function(ctx)
  return action.ask_user {
    id = "approval",
    message = "Approve?",
    choices = { "yes", "no" },
  }
end
local done = step("done")
done.run = function(ctx)
  return action.status { status = "success", body = "ok" }
end
confirm:on("answered", done)
return workflow("ask", confirm)
LUA

cat > "$T/workflows/complete.lua" <<'LUA'
local start = step("start")
start.run = function(ctx)
  return action.status { status = "success", body = "done" }
end
return workflow("complete", start)
LUA

cat > "$T/workflows/fail.lua" <<'LUA'
local start = step("start")
start.run = function(ctx)
  return action.fail { reason = "nope" }
end
return workflow("fail", start)
LUA

# config.toml — substitute the actual $T path (no shell expansion inside TOML).
cat > "$T/config.toml" <<TOML
state_dir = "$T/state"
workflow_store = "$T/state/workflow.redb"
workflow_dirs = ["$T/workflows"]
TOML

cat > "$T/pty_smoke.py" <<'PY'
import os, sys, pty, select, time, re

CONFIG = sys.argv[1]
MODE = sys.argv[2]
BIN = "./target/debug/cowboy"

def spawn():
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.execv(BIN, [BIN, "--config", CONFIG])
        os._exit(127)
    return pid, fd

def drain(fd, buf, timeout=0.4):
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            buf.append(data)
            end = time.time() + timeout

def wait_for(fd, buf, pattern, timeout=20):
    end = time.time() + timeout
    rx = re.compile(pattern)
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            buf.append(data)
            if rx.search(b"".join(buf).decode("utf-8", "replace")):
                return True
    return False

def send(fd, s):
    os.write(fd, s.encode()); time.sleep(0.25)

pid, fd = spawn()
buf = []
drain(fd, buf, 1.0)

if MODE == "ask-ctrlc":
    send(fd, "/run --workflow ask needs approval\r")
    wait_for(fd, buf, r"Waiting for input|waiting")
    time.sleep(0.5)
    os.write(fd, b"\x03")
elif MODE == "fail-exit":
    send(fd, "/run --workflow fail x\r")
    wait_for(fd, buf, r"Run failed|failed")
    time.sleep(0.5)
    send(fd, "/exit\r")
elif MODE == "complete-exit":
    send(fd, "/run --workflow complete go\r")
    wait_for(fd, buf, r"completed|Run completed")
    time.sleep(0.5)
    send(fd, "/exit\r")
elif MODE == "norun-ctrlc":
    time.sleep(0.5)
    os.write(fd, b"\x03")
else:
    print("UNKNOWN MODE", MODE); os._exit(2)

drain(fd, buf, 1.5)
try:
    os.close(fd)
except OSError:
    pass
_, status = os.waitpid(pid, 0)
raw = b"".join(buf).decode("utf-8", "replace")
marker = "\x1b[?1049l"
tail = raw.rsplit(marker, 1)[-1] if marker in raw else raw
clean = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", tail)
clean = re.sub(r"\x1b[>=]", "", clean).replace("\r", "")
hint = ""
for ln in clean.splitlines():
    if "not complete" in ln or "Resume with" in ln:
        hint = ln.strip(); break
print("MODE:", MODE)
print("EXIT:", os.waitstatus_to_exitcode(status))
print("HINT:", hint if hint else "<none>")
PY

cargo build -p cowboy
```

Each scenario then runs as a single self-contained driver invocation. The
driver spawns `./target/debug/cowboy` under a PTY, performs that scenario's
input/wait/exit internally, and prints the line left on the normal screen after
the alternate screen closes (the text after the `ESC[?1049l` sequence). State is
reused across the four scenarios (no reset), so each scenario starts a fresh run
and its hint references that scenario's own run id. The catalog workflow ids are
the entry filenames `ask`, `complete`, and `fail`.

```sh
python3 "$T/pty_smoke.py" "$T/config.toml" ask-ctrlc       # Scenario 1: waiting + Ctrl+C -> answer hint
python3 "$T/pty_smoke.py" "$T/config.toml" fail-exit       # Scenario 2: failed + /exit -> resolve hint
python3 "$T/pty_smoke.py" "$T/config.toml" complete-exit   # Scenario 3: completed + /exit -> no hint
python3 "$T/pty_smoke.py" "$T/config.toml" norun-ctrlc     # Scenario 4: no run + Ctrl+C -> no hint
rm -rf "$T"
```

Scenario 1 (`ask-ctrlc`) exercises the `KeyHandling::Exit` clean-exit path;
scenario 2 (`fail-exit`) exercises the `exit_requested` clean-exit path.
Expected: scenario 1 prints
`Run <id> is not complete. Resume with: cowboy answer <id> approval <answer>`;
scenario 2 prints `Run <id> is not complete. Resume with: cowboy resolve <id>`;
scenarios 3 and 4 print no `not complete` / `Resume with` line.

`Cancelled` -> no-hint is covered deterministically by the `resume_hint()` unit
test (`RunCancelled` on `run-a` -> `None`) rather than a fragile interactive
cancellation, because durable `Cancelled` requires a live background task to
cancel and cannot be produced by the instant fixtures above.

# TODO

- [x] TODO-01: Add `AppState::resume_hint()` in `crates/tui/app/src/app/state.rs`.
  - Procedure: implement the method per Changes (mapping `Running`->resume,
    `Failed`->resolve, `WaitingForInput`->answer-or-resume-fallback,
    `Completed`/`Cancelled`/`None`/no-active-run->`None`). Prerequisite: the
    TODO-03 tests are authored. Acceptance commands use the collision-free
    module-qualified filter: `cargo test -p cowboy state::tests::resume_hint -- --list`
    MUST report `9 tests, 0 benchmarks` naming the 9 `resume_hint_*` tests
    (guarding against a zero-match false pass and excluding the app-module
    `print_resume_hint` helper), then `cargo test -p cowboy state::tests::resume_hint`
    MUST report `test result: ok. 9 passed; 0 failed`.
  - Expected result: exactly the 9 `resume_hint_*` tests are selected by
    `state::tests::resume_hint` and all pass; compilation alone is not
    acceptance.
  - Observed result: `AppState::resume_hint()` added after `active_run_id()` in
    `state.rs`, mapping Running->`cowboy resume`, Failed->`cowboy resolve`,
    WaitingForInput->`cowboy answer <id> <prompt-id> <answer>` when the pending
    prompt matches the active run id else the resume fallback, and
    Completed/Cancelled/None/no-active-run->`None`.
    `cargo test -p cowboy state::tests::resume_hint -- --list` reported
    `9 tests, 0 benchmarks` naming exactly the 9 `resume_hint_*` tests; then
    `cargo test -p cowboy state::tests::resume_hint` reported `test result: ok.
    9 passed; 0 failed`.
- [x] TODO-02: Wire the hint through `run_loop` and print it in `run_tui`.
  - Procedure: change `run_loop` to `Result<Option<String>>`, return
    `state.resume_hint()` at both clean exit points; add `print_resume_hint`,
    the `TerminalRestore` trait/impl, and `finish_tui`; call
    `finish_tui(result, &mut terminal_mode, &mut io::stdout())` in `run_tui`
    after `cancel_store_waits()`/error logging. Prerequisite: the TODO-06 and
    TODO-08 tests are authored. Acceptance commands:
    `cargo test -p cowboy print_resume_hint` MUST report `test result: ok. 1 passed`
    and `cargo test -p cowboy finish_tui` MUST report `test result: ok. 3 passed`.
  - Expected result: both named test filters select a nonzero number of tests
    and pass; compilation alone is not acceptance.
  - Observed result: `run_loop` now returns `Result<Option<String>>` and returns
    `state.resume_hint()` at the `exit_requested` and `KeyHandling::Exit`
    branches (error paths unchanged); added `print_resume_hint`, the
    `TerminalRestore` trait + `TerminalModeGuard` impl, and `finish_tui`;
    `run_tui` calls `finish_tui(result, &mut terminal_mode, &mut io::stdout())?`
    after `cancel_store_waits()`/error logging. `cargo test -p cowboy
    print_resume_hint` reported `test result: ok. 1 passed`;
    `cargo test -p cowboy finish_tui` reported `test result: ok. 3 passed`.
- [x] TODO-03: Add `resume_hint()` unit tests in `state.rs`.
  - Procedure: add the 9 `resume_hint_*` tests in module `app::state::tests`
    using the exact deterministic event sequences in "Tests to be
    added/updated" (Running; WaitingForInput matching prompt; WaitingForInput
    no prompt via `RunStatusChanged{status:"waiting_for_input"}`; WaitingForInput
    cross-run mismatch via `WaitingForInput` on `run-a` then `StepStarted` on
    `run-b`; Failed; Cancelled; Completed; unknown status via
    `RunStatusChanged{status:"zzz_unmapped"}`; no active run). Run
    `cargo test -p cowboy state::tests::resume_hint`.
  - Expected result: all 9 tests pass; each asserts the exact full hint line or
    `None`.
  - Observed result: added the 9 `resume_hint_*` tests in `app::state::tests`
    with the exact deterministic event sequences; `cargo test -p cowboy
    state::tests::resume_hint` reported `test result: ok. 9 passed; 0 failed`.
- [x] TODO-04: Verify full TUI-crate test suite and lint.
  - Procedure: run `cargo test -p cowboy` and record its final
    `test result: ok ...` line; run `cargo clippy -p cowboy --all-targets` and
    read its own output. Acceptance is judged from these two commands only — do
    not run or record a separate `grep` (or any other) command as a proof step.
  - Expected result: `cargo test -p cowboy` reports `test result: ok` with zero
    failures across the lib and CLI integration suites; `cargo clippy -p cowboy
    --all-targets` exits 0 and prints no `warning:` or `error:` lines in its own
    output.
  - Observed result: `cargo test -p cowboy` reported `test result: ok. 329
    passed; 0 failed; 2 ignored` for the lib suite with the CLI integration
    suites also reporting `test result: ok` and zero failures;
    `cargo clippy -p cowboy --all-targets` exited 0 and printed no `warning:` or
    `error:` lines.
- [x] TODO-05: Manual smoke test of exit hint behavior.
  - Procedure (`command` kind; execute each step as exactly one complete
    executable command, in order; record each command against its one-based
    step index; scenario labels live in this prose and in the observed result,
    never inside a command string):
    1. `T="$(mktemp -d)"`
    2. `mkdir -p "$T/state" "$T/workflows"`
    3. Write `ask.lua` with the complete heredoc command:
       ```sh
       cat > "$T/workflows/ask.lua" <<'LUA'
       local confirm = step("confirm")
       confirm.run = function(ctx)
         return action.ask_user {
           id = "approval",
           message = "Approve?",
           choices = { "yes", "no" },
         }
       end
       local done = step("done")
       done.run = function(ctx)
         return action.status { status = "success", body = "ok" }
       end
       confirm:on("answered", done)
       return workflow("ask", confirm)
       LUA
       ```
    4. Write `complete.lua` with the complete heredoc command:
       ```sh
       cat > "$T/workflows/complete.lua" <<'LUA'
       local start = step("start")
       start.run = function(ctx)
         return action.status { status = "success", body = "done" }
       end
       return workflow("complete", start)
       LUA
       ```
    5. Write `fail.lua` with the complete heredoc command:
       ```sh
       cat > "$T/workflows/fail.lua" <<'LUA'
       local start = step("start")
       start.run = function(ctx)
         return action.fail { reason = "nope" }
       end
       return workflow("fail", start)
       LUA
       ```
    6. Write `config.toml` with the complete heredoc command (shell expands `$T`):
       ```sh
       cat > "$T/config.toml" <<TOML
       state_dir = "$T/state"
       workflow_store = "$T/state/workflow.redb"
       workflow_dirs = ["$T/workflows"]
       TOML
       ```
    7. Write `pty_smoke.py` with the complete heredoc command (identical to the
       driver shown in "How to verify"):
       ```sh
       cat > "$T/pty_smoke.py" <<'PY'
       import os, sys, pty, select, time, re

       CONFIG = sys.argv[1]
       MODE = sys.argv[2]
       BIN = "./target/debug/cowboy"

       def spawn():
           pid, fd = pty.fork()
           if pid == 0:
               os.environ["TERM"] = "xterm-256color"
               os.execv(BIN, [BIN, "--config", CONFIG])
               os._exit(127)
           return pid, fd

       def drain(fd, buf, timeout=0.4):
           end = time.time() + timeout
           while time.time() < end:
               r, _, _ = select.select([fd], [], [], 0.05)
               if r:
                   try:
                       data = os.read(fd, 65536)
                   except OSError:
                       return
                   if not data:
                       return
                   buf.append(data)
                   end = time.time() + timeout

       def wait_for(fd, buf, pattern, timeout=20):
           end = time.time() + timeout
           rx = re.compile(pattern)
           while time.time() < end:
               r, _, _ = select.select([fd], [], [], 0.1)
               if r:
                   try:
                       data = os.read(fd, 65536)
                   except OSError:
                       break
                   if not data:
                       break
                   buf.append(data)
                   if rx.search(b"".join(buf).decode("utf-8", "replace")):
                       return True
           return False

       def send(fd, s):
           os.write(fd, s.encode()); time.sleep(0.25)

       pid, fd = spawn()
       buf = []
       drain(fd, buf, 1.0)

       if MODE == "ask-ctrlc":
           send(fd, "/run --workflow ask needs approval\r")
           wait_for(fd, buf, r"Waiting for input|waiting")
           time.sleep(0.5)
           os.write(fd, b"\x03")
       elif MODE == "fail-exit":
           send(fd, "/run --workflow fail x\r")
           wait_for(fd, buf, r"Run failed|failed")
           time.sleep(0.5)
           send(fd, "/exit\r")
       elif MODE == "complete-exit":
           send(fd, "/run --workflow complete go\r")
           wait_for(fd, buf, r"completed|Run completed")
           time.sleep(0.5)
           send(fd, "/exit\r")
       elif MODE == "norun-ctrlc":
           time.sleep(0.5)
           os.write(fd, b"\x03")
       else:
           print("UNKNOWN MODE", MODE); os._exit(2)

       drain(fd, buf, 1.5)
       try:
           os.close(fd)
       except OSError:
           pass
       _, status = os.waitpid(pid, 0)
       raw = b"".join(buf).decode("utf-8", "replace")
       marker = "\x1b[?1049l"
       tail = raw.rsplit(marker, 1)[-1] if marker in raw else raw
       clean = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", tail)
       clean = re.sub(r"\x1b[>=]", "", clean).replace("\r", "")
       hint = ""
       for ln in clean.splitlines():
           if "not complete" in ln or "Resume with" in ln:
               hint = ln.strip(); break
       print("MODE:", MODE)
       print("EXIT:", os.waitstatus_to_exitcode(status))
       print("HINT:", hint if hint else "<none>")
       PY
       ```
    8. `cargo build -p cowboy`
    9. `python3 "$T/pty_smoke.py" "$T/config.toml" ask-ctrlc`
    10. `python3 "$T/pty_smoke.py" "$T/config.toml" fail-exit`
    11. `python3 "$T/pty_smoke.py" "$T/config.toml" complete-exit`
    12. `python3 "$T/pty_smoke.py" "$T/config.toml" norun-ctrlc`
    13. `rm -rf "$T"`
    Each of steps 9-12 is one complete driver invocation that internally spawns
    the TUI under a PTY, performs that scenario's run/wait/exit, and prints the
    post-restoration line; no scenario bundles another scenario's step.
  - Expected result: step 9 prints
    `Run <id> is not complete. Resume with: cowboy answer <id> approval <answer>`;
    step 10 prints `Run <id> is not complete. Resume with: cowboy resolve <id>`;
    steps 11 and 12 print no `not complete` / `Resume with` line. Step 9
    exercises the `Ctrl+C` (`KeyHandling::Exit`) clean-exit path and step 10 the
    `/exit` (`exit_requested`) clean-exit path.
  - Observed result: steps 1-8 ran with exit status 0 (`cargo build -p cowboy`
    finished successfully). The catalog workflow ids are the entry filenames
    `ask`, `complete`, `fail`, used directly in the driver's `/run --workflow`
    commands. Step 9 (`ask-ctrlc`) printed `MODE: ask-ctrlc / EXIT: 0 / HINT:
    Run run-a8802180-eb00-4524-bdd6-0a26b58c568e is not complete. Resume with:
    cowboy answer run-a8802180-eb00-4524-bdd6-0a26b58c568e approval <answer>`.
    Step 10 (`fail-exit`) printed `MODE: fail-exit / EXIT: 0 / HINT: Run
    run-d0d449c5-07fd-4cf0-bb96-98ab480994f1 is not complete. Resume with:
    cowboy resolve run-d0d449c5-07fd-4cf0-bb96-98ab480994f1`. Step 11
    (`complete-exit`) printed `MODE: complete-exit / EXIT: 0 / HINT: <none>`;
    step 12 (`norun-ctrlc`) printed `MODE: norun-ctrlc / EXIT: 0 / HINT: <none>`
    (no `not complete` / `Resume with` line). Step 13 removed the temp fixtures.
    Both the `Ctrl+C` and `/exit` clean-exit paths were exercised; run ids are
    fresh per run and the answer/resolve/no-hint mapping matches exactly.
- [x] TODO-06: Add the `print_resume_hint` helper unit test in `app.rs`.
  - Procedure: add `#[test] print_resume_hint_writes_line_for_some_and_nothing_for_none`
    in module `app::tests` constructing a `Vec<u8>` writer, calling
    `print_resume_hint(&mut buf, Some("Run r1 is not complete. Resume with: cowboy resume r1"))`
    and asserting the buffer equals that line plus `\n`; call again with `None`
    and assert the buffer is empty. Run `cargo test -p cowboy print_resume_hint`.
  - Expected result: `test result: ok. 1 passed`; both assertions pass.
  - Observed result: added
    `print_resume_hint_writes_line_for_some_and_nothing_for_none` in
    `app::tests` asserting the buffer equals the line plus `\n` for `Some` and is
    empty for `None`; `cargo test -p cowboy print_resume_hint` reported
    `test result: ok. 1 passed; 0 failed`.
- [x] TODO-08: Add a testable TUI teardown seam and cover restoration-before-output and error suppression.
  - Procedure: add three `finish_tui_*` `#[test]`s in module `app::tests` using a
    fake `TerminalRestore` that records `"restore"` and a writer that records
    `"write"` into a shared `Vec<String>`. Assert `finish_tui(Ok(Some(line)), ...)`
    records `["restore","write"]` and writes `"<line>\n"`;
    `finish_tui(Ok(None), ...)` records `["restore"]` and writes nothing;
    `finish_tui(Err(e), ...)` records `["restore"]`, writes nothing, and returns
    the same error. Run `cargo test -p cowboy finish_tui`.
  - Expected result: `test result: ok. 3 passed`; all three cases pass, proving
    output occurs only after restoration and is suppressed on `None`/error.
  - Observed result: added `finish_tui_restores_before_writing_hint`
    (records `["restore","write"]` and writes `"<line>\n"`),
    `finish_tui_suppresses_hint_on_none` (records `["restore"]`, writes nothing),
    and `finish_tui_returns_loop_error_and_suppresses_hint` (records
    `["restore"]`, writes nothing, returns the same error `"loop boom"`) in
    `app::tests` using a fake `TerminalRestore` and a recording writer sharing one
    `Vec<String>` log. `cargo test -p cowboy finish_tui` reported
    `test result: ok. 3 passed; 0 failed`.
