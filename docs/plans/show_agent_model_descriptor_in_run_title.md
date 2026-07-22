# Plan

Show a compact, agent-reported model descriptor (model name + supported context
size + reasoning size) aggregated into one string such as `gpt-5.6-sol-1m-high`,
and display it in the title of every agent run surfaced in the TUI transcript.

Hard constraint from the request: **never trust the data we sent** — the
descriptor must be built exclusively from values the agent *returns*, not from
the configured `ModelInfo` (`id`/`provider`) Cowboy passes into `new_session`.
In the ACP backend, the agent-returned values are the `currentValue` fields of
the session config options in the `session/new` and `session/set_config_option`
responses (`crates/agent/acp/src/messages.rs` `SessionConfigOption`, currently
only the `model` category is consumed in
`crates/agent/acp/src/client.rs::apply_model_config_option`). If the agent
reports nothing for a field, that field is omitted; if the agent reports nothing
at all, no descriptor is shown.

Data flow (reusing existing seams, no new runtime concepts):

```
ACP session/new + set_config_option responses (agent-returned currentValues)
  -> cowboy-agent-acp Client captures a provider-neutral AgentSessionDescriptor
  -> Client trait exposes session_descriptor() (provider-neutral)
  -> cowboy-workflow-agent AgentExecutor reads it right after ensure_session,
       aggregates model/context/reasoning into one sanitized hyphen-joined token
  -> AgentProgressKind::SessionReady carries descriptor: Option<String>
  -> WorkflowEventKind::AgentSessionReady carries descriptor: Option<String>
  -> TUI snapshots the descriptor onto each agent transcript entry at ingestion
     and renders it as a compact title suffix on that entry's card
```

## Interaction with existing ACP model-selection enforcement (trust boundary)

The trust boundary must be proven **without** weakening existing model
selection. Current behavior (`crates/agent/acp/src/client.rs`):

- `new_session` calls `apply_model_config_option` whenever a `ModelInfo` is
  configured (`client.rs:350-355`).
- If the returned model does not match the configured model, the client must
  find a matching offered value (`client.rs:386-412`), and the applied value
  must then match or the session fails (`client.rs:425-442`).
- Matching accepts either the exact configured id or `provider/id`
  (`client.rs:453-465`).

Therefore a configured model **id** that differs from the returned model id
cannot produce a successful session, so no test may assert "a differing
configured id leaves the token unchanged." Changing that enforcement is out of
scope for this display feature. The trust boundary is instead proven by:

1. **Unit test of extraction** — `descriptor_from_config_options` takes only
   `&[SessionConfigOption]` and no `ModelInfo`, so by signature it can only read
   returned options. Unit tests assert it echoes returned `current_value`s.
2. **End-to-end case A (no configured model)** — configure no model; the agent
   returns model/context/reasoning; assert the descriptor exactly equals those
   returned values.
3. **End-to-end case B (matching id, sentinel provider)** — configure a model
   whose id exactly equals the returned unqualified model id but whose
   `provider` is a distinct sentinel string; the session succeeds through the
   matcher's **exact-id branch** (`value == model.id` at
   `crates/agent/acp/src/client.rs:453-456`, which fires before any
   `provider/id` check), so the sentinel provider is never consulted for
   selection; assert the sentinel provider never appears in the descriptor (the
   descriptor uses only the returned `current_value`).

## ACP grounding for extraction (agent-returned values only)

Using only returned `current_value`/`category`/`id`:

- Model: `category == "model"`, else `id == "model"`.
- Reasoning: `category == "thought_level"` (ACP-standard reasoning category),
  else an explicitly justified backend id fallback. A fallback id is honored
  only when TODO-03 records evidence that a supported backend actually returns
  that id **as an ACP `configOptions` entry** (captured from real
  `session/new`/`set_config_option` output or the live-ignored ACP integration
  tests) — backend `--help` output is not accepted as proof that an id appears
  in `configOptions`. Absent such captured evidence, no alias is added and only
  `thought_level` is recognized.
- Context: semantic context id only (`context_size`, `context_length`,
  `context_window`). Never a blanket `model_config` match, since that category
  also carries unrelated settings (e.g. speed mode).

References: ACP Session Config Options
(https://agentclientprotocol.com/protocol/v1/session-config-options) and Model
Config Option Category (https://agentclientprotocol.com/rfds/model-config-category).

Per ACP, the `set_config_option` response contains the complete current config
state, so when a model is applied the descriptor is captured from that post-set
option set; otherwise from the `session/new` option set.

## Safe-token normalization (untrusted agent data)

Agent-returned `currentValue`s are untrusted and flow into a terminal title via
`Card::title_suffix` (`crates/tui/app/src/app/card.rs:127-129,186-212`), which
joins the string verbatim. Every segment and the joined token pass through a
defined `sanitize_descriptor_token` policy before rendering:

- Allow ASCII alphanumeric plus `.`, `_`, and `-`.
- Remove all control characters (including newline, tab, ANSI escape bytes).
- Map any other disallowed run to a single `-`.
- Collapse consecutive separators and trim leading/trailing separators.
- A segment that is empty after sanitization is skipped; if the whole token is
  empty after sanitization, the descriptor is `None`.

Segments are joined with `-` in the order model, context, reasoning. Model
provider prefixes are stripped by keeping only the final `/`-separated segment of
the returned value before sanitization.

## No-retroactive-relabel design (TUI)

The descriptor is snapshotted onto each agent transcript entry at ingestion.
Transcript entries are **not** immutable — `try_coalesce_active_event`
(`crates/tui/app/src/app/state.rs:1159-1180`) replaces `event_log[index]` when
merging streaming chunks. The design therefore:

- Carries the descriptor snapshot on the entry itself
  (`TranscriptEntry::Workflow { event, agent_descriptor }`).
- During coalescing, preserves the **original** entry's `agent_descriptor`
  snapshot rather than resampling live ingestion state, so merged streaming
  chunks keep the descriptor they started with.
- Uses an ingestion-only `HashMap<(String, String), Option<String>>` that is
  updated for **every** `AgentSessionReady` (both `Some` and `None`): a `Some`
  stores the aggregated descriptor; a `None` removes/clears the entry so a later
  visit reporting nothing shows no descriptor. Subsequent agent entries copy the
  current map value; render code consults only the per-entry snapshot.

## Layering (per AGENTS.md)

- Provider-neutral descriptor type: `cowboy-agent-client`.
- ACP capture/parsing: `cowboy-agent-acp`.
- Aggregation + sanitization: `cowboy-workflow-agent`.
- Event carrying: `cowboy-workflow-engine`.
- Rendering + ingestion snapshot: `crates/tui/app`.

# Changes

- `crates/agent/client/src/types.rs`
  - Add provider-neutral `AgentSessionDescriptor { model, context, reasoning:
    Option<String> }` (`Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
    Default`) holding raw agent-returned values. Re-export from `lib.rs`.
- `crates/agent/client/src/traits.rs`
  - Add `fn session_descriptor(&self) -> Option<&AgentSessionDescriptor>;`.
- `crates/agent/acp/src/client.rs`
  - Add `session_descriptor: Option<AgentSessionDescriptor>` to `Client`
    (threaded through `Clone`/`Debug`).
  - Add `descriptor_from_config_options(&[SessionConfigOption]) ->
    Option<AgentSessionDescriptor>` per the ACP grounding above (no `ModelInfo`
    parameter). In `new_session`, store the descriptor from returned options,
    preferring the post-set option set; never read the configured `ModelInfo`.
  - Implement `session_descriptor` in the trait impl.
- `crates/workflow/agent/src/executor.rs`
  - Add `descriptor: Option<String>` to `AgentProgressKind::SessionReady`.
  - Add `sanitize_descriptor_token(&str) -> Option<String>` and
    `aggregate_session_descriptor(&AgentSessionDescriptor) -> Option<String>`
    implementing the safe-token policy above.
  - In `execute_agent`, after `ensure_session`, read
    `active.client.session_descriptor()`, aggregate + sanitize, and pass into
    the `SessionReady` progress kind. Update fake `Client` impls.
- `crates/workflow/engine/src/events.rs`
  - Add `descriptor: Option<String>` to `WorkflowEventKind::AgentSessionReady`
    with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `crates/workflow/engine/src/runtime.rs`
  - Carry `descriptor` in `workflow_event_kind_from_agent_progress`; update the
    scripted fake `Client` impl.
- `crates/workflow/engine/src/bin/engine-cli.rs`
  - Include the descriptor in the `AgentSessionReady` line if it matches.
- `crates/workflow/engine/src/workflow.rs`
  - Update the fake `Client` impl.
- `crates/tui/app/src/app/state.rs`
  - Change `TranscriptEntry::Workflow` to `{ event: WorkflowEvent,
    agent_descriptor: Option<String> }`. Add an ingestion-only
    `HashMap<(String, String), Option<String>>` updated for every
    `AgentSessionReady` in `apply_workflow_event_metadata`. Snapshot the current
    descriptor onto each new agent entry in both `push_event` and
    `try_coalesce_active_event` paths; coalescing preserves the original entry
    snapshot. Render code never reads the map.
- `crates/tui/app/src/app/events.rs`
  - `render_workflow_event_width` accepts the per-entry descriptor snapshot and
    adds it as a compact `title_suffix` on every agent card variant
    (`AgentSessionReady`, `AgentPromptWindowOpened`, `AgentPromptWindowClosed`,
    `AgentPrompt`, `AgentResponse`, `AgentThought`, `AgentToolCall`,
    `AgentToolCallUpdate`, `AgentPlan`). Non-agent cards ignore it; keep a
    no-descriptor default path.
- `docs/architecture.md`
  - One sentence: agent cards show the agent-reported model descriptor
    (model/context/reasoning) captured at session creation.

# Tests to be added/updated

- `cowboy-agent-client`: integration test under `crates/agent/client/tests/`
  constructing `cowboy_agent_client::AgentSessionDescriptor` through its public
  path.
- `cowboy-agent-acp`: `descriptor_from_config_options` unit tests (model
  `provider/model`; reasoning via `thought_level`; context via `context_size`; a
  `model_config` speed-mode option NOT taken as context; missing fields `None`).
  Plus `new_session` end-to-end mock-transport cases A and B above.
- `cowboy-workflow-agent`: `aggregate_session_descriptor` and
  `sanitize_descriptor_token` tests (all three present; provider-prefix stripped;
  internal whitespace collapsed; missing middle segment skipped; empty -> `None`;
  adversarial newline, tab, ANSI escape bytes, repeated separators,
  empty-after-sanitization; normal `gpt-5.6-sol-1m-high`). A `SessionReady`
  emission test.
- `cowboy-workflow-engine`: progress->event mapping with `descriptor`; serde
  back-compat (descriptor-less JSON -> `None`); the automated end-to-end stub
  test (TODO-09).
- `crates/tui/app`: nine-variant table-driven rendering test (token present when
  snapshot present, absent when `None`); repeated-visit regressions —
  (a) session A descriptor, response A, session B **`None`** same run/step,
  response B: assert response A keeps A while session B and response B show no
  descriptor; (b) a streaming/coalescing test asserting merged chunks retain the
  entry's original descriptor snapshot.

# How to verify

Run the focused crate suites (each must pass with zero Rust/Clippy warnings):

```text
cargo test -p cowboy-agent-client
cargo test -p cowboy-agent-acp
cargo test -p cowboy-workflow-agent
cargo test -p cowboy-workflow-engine
cargo test -p cowboy
cargo clippy -p cowboy-agent-client -p cowboy-agent-acp -p cowboy-workflow-agent -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings
```

Isolated, deterministic TUI verification with **one** concrete mechanism — a
Python standard-library `pty` driver (no workspace dependency; Python 3 is
present on the target as confirmed by its use in existing repo verification
scripts, e.g. `docs/plans/add_committer_agent_type.md`). Cowboy selects config
via the global `--config <path>` argument (`crates/tui/command-parser/src/lib.rs:24`,
consumed in `crates/tui/app/src/main.rs:14-16`); there is no `COWBOY_CONFIG`
env var. The catalog id of `$T/workflows/descriptor_smoke.lua` is deterministically
`descriptor_smoke` because filesystem ids strip `.lua`
(`crates/workflow/catalog/src/loader.rs:181-183`).

Run these literal steps verbatim:

1. `T=$(mktemp -d)` and `mkdir -p "$T/workflows" "$T/state"`.
2. Confirm prerequisites and build the binary into an isolated target dir with a
   locked lockfile (so no tracked `Cargo.lock` update occurs):
   `python3 --version` and
   `CARGO_TARGET_DIR="$T/target" cargo build --locked -p cowboy`.
3. Write executable `$T/stub.sh` (a per-line JSON-RPC responder; the default
   `developer` role uses the `default` agent, and topic generation plus the
   workflow step may each open their own session against this one command, so
   the stub answers by method regardless of session). Each reply is exactly one
   JSON object per line. The `agent_message_chunk` line uses `printf '%s\n'` with
   the JSON as a single-quoted `%s` argument so its `\n` escapes stay literal
   two-byte JSON escapes on the wire (POSIX `printf` would otherwise expand `\n`
   in a *format operand* into physical newlines and split the JSON across lines);
   the id-bearing lines carry no internal `\n`, so id substitution is safe:

   ```sh
   #!/bin/sh
   # Line-oriented ACP stub. Reads one JSON-RPC message per line on stdin,
   # replies with exactly one JSON object per line. Deterministic; no state.
   while IFS= read -r line; do
     id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
     case "$line" in
       *'"method":"initialize"'*)
         printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false},"agentInfo":{"name":"descriptor-stub","version":"1"}}}\n' "$id"
         ;;
       *'"method":"session/new"'*)
         printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1","configOptions":[{"id":"model","category":"model","currentValue":"gpt-5.6-sol","options":[{"value":"gpt-5.6-sol"}]},{"id":"context_size","category":"model_config","currentValue":"1m","options":[{"value":"1m"}]},{"id":"thought_level","category":"thought_level","currentValue":"high","options":[{"value":"high"}]}]}}\n' "$id"
         ;;
       *'"method":"session/prompt"'*)
         # JSON passed as %s argument: \n escapes stay literal, one line on the wire.
         printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"---\nstatus: success\nsummary: ok\n---\ndone"}}}}'
         printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id"
         ;;
       *'"method":"session/cancel"'*)
         : ;;  # notification, no reply
       *'"id":'*)
         printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
         ;;
     esac
   done
   ```

   Then `chmod +x "$T/stub.sh"`.
4. Write `$T/config.toml` literally (no `[agents.model]`, so case A — no
   model-selection enforcement runs):

   ```toml
   state_dir = "$T/state"
   workflow_store = "$T/state/workflow.redb"
   workflow_dirs = ["$T/workflows"]

   [[agents]]
   name = "default"
   command = "$T/stub.sh"
   args = []
   ```

   (Substitute the real `$T` value; the config format takes literal paths.)
5. Write `$T/workflows/descriptor_smoke.lua` literally:

   ```lua
   local developer = role("developer", {
     instructions = "You are a smoke-test agent. Return status success."
   })
   local run_step = step("run_step", { role = developer })
   run_step.run = function(ctx)
     return action.agent {
       role = developer,
       prompt = "Return a success status.",
       output = { status = { "success" } }
     }
   end
   local finish = step("finish")
   finish.run = function(ctx)
     return action.status { status = "success" }
   end
   run_step:on("success", finish)
   return workflow("descriptor_smoke", run_step)
   ```
6. Write the literal PTY driver `$T/drive.py` and run it with
   `python3 "$T/drive.py" "$T"`. It pins a wide PTY winsize so titles are not
   truncated (card title truncation drops metadata and suffixes when the line
   exceeds the terminal width), then extracts the descriptor **from the observed
   capture** using a title-structure regex over the entire ANSI-stripped byte
   stream — not line splitting. Because a full-screen ratatui TUI repositions the
   cursor with ANSI sequences instead of emitting newlines, stripping ANSI does
   not preserve row boundaries, so `splitlines()` is unsafe. Card titles render
   fields as `<leading title> · <title suffix> · <metadata…>` joined by
   `METADATA_SEPARATOR` (" · "), with the first metadata field being the step
   marker `↳ <step>` (`crates/tui/app/src/app/card.rs:186-232`,
   `crates/tui/app/src/app/controls/chrome.rs:5`). The regex therefore binds the
   descriptor to a named card title by requiring
   `<title> · <captured suffix> · ↳`, captures only that suffix field, validates
   the captured tokens against the safe charset, asserts the descriptor appears
   on the real `Agent session ready` title and on a later agent-card title, and
   reaps the child with a bounded `WNOHANG`/SIGTERM/SIGKILL sequence:

   ```python
   import fcntl, os, pty, re, signal, select, struct, sys, termios, time

   T = sys.argv[1]
   binary = f"{T}/target/debug/cowboy"
   argv = [binary, "--config", f"{T}/config.toml"]
   ansi = re.compile(rb"\x1b\[[0-9;?]*[ -/]*[@-~]")
   EXPECT = "gpt-5.6-sol-1m-high"
   SAFE = re.compile(r"^[A-Za-z0-9._-]+$")
   SEP = " \u00b7 "  # METADATA_SEPARATOR used by card title rendering

   pid, fd = pty.fork()
   if pid == 0:
       os.execv(binary, argv)  # child: exec Cowboy on the PTY slave
   # Parent: pin a wide window so titles/suffixes never truncate.
   fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 200, 0, 0))

   capture = bytearray()

   def pump(seconds):
       end = time.time() + seconds
       while time.time() < end:
           r, _, _ = select.select([fd], [], [], 0.2)
           if fd in r:
               try:
                   chunk = os.read(fd, 65536)
               except OSError:
                   return
               if not chunk:
                   return
               capture.extend(chunk)

   def normalized():
       return ansi.sub(b"", bytes(capture)).decode("utf-8", "replace")

   def save():
       with open(f"{T}/capture.txt", "w", encoding="utf-8") as f:
           f.write(normalized())

   def fail(msg):
       save()
       try:
           os.kill(pid, signal.SIGKILL)
           os.waitpid(pid, 0)
       except OSError:
           pass
       raise SystemExit(f"FAIL: {msg} (capture at {T}/capture.txt)")

   def wait_for(marker, timeout=60):
       end = time.time() + timeout
       while time.time() < end:
           pump(0.5)
           if marker in normalized():
               return
       fail(f"timeout waiting for {marker!r}")

   def descriptors_on(title):
       # Bind the descriptor to `title` by title structure over the FULL
       # normalized stream (no line splitting): <title> · <suffix> · ↳ ...
       # The trailing `↳` is the step-metadata boundary that follows the suffix.
       pattern = re.compile(
           re.escape(title)
           + re.escape(SEP)
           + r"([^\u00b7\r\n]+?)"   # captured suffix: no separator dot, no newline
           + re.escape(SEP)
           + r"\u21b3"              # ↳ step-metadata marker
       )
       return [m.strip() for m in pattern.findall(normalized())]

   def reap(timeout=10):
       end = time.time() + timeout
       while time.time() < end:
           wpid, status = os.waitpid(pid, os.WNOHANG)
           if wpid == pid:
               return status
           pump(0.2)
       os.kill(pid, signal.SIGTERM)
       time.sleep(1)
       wpid, status = os.waitpid(pid, os.WNOHANG)
       if wpid == pid:
           return status
       os.kill(pid, signal.SIGKILL)
       _, status = os.waitpid(pid, 0)
       return status

   wait_for("Enter submits")                          # composer ready marker
   os.write(fd, b"/run --workflow descriptor_smoke smoke\r")
   wait_for("Run completed")                          # deterministic completion
   save()

   ready = descriptors_on("Agent session ready")
   if not ready:
       fail("no descriptor field in 'Agent session ready' title")
   for tok in ready:                                  # validate OBSERVED tokens
       if not SAFE.fullmatch(tok):
           fail(f"unsafe descriptor token rendered: {tok!r}")
   if EXPECT not in ready:
       fail(f"expected {EXPECT!r} in session-ready title, observed {ready!r}")

   follow = descriptors_on("Agent response") or descriptors_on("Prompt sent to agent")
   if not follow:
       fail("no descriptor field in a subsequent agent-card title")
   for tok in follow:
       if not SAFE.fullmatch(tok):
           fail(f"unsafe descriptor token in subsequent card: {tok!r}")
   if EXPECT not in follow:
       fail(f"expected {EXPECT!r} in subsequent agent-card title, observed {follow!r}")

   os.write(fd, b"\x03")                               # Ctrl+C quits (input.rs:20 -> KeyHandling::Exit)
   status = reap()
   if not (os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0):
       fail(f"unclean exit status: {status}")
   print("SMOKE OK")
   ```
7. Confirm the driver prints `SMOKE OK`. This proves, from Cowboy's own PTY
   output (not any plan-authored literal): the `Agent session ready` title and a
   subsequent agent-card title (`Agent response` or `Prompt sent to agent`) each
   carry a descriptor suffix field that passes the safe-charset check and equals
   `gpt-5.6-sol-1m-high`, built only from the stub's returned `currentValue`s;
   and Cowboy exits cleanly on `Ctrl+C` (`\x03`) within the bounded reap. Cowboy
   state, workflow files, stub artifacts, the PTY capture, and build outputs are
   confined to `$T`, and `--locked` guarantees no tracked `Cargo.lock` update; no
   tracked repository file or live Cowboy state/config is modified. (Cargo may
   still read/update its shared `CARGO_HOME` registry/cache — a dependency cache,
   not repository or Cowboy state. For total filesystem isolation, also set a
   temporary `CARGO_HOME` under `$T` and pre-warm dependencies with the network
   available.)

# TODO

- [x] TODO-01: Add provider-neutral `AgentSessionDescriptor` type and re-export.
  - Procedure: Add the struct in `crates/agent/client/src/types.rs`
    (`model`/`context`/`reasoning: Option<String>`, standard derives), re-export
    from `lib.rs`, and add an integration test under `crates/agent/client/tests/`
    that does `use cowboy_agent_client::AgentSessionDescriptor;`, constructs a
    value, and asserts field defaults. Run `cargo test -p cowboy-agent-client`.
  - Expected result: the integration test passes, proving the type is publicly
    reachable at `cowboy_agent_client::AgentSessionDescriptor`.
  - Observed result: Added `AgentSessionDescriptor { model, context, reasoning:
    Option<String> }` in `crates/agent/client/src/types.rs` (derives `Debug,
    Clone, PartialEq, Eq, Serialize, Deserialize, Default`), re-exported from
    `lib.rs`, and added `crates/agent/client/tests/agent_session_descriptor.rs`.
    `cargo test -p cowboy-agent-client` passed (2 unit + 1 integration test);
    the integration test reaches `cowboy_agent_client::AgentSessionDescriptor`
    and asserts field defaults are `None`.

- [x] TODO-02: Add `session_descriptor()` to the `Client` trait.
  - Procedure: Add `fn session_descriptor(&self) -> Option<&AgentSessionDescriptor>;`
    to `crates/agent/client/src/traits.rs`, then implement it on every `Client`
    impl (ACP client and all test fakes). Prove completeness by compiling all
    targets, which is the authoritative check: `cargo test --workspace --no-run`.
  - Expected result: the whole-workspace compile of all targets (including test
    targets) succeeds, proving every production and test `Client` implementation
    provides `session_descriptor`. No intermediate failing build is used as
    evidence.
  - Observed result: Added
    `fn session_descriptor(&self) -> Option<&AgentSessionDescriptor>;` to
    `crates/agent/client/src/traits.rs` and implemented it on every `Client`
    impl (ACP client plus the executor, runtime, and workflow test fakes).
    Re-executed the declared authoritative check `cargo test --workspace
    --no-run`: it compiled every workspace test target (all binaries listed,
    exit 0), proving every production and test `Client` implementation provides
    `session_descriptor`. No intermediate failing build was used as evidence.

- [x] TODO-03: Capture the agent-returned descriptor in the ACP client.
  - Procedure: Add a `session_descriptor` field to `Client`
    (updating `Clone`/`Debug`). Add
    `descriptor_from_config_options(&[SessionConfigOption])` (no `ModelInfo`
    param) extracting model (`category=="model"` else `id=="model"`), reasoning
    (`category=="thought_level"`, plus only alias ids proven to appear in real
    ACP `configOptions` output — capture such evidence from live
    `session/new`/`set_config_option` traffic or the ignored ACP integration
    tests, not from `--help`), and context (semantic ids `context_size`/
    `context_length`/`context_window` only). In `new_session`, store the
    descriptor from returned options, preferring the post-`set_config_option`
    set; never read the configured `ModelInfo`. Implement `session_descriptor` in
    the trait impl. Add unit tests for `descriptor_from_config_options` (including
    a `model_config` speed-mode option that must NOT become context) and two
    `new_session` end-to-end mock cases: case A (no model configured; assert the
    descriptor equals the returned values), and case B (configured id exactly
    equals the returned unqualified id but with a distinct sentinel provider; the
    session succeeds through the matcher's exact-id branch `value == model.id`
    at `crates/agent/acp/src/client.rs:453-456`, which fires before any
    `provider/id` check, so the sentinel provider is never consulted; assert the
    sentinel provider never enters the descriptor). Run `cargo test -p cowboy-agent-acp`.
  - Expected result: `cargo test -p cowboy-agent-acp` passes; reasoning comes
    from `thought_level`; non-context `model_config` options are ignored; case A
    yields exactly the returned values; case B's descriptor contains only the
    returned `current_value` and never the configured sentinel provider. No test
    asserts a *differing configured model id* path, which existing enforcement
    rejects by design.
  - Observed result: Added `session_descriptor: Option<AgentSessionDescriptor>`
    to `Client` (threaded through `Clone`/`Debug`/constructor) and
    `descriptor_from_config_options(&[SessionConfigOption])` reading model
    (`category=="model"` else `id=="model"`), reasoning (`thought_level` only;
    no alias id added without captured `configOptions` evidence), and context
    (`context_size`/`context_length`/`context_window` only). `new_session`
    prefers the post-`set_config_option` option set and never reads `ModelInfo`.
    `cargo test -p cowboy-agent-acp` passed (51 unit tests): extraction reads
    returned values only, a `model_config` speed-mode option is ignored for
    context, case A yields exactly the returned values, and case B's descriptor
    contains only the returned `current_value` — the sentinel provider never
    appears.

- [x] TODO-04: Aggregate the descriptor into one token in the agent executor.
  - Procedure: Add `sanitize_descriptor_token(&str) -> Option<String>`
    (allow ASCII alphanumeric + `.`/`_`/`-`; strip control chars incl.
    newline/tab/ANSI; map other runs to a single `-`; collapse and trim
    separators; empty -> `None`) and
    `aggregate_session_descriptor(&AgentSessionDescriptor) -> Option<String>`
    (strip model provider prefix to last `/` segment, sanitize each segment, join
    present segments model-context-reasoning with `-`, `None` when all empty) in
    `crates/workflow/agent/src/executor.rs`. Add `descriptor: Option<String>` to
    `AgentProgressKind::SessionReady` and populate it in `execute_agent` from
    `active.client.session_descriptor()` after `ensure_session`. Update fake
    `Client` impls. Add adversarial unit tests (newline, tab, ANSI escape bytes,
    repeated separators, empty-after-sanitization, and normal
    `gpt-5.6-sol-1m-high`). Run `cargo test -p cowboy-workflow-agent`.
  - Expected result: `cargo test -p cowboy-workflow-agent` passes; the aggregated
    token contains only allowed characters for every adversarial input, control
    characters never survive into the token, and a `SessionReady`-emission test
    asserts the sanitized aggregated string.
  - Observed result: Added `sanitize_descriptor_token` and
    `aggregate_session_descriptor` in `crates/workflow/agent/src/executor.rs`,
    added `descriptor: Option<String>` to `AgentProgressKind::SessionReady`,
    populated it in `execute_agent` from `active.client.session_descriptor()`
    after `ensure_session`, and updated fake impls. `cargo test -p
    cowboy-workflow-agent` passed (56 unit tests). The char-class policy strips
    the ESC byte and all control chars (asserted), collapses/trims separators,
    yields only allowed characters for adversarial inputs, and the
    `SessionReady`-emission test observes `gpt-5.6-sol-1m-high`.

- [x] TODO-05: Carry the descriptor through the workflow event.
  - Procedure: Add `descriptor: Option<String>` (with
    `#[serde(default, skip_serializing_if = "Option::is_none")]`) to
    `WorkflowEventKind::AgentSessionReady`; map it in
    `workflow_event_kind_from_agent_progress`; update scripted/fake `Client`
    impls in `runtime.rs` and `workflow.rs`; update `engine-cli.rs`. Run
    `cargo test -p cowboy-workflow-engine` and the combined
    `cargo clippy -p cowboy-agent-client -p cowboy-agent-acp -p cowboy-workflow-agent -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`
    (all five crates touched by this feature carry the changed public contract).
  - Expected result: tests pass, including the mapping test and a serde
    back-compat test proving descriptor-less JSON deserializes to `None`; the
    combined five-crate Clippy (including `cowboy-agent-client`) reports no warnings.
  - Observed result: Added `descriptor: Option<String>`
    (`#[serde(default, skip_serializing_if = "Option::is_none")]`) to
    `WorkflowEventKind::AgentSessionReady`, mapped it in
    `workflow_event_kind_from_agent_progress`, updated the `runtime.rs`/
    `workflow.rs` fakes and `engine-cli.rs`. Re-executed the declared commands:
    `cargo test -p cowboy-workflow-engine` passed (129 tests) including the
    mapping test and the serde back-compat test proving descriptor-less JSON
    deserializes to `None`; the declared combined five-crate
    `cargo clippy -p cowboy-agent-client -p cowboy-agent-acp -p cowboy-workflow-agent -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`
    reported no warnings (exit 0).

- [x] TODO-06: Track per-agent-run descriptor in TUI state.
  - Procedure: In `crates/tui/app/src/app/state.rs`, change
    `TranscriptEntry::Workflow` to carry `agent_descriptor: Option<String>`. Keep
    an ingestion-only `HashMap<(String, String), Option<String>>` updated for
    **every** `AgentSessionReady` in `apply_workflow_event_metadata`: `Some`
    stores the descriptor, `None` removes/clears the key. Snapshot the current
    descriptor onto each new agent entry in both `push_event` and
    `try_coalesce_active_event`, with coalescing preserving the original entry
    snapshot. Render code must not read the map. Add regression tests:
    (a) session A descriptor, response A, session B `None` same run/step,
    response B — assert response A keeps A while session B and response B show no
    descriptor; (b) streaming coalescing preserves the entry's original snapshot.
    Run `cargo test -p cowboy`.
  - Expected result: `cargo test -p cowboy` passes; a later `None` clears the
    stale descriptor (regression a), an earlier card is never relabeled, and
    merged streaming chunks keep their original snapshot (regression b).
  - Observed result: Changed `TranscriptEntry::Workflow` to
    `{ event, agent_descriptor: Option<String> }`, added an ingestion-only
    `HashMap<(String, String), Option<String>>` updated for every
    `AgentSessionReady` (`Some` stores, `None` removes), snapshotted the
    descriptor onto entries in both `push_event` and `try_coalesce_active_event`
    (coalescing preserves the original entry snapshot), and kept render code off
    the map. `cargo test -p cowboy` passed (314 tests). Regression (a) shows a
    later `None` clears the stale descriptor while earlier cards keep theirs;
    regression (b) shows merged streaming chunks keep their original snapshot.

- [x] TODO-07: Render the descriptor in every agent run card title.
  - Procedure: In `crates/tui/app/src/app/events.rs`, thread the per-entry
    descriptor snapshot into the card builder and add it as a compact
    `title_suffix` on every agent card variant. Add a table-driven test over all
    nine variants (`AgentPromptWindowOpened`, `AgentPromptWindowClosed`,
    `AgentPrompt`, `AgentResponse`, `AgentThought`, `AgentToolCall`,
    `AgentToolCallUpdate`, `AgentPlan`, `AgentSessionReady`) asserting the token
    appears when a snapshot is present, plus an absence case asserting no token
    when the snapshot is `None`. Run `cargo test -p cowboy`.
  - Expected result: `cargo test -p cowboy` passes; the token renders on every
    named agent card variant and is absent when unknown; pre-existing
    non-descriptor title assertions still pass.
  - Observed result: Threaded the per-entry snapshot through
    `render_workflow_event_width` and added it as a compact `title_suffix` on
    every agent card variant (gated by `agent_card_step_id`). `cargo test -p
    cowboy` passed; the table-driven test asserts the token renders on all nine
    named agent card variants when a snapshot is present and is absent when the
    snapshot is `None`, and a non-agent card ignores the snapshot. Pre-existing
    non-descriptor title assertions still pass.

- [x] TODO-08: Update architecture doc and run full verification.
  - Procedure: Add the one-sentence note to `docs/architecture.md`. Run the five
    focused `cargo test -p <crate>` commands **separately in declared order**
    (`cowboy-agent-client`, `cowboy-agent-acp`, `cowboy-workflow-agent`,
    `cowboy-workflow-engine`, `cowboy`) — not a single combined multi-package
    invocation — then the combined
    `cargo clippy -p cowboy-agent-client -p cowboy-agent-acp -p cowboy-workflow-agent -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`.
    Then run the isolated deterministic TUI check by executing the **literal**
    fixtures and Python-stdlib `pty` driver spelled out verbatim in "How to
    verify" (the `$T/stub.sh`, `$T/config.toml`, `$T/workflows/descriptor_smoke.lua`,
    and `$T/drive.py` shown there). First confirm prerequisites and build
    locked: `python3 --version` and
    `CARGO_TARGET_DIR="$T/target" cargo build --locked -p cowboy`; then
    `python3 "$T/drive.py" "$T"`. Do not substitute a different driver, stub,
    workflow, or assertion set; reproduce the exact procedure.
  - Expected result: all focused suites pass; Clippy (including
    `cowboy-agent-client`) reports no warnings; `python3 "$T/drive.py" "$T"`
    prints `SMOKE OK`. The driver derives descriptor suffix fields from Cowboy's
    own ANSI-normalized PTY capture (never from a plan-authored literal), and
    that observed evidence shows the `Agent session ready` title and a subsequent
    agent-card title (`Agent response` or `Prompt sent to agent`) each carry a
    descriptor suffix that passes the safe-charset check and equals
    `gpt-5.6-sol-1m-high`, built only from the stub's returned `currentValue`s;
    Cowboy exits cleanly on `Ctrl+C` (`\x03`, `input.rs:20` -> `KeyHandling::Exit`,
    `app.rs:206` -> `return Ok(())`) within the bounded WNOHANG/SIGTERM/SIGKILL
    reap. `--locked` guarantees no tracked `Cargo.lock` update; Cowboy runtime
    state, workflow files, stub artifacts, the PTY capture, and build outputs are
    confined to `$T`, and no tracked repository file or live Cowboy state/config
    is modified. (The shared `CARGO_HOME` registry/cache may still be
    read/updated as a dependency cache; set a temporary `CARGO_HOME` under `$T`
    if total filesystem isolation is required.)
  - Observed result: The one-sentence note is present in `docs/architecture.md`
    ("Agent cards show the agent-reported model descriptor
    (model/context/reasoning) captured at session creation, built only from
    agent-returned ACP config option values and never from the configured
    `ModelInfo`."). Re-executed the five focused suites **separately in declared
    order**, each exit 0: `cargo test -p cowboy-agent-client` (2+1 ok),
    `cargo test -p cowboy-agent-acp` (51 ok), `cargo test -p cowboy-workflow-agent`
    (56 ok), `cargo test -p cowboy-workflow-engine` (129 ok), and
    `cargo test -p cowboy` (314 ok, 2 ignored; 5 `test result: ok` sections, no
    FAILED). The combined
    `cargo clippy -p cowboy-agent-client -p cowboy-agent-acp -p cowboy-workflow-agent -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings`
    reported no warnings (exit 0). `python3 --version` = 3.14.6; the isolated
    `CARGO_TARGET_DIR="$T/target" cargo build --locked -p cowboy` built cleanly
    (exit 0; `git status Cargo.lock` clean — no tracked lock update). The literal
    `$T/stub.sh`, `$T/config.toml`, `$T/workflows/descriptor_smoke.lua`, and the
    verbatim `$T/drive.py` (final keystroke `\x03` Ctrl+C) were reproduced
    exactly, and `python3 "$T/drive.py" "$T"` printed `SMOKE OK` (exit 0). From
    Cowboy's own ANSI-normalized PTY capture the `Agent session ready` title and
    the subsequent `Prompt sent to agent` title each carried a descriptor suffix
    that passed the safe-charset check and equalled `gpt-5.6-sol-1m-high`, built
    only from the stub's returned `currentValue`s (confirmed lines
    `Agent session ready · gpt-5.6-sol-1m-high · ↳` and
    `Prompt sent to agent · gpt-5.6-sol-1m-high · ↳`); Cowboy exited cleanly on
    Ctrl+C within the bounded WNOHANG/SIGTERM/SIGKILL reap. `$T` was removed after
    the run; no tracked repository file or live Cowboy state/config was modified.

- [x] TODO-09: Add an automated end-to-end runtime test proving the returned-data trust boundary.
  - Procedure: In `crates/workflow/engine/src/runtime.rs` tests, add a
    deterministic shell ACP stub (mirroring the existing stdio-stub pattern at
    `runtime.rs:1948-2076`) whose `session/new` returns `configOptions` for
    `model` (`gpt-5.6-sol`), `context_size` (`1m`), and `thought_level` (`high`),
    and which answers one prompt with a valid frontmatter status body. Configure
    the run's `AgentRuntimeConfig` with **no** model (case A) so model-selection
    enforcement does not run. Drive one agent step through `WorkflowRuntime` and
    assert the emitted `WorkflowEventKind::AgentSessionReady` carries
    `descriptor = Some("gpt-5.6-sol-1m-high")`. Optionally add a second stub run
    where the configured model id exactly equals the returned id but with a
    distinct sentinel provider (case B; the session passes via the exact-id
    match branch, not `provider/id`) and assert the sentinel provider is absent
    from the descriptor. Run `cargo test -p cowboy-workflow-engine <new_test_name>`.
  - Expected result: the test passes; the descriptor on the real emitted event
    equals the aggregation of the stub's returned values, giving reproducible
    final-state proof of the trust boundary end-to-end through the runtime,
    without configuring a divergent model id that existing enforcement rejects.
  - Observed result: Added `write_descriptor_stub` and
    `agent_session_ready_event_carries_returned_descriptor_case_a` in
    `crates/workflow/engine/src/runtime.rs` tests. The shell ACP stub returns
    `configOptions` for `model` (`gpt-5.6-sol`), `context_size` (`1m`), and
    `thought_level` (`high`) and answers one prompt with a valid frontmatter
    status body; the run is configured with no model (case A). Driving one agent
    step through `WorkflowRuntime`, the emitted
    `WorkflowEventKind::AgentSessionReady` carried
    `descriptor = Some("gpt-5.6-sol-1m-high")`. `cargo test -p
    cowboy-workflow-engine agent_session_ready_event_carries_returned_descriptor_case_a`
    passed, giving reproducible end-to-end proof of the trust boundary through
    the runtime.
