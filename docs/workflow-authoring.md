# Workflow authoring

Cowboy workflows are Lua files that compile into a durable workflow graph. A run starts at the workflow head step, each step returns one `action.*` value, and the action output status routes to the next step.

```text
.lua source
  -> role(...) declarations
  -> step(...) declarations with run(ctx)
  -> workflow(name, head)
  -> status transitions: step:on(status, next_step)
```

The Lua VM is sandboxed and recreated for each compile or step execution. Persist run state through action outputs and `ctx.prev`; do not depend on mutable Lua globals surviving between steps.

## File placement

Custom workflows are `.lua` files under a configured `workflow_dirs` entry. By default, Cowboy scans `${XDG_CONFIG_HOME:-~/.config}/cowboy/workflows`, so a user workflow can live at `~/.config/cowboy/workflows/my-workflow.lua`. For project-local workflows, add a project directory such as `.cowboy/workflows` to `workflow_dirs` in `${XDG_CONFIG_HOME:-~/.config}/cowboy/config.toml`, then put `.lua` files under that directory:

```toml
workflow_dirs = [".cowboy/workflows", "~/.config/cowboy/workflows"]
```

Cowboy scans configured workflow directories recursively and uses each relative `.lua` path as the workflow id without the `.lua` suffix; for example, `.cowboy/workflows/review/security.lua` is cataloged as `review/security`. The built-in default workflow is always available, so custom workflows only need to be added when the default developer flow is not enough.

`workflow_dirs` is a low-to-high precedence overlay stack. When multiple roots
contain the same relative Lua path, the later root provides that workflow entry.
Its `require` calls search the selected root first, then each lower-precedence
root. This permits a project overlay such as `github/assignment.lua` to reuse
`github/common.lua` from a base directory while still overriding that helper
locally. Imports must remain relative paths inside one of those roots; absolute
paths, `..` traversal, and escaping symlinks are rejected rather than falling
back. Cowboy snapshots the resolved entry and every imported file, so resumed
runs are unaffected by later source changes.

A workflow file must return one `workflow(...)` table:

```lua
local implement = step("implement")
implement.run = function(ctx)
  return action.status { status = "success" }
end

return workflow("my-workflow", implement, {
  description = "Short selector-facing description"
})
```

## Core authoring API

### `role(id, config)`

Declares reusable role metadata.

```lua
local developer = role("developer", {
  instructions = "Implement focused changes and explain the result.",
  agent = "default",
  language = "rust"
})
```

Accepted forms:

```lua
role("developer")
role("developer", "Instruction text")
role("developer", { instructions = "Instruction text", agent = "planner", custom = "metadata" })
```

Rules:

- `id` must be a non-empty string.
- `instructions` defaults to `""`.
- `agent`, when present, must be a non-empty string naming a configured `[[agents]]` entry.
- Extra table fields other than `instructions` and `agent` are preserved as role properties in the compiled definition.
- `action.agent` and `step(..., { role = ... })` accept either the role table or the role id string.
- The role id is also the agent-session reuse key within a run. Keep ids stable and role-specific.

### `step(id, config)`

Declares one workflow state. Every step must define `step.run = function(ctx) ... end`.

```lua
local implement = step("implement", {
  role = developer,
  purpose = "make code changes"
})

implement.run = function(ctx)
  return action.agent {
    role = developer,
    prompt = "Implement the approved change using the selected workflow context.",
    output = {
      status = { "success", "failed", "needs_fix" },
      fields = {
        summary = "string",
        files = "array"
      }
    }
  }
end
```

Accepted config fields:

- `role`: optional default role metadata for the step; preserved on `ctx.step.role` and validated against declared roles.
- `run`: optional function supplied inline instead of assigning `step.run` later.
- Any other fields become `ctx.step.properties` at runtime.

Runtime context passed to `run(ctx)`:

| Field | Meaning |
| --- | --- |
| `ctx.request` | Original user request for the run. |
| `ctx.user_inputs` | Ordered initial request plus every durably accepted on-the-fly prompt; see schema below. |
| `ctx.run_id` | Stable run id. |
| `ctx.workflow.name` | Current workflow name. |
| `ctx.workflow.head` | Workflow head step id. |
| `ctx.current_step` | Current step id. |
| `ctx.step.id` | Current step id. |
| `ctx.step.role` | Current step's configured role id, or `nil`. |
| `ctx.step.properties` | Non-reserved fields from the step config. |
| `ctx.prev` | Latest completed step output, including completed wait-for-input inputs, or `nil` on the first step. |
| `ctx.steps_executed` | Number of already-executed steps in the run. |
| `ctx.system.os` | Host operating system (`std::env::consts::OS`), e.g. `linux`. |
| `ctx.system.arch` | Host CPU architecture (`std::env::consts::ARCH`), e.g. `x86_64`. |
| `ctx.system.family` | Host OS family (`std::env::consts::FAMILY`), e.g. `unix`. |
| `ctx.system.hostname` | Machine hostname, or `nil` if unavailable. |
| `ctx.system.username` | Current user name, or `nil` if unavailable. |
| `ctx.system.home_dir` | Home directory (`HOME`/`USERPROFILE`), or `nil` if unavailable. |
| `ctx.system.cwd` | Process working directory, or `nil` if unavailable. |

`ctx.user_inputs` has this exact shape and is ordered by `sequence`:

```lua
{
  {
    sequence = 0,
    kind = "initial",
    content = "the original request",
    submitted_at = "2026-01-02T03:04:05.000Z",
  },
  {
    sequence = 1,
    kind = "follow_up",
    content = "the first accepted on-the-fly prompt",
    submitted_at = "2026-01-02T03:05:06.000Z",
  },
}
```

Sequence `0` is synthesized from `WorkflowRun.original_request` and the run's
creation timestamp. Durable follow-ups start at `1`. Timestamps are UTC RFC
3339 with millisecond precision and a `Z` suffix. Cowboy uses
`content.trim().is_empty()` only to reject empty input, then stores and forwards
every accepted string byte-for-byte, including leading/trailing whitespace and newlines.
`ctx.request` remains the unchanged original request.

Ordinary runs use `"kind": "initial"` for sequence `0`. A terminal TUI restart
creates a new run whose sequence `0` uses `"kind": "restart"` and whose
`content` is the exact restart submission, including leading/trailing whitespace
and newlines. Restart does not expose inputs from the source run.

Every fresh agent session receives the complete `ctx.user_inputs` history.
Reused sessions receive only entries whose sequence has not already been
delivered, regardless of the workflow-authored `prompt`.
Inputs to `action.wait_for_input` are deliberately excluded: they remain available
only through `ctx.prev.fields.input` because they are workflow control-point
answers rather than on-the-fly direction.

### Agent prompt layering

The Rust agent layer is the canonical final prompt assembler. Legacy
prompt-only actions retain their existing full-prompt behavior. Structured
actions separate a stable task contract from the current turn. The role block
is sent once per backend session; task instructions and deliverable format are
sent once per `(run, role, task.key, contract fingerprint)`; recovery context is
sent with that contract and after fresh-session fallback; the current turn and
new user-input sequences are sent per dispatch. A same-session retry sends only
the retry correction and new user inputs.

Restarted runs reuse the source run's exact workflow snapshot and config-set
name. Persisted role sessions are copied per role with the same backend session
id and static-delivery fingerprints, but their per-run input watermark resets so
the restart sequence `0` is delivered once. Copied sessions use exact supplied
session semantics: if the backend cannot load the recorded session, execution
fails and Cowboy does not create a replacement or replay role/task contracts.
Nested `action.workflow` runs apply the same rule recursively to their own
snapshots and role sessions.

Reuse requires both an equal `task.key` and an equal fingerprint of
`task.instructions` plus the `output` specification. Steps such as
`implement` and `revise` therefore import one shared contract definition.
Initial objectives and revision-specific wording belong only in `task.turn`;
stage-specific durable state belongs in `task.recovery_context`. Do not derive
fingerprinted instructions from a step-specific objective, heading, or change
request.

Task keys are scoped to one run and role. Reuse a key when work returns to the
same responsibility (for example `implement` and `revise`), and use distinct
keys for distinct responsibilities such as testing and implementation review.
Do not concatenate `ctx.request`,
`ctx.user_inputs`, a `## User Inputs` heading, or cumulative-input boilerplate
into the Lua prompt; doing so duplicates user direction.

The example workflows use `utils/context.lua::build_agent_prompt(ctx, spec)` to
declare that selected context explicitly:

```lua
local prompt, errors = context.build_agent_prompt(ctx, {
  objective = "Run focused tests for the approved implementation.",
  heading = "Implementation result:",
  require_previous = true,
  fields = { "user_feedback", "plan_doc", "repro_test" },
  evidence = { { name = "implementation", required = true } },
  guidance = { "preserve_user_feedback", "preserve_evidence" },
  instructions = "Run each applicable TODO procedure in plan order.",
})
if not prompt then
  return context.invalid_context_action(ctx, "failed", errors)
end
```

Field and evidence order is deterministic. Optional absent context is omitted,
while a selected valid empty array is rendered as `array(empty)`. Every selected
evidence source requires both its command array and evidence array, even when
the source selection is optional. Both arrays may be explicitly empty, but if
only one is present the builder returns a field-specific non-agent result that
names the missing field and its present partner. Missing or malformed required
context is handled the same way; invalid context is never converted to
placeholder text or sent to the agent. Select only the metadata, artifact
references, evidence sources, body, and reusable guidance needed by the current
stage. Preserve cumulative raw `user_feedback` separately when workflow-control
answers must be shown; never merge reviewer or agent feedback into it.

`ctx.system` is a curated, read-only, best-effort snapshot of the execution
environment (modelled on the identity data `chezmoi` exposes to templates). Any
field that cannot be determined is `nil`; gathering context never fails a step.
Only this fixed allow-list is exposed — the full process environment is
intentionally **not** provided, because environment variables often contain
secrets that would otherwise leak into persisted run records and event logs.

`ctx.prev` has this shape when a previous step completed:

```lua
{
  record_id = "run-...-2",
  step = "implement",
  action = "agent",       -- or "status"
  output = {
    status = "needs_fix",
    fields = { summary = "...", files = { "src/lib.rs" } },
    body = "Markdown details",
    raw = "original parsed value"
  },
  -- Compatibility aliases for snapshotted workflows:
  status = "needs_fix",
  fields = { summary = "...", files = { "src/lib.rs" } },
  body = "Markdown details",
  raw = "original parsed value"
}
```

### `workflow(name, head, config)`

Builds the workflow definition returned by the file.

```lua
return workflow("feature-flow", implement, "Implement and summarize feature requests")
```

Accepted forms:

```lua
workflow("feature-flow", implement)
workflow("feature-flow", implement, "Description")
workflow("feature-flow", implement, { description = "Description" })
workflow("feature-flow", implement, {
  description = "Description",
  config_set = "careful"
})
```

Rules:

- `name` must be non-empty.
- `head` must be a declared step table.
- `description` is optional and used by workflow selection/catalog display.
- `config_set` is optional, must be a nonblank string, and defaults to `default` when omitted.
- All declared steps are compiled; validation rejects an unknown head, unknown roles, unknown transition targets, and empty transition statuses.

### Runtime config sets

The host config defines named runner policies:

```toml
[config_sets.default]
max_steps_per_run = 100
max_visits_per_step = 20
max_retries_per_run = 200
max_retries_per_step = 2
# agent_human_input = true  # Opt in; omitted/false preserves existing behavior.

[config_sets.careful]
# Omitted fields independently inherit 100, 20, 200, and 2.
max_retries_per_run = 20
max_retries_per_step = 4
```

The built-in `default` set always exists. Each omitted field inherits its shown
built-in value; `agent_human_input` independently defaults to `false`. Retry
limits may be `0`; step and visit limits must be greater than zero. Blank names
and unknown fields are rejected. An unknown workflow selection fails before a
new run is persisted and reports the available sets.

Cowboy persists only the selected set **name** into a new run. A workflow's
named set overrides `default` for this option; omission in that named set
means built-in `false`, not the value of `[config_sets.default]`. Effective
policy resolves from the currently loaded config on every operation. If the
selected set disappears, the runtime falls back to the live `default` set,
then built-in defaults if `default` is also absent. A long-lived TUI must
restart to read config-file edits. Retry counters remain durable across
visits and config edits.

With missing/disabled agent human-input recovery, retry dispatches are durable
and cumulative across the run and each step id; raising a limit adds budget
without resetting accounting. Initial attempts do not count, and retries
consume neither step nor visit budgets. `StepRetrying` events keep visit-local
attempts and a fixed `max_attempts`. Opted-in agent failures bypass this retry
loop and park immediately.

This agent human-input option is additive: existing run records and ordinary
wait callbacks deserialize with the new checkpoint and unsafe marker absent.
Historical failed runs stay failed; enabling the option does not migrate or
reclassify them. The earlier name-only config-set cutover is separate and has
no automatic old-store conversion.

Top-level runner-limit keys from older configs are no longer accepted. Move
them under `[config_sets.default]`.

## Actions

Each `step.run(ctx)` must return exactly one action table created by `action.agent`, `action.command`, `action.status`, `action.wait_for_input`, `action.workflow`, or `action.fail`.

### `action.agent { role, prompt, task, output, pre_input }`

Runs an ACP-compatible coding agent and parses the agent's YAML-frontmatter response into a step output.

```lua
return action.agent {
  role = developer,
  prompt = "Complete declarative prompt retained for debugging.",
  task = {
    key = "implementation_review",
    instructions = "Review the implementation and classify the result.",
    recovery_context = "Plan doc: docs/plans/change.md",
    turn = "Changes needed:\n- Verify retry recovery.\n\nContext:\nThe prior test failed."
  },
  output = {
    status = { "approved", "rejected" },
    fields = {
      summary = "string",
      comments = {
        type = "array",
        description = "One bullet per required change, in priority order."
      }
    },
    required_fields = { "summary" }
  }
}
```

Fields:

- `role` (required): role table or role id string.
- `prompt` (required): full declarative prompt retained for legacy behavior and
  workflow debugging. For structured actions, the executor sends `task.turn`
  instead.
- `task` (optional): structured prompt-delivery contract.
  - `task.key`: non-empty stable key scoped to the run and role.
  - `task.instructions`: static task instructions.
  - `task.recovery_context`: durable current state needed to recover a fresh
    backend session.
  - `task.turn`: minimal current dispatch, normally only
    `Changes needed:` and `Context:` for backward routes.
- `output` (optional): instructions for expected frontmatter output.
  - `output.status`: either one status string or an array of allowed status strings.
  - `output.fields`: table describing expected fields, keyed by field name. Each entry is either:
    - a plain type string (`"array"`, `"boolean"`, `"number"`, or `"string"`), or
    - a table `{ type = "...", description = "..." }`, where `description` is optional prompt guidance shown to the agent about what to return in that field.
  - `output.required_fields`: array of field names that must be present and non-null in the returned output.

- `pre_input` (optional): id of a trusted preflight step in the same workflow
  snapshot. It is evaluated only when this agent action is waiting for human
  input and the selected config set enables `agent_human_input`.

The output spec is prompt guidance. The runtime parses frontmatter and then routes by the returned status; it does not currently enforce a JSON Schema for `fields`.

Backward-routing outputs should provide non-empty `changes_needed` and
`change_context` fields. Revision turns must select those fields from
`ctx.prev.output.fields`; do not include the full plan/specification, previous
body/raw output, unrelated evidence, static role/task instructions, or the
deliverable schema.

The deterministic prompt regression target writes ten UTF-8 files under the
directory named by `COWBOY_PROMPT_CAPTURE_DIR`. Each named file is atomically
replaced so stale captures cannot satisfy comparisons.

#### Opt-in agent human-input recovery

Set `agent_human_input = true` in the selected `[config_sets.<name>]` to park an
incomplete `action.agent` on the original step. Missing/`false` preserves the
old `blocked` status routing, retry/error behavior, ordinary input waits, and
ACP automatic watchdog/`Continue` policy. An allowed `status: blocked` parks
immediately, even when completion-only fields are absent. The first safe ACP
launch/initialize, session, prompt/crash/timeout, or invalid-result failure
parks with a category-specific question **before** a workflow retry or ACP
automatic replacement/continuation. In-flight tool inactivity is bounded.

The persisted `WaitingForInput` has the original step, a fresh `agent-<UUID>`
input id, empty choices, and an `agent_human_input` callback bound to the run,
step visit, previous head, role and original action. No completed agent record
or synthetic success is produced. Provide nonempty supplemental instructions
of at most 4096 bytes with `cowboy provide-input <run-id> <input-id> <context>`
(or use the pending TUI composer). The current id is single-use: wrong, stale
or repeated ids cannot dispatch. The response is scoped to the incomplete
action, not exposed to Lua as `ctx.prev.fields.input`; the existing role
session loads only on its original configured backend and working directory.
An unsupported or failed load parks again without replacing the saved ID; a
first session is created only when none exists. A reused session has three
independent delivery facts: role instructions, the stable task contract, and
the **current frozen action** (including its raw `prompt` or structured
`task.turn`, keyed by step-record id and full action fingerprint). The current
action is marked delivered only after its prompt window seals. A prior role
or matching stable task key does not prove the current raw prompt/new turn was
delivered. With missing current-action evidence Cowboy sends the original raw
task or new turn together with instructions to verify work already performed;
it does not rerun earlier workflow steps. A confirmed current action omits its
raw task/turn on subsequent human answers. A fresh session also receives the
original task. A further inability parks with a new id.

For an incomplete or unsafe agent step, Cowboy removes that step's raw ACP
prompt, response, thought, tool and session progress from the returned report
and persisted workflow event log. The original action stays private durable
state; only the safe question and input id are public wait metadata.

Failed ACP process groups are stopped and direct children reaped or proven
absent before a human wait is published. Unverified cleanup or ACP
policy/provenance rejection is terminal: `resume`, `step`, restart and manual
`resolve` cannot redispatch or synthesize success. A process crash or
cancellation during a validated continuation leaves a durable checkpoint but
no cross-process proof that the old ACP writer stopped. `resume` marks it
terminal unsafe rather than restoring an actionable wait. A new human answer
cannot override that boundary. `step`/`resume` leave an ordinary outstanding
agent wait unchanged; `action.wait_for_input` keeps its previous re-prompt
behavior.

#### Native writer shutdown evidence

New opted-in runs started with an explicit workflow (including nested child
runs) register the exact agent action and backend owner generation before
spawning ACP. Cowboy closes and verifies **every** acquired owned process
group/Job at a completed, stepwise or waiting boundary, then records private
shutdown receipts. `cowboy ownership <run-id>` is a CLI-only, read-only JSON
projection: `state: "verified"` requires all generations and linked child,
ancestor and restart-source runs to have stopped. `status` and `head` remain
the current workflow state; `attempt_count` includes failed OS launches and
`process_count` counts distinct actually spawned and verified process scopes.
The command never returns the owner token, process id, backend session or
action body. Completed is not itself shutdown proof; waiting may be quiescent
without being complete. Missing/false policy and historical runs have no
native shutdown proof. An unsealed writer or interrupted continuation remains
unsafe and cannot be authorized by another reply, `resolve` or restart.

ACP-backed workflow selection occurs before a run id exists, so its runs are
not eligible for this candidate; use an explicit workflow id when a local
owned-writer certificate is required. Enrolled runs do not call the optional
ACP presentation-only request-topic generator. The process-group/Job proof
does not cover escaped children or hard Cowboy `SIGKILL` and is not permission
to release a checkout; a caller must inspect its own current reservation and
artifacts independently. See [configuration and CLI](../README.md#configuration).

#### Optional current-context preflight

When a human answer depends on changing external state, `pre_input` lets the
workflow verify that state **before** Cowboy accepts the answer or starts the
original agent again. For example, the workflow may validate a revision and
provide a fresh read-only snapshot instead of forwarding an obsolete answer:

```lua
local developer = role("developer", "Implement the user's request")

local guard = step("preflight")
guard.run = function(ctx)
  -- Fixed workflow-owned command and arguments; ctx.input is not available here.
  return action.command {
    program = "cat", args = { "authority.json" }, timeout_ms = 1000
  }
end

guard.verify = function(ctx)
  local current = cowboy.json.decode(ctx.authority.fields.stdout)
  if current.revision ~= ctx.input then
    return action.status { status = "reject" }
  end

  return action.status {
    status = "ready", fields = { context = current.context }
  }
end

local implement = step("implement")
implement.run = function(ctx)
  return action.agent {
    role = developer, prompt = "Implement the requested change",
    pre_input = "preflight"
  }
end

return workflow("implement", implement)
```

The preflight step does not advance the workflow or persist a completed step
record. Cowboy evaluates `guard.run(ctx)` from the run's persisted workflow
snapshot under its same-run lock. The context contains the original request,
workflow and step metadata, `incomplete_step`, `user_inputs`, `prev` and
`steps_executed`, but **not** the current answer. It must return
`action.command` with an explicit timeout of 1–5000 ms; Cowboy executes it
directly from the runtime cwd using the configured allowed environment. Once
the command succeeds without timeout, truncated/incomplete streams or an
oversized stdout (>4096 bytes), `guard.verify(ctx)` runs from the same
snapshot with `ctx.input` (the bounded answer) and `ctx.authority` (the
normalized command `status`, `fields` and `body`). Only
`action.status { status = "ready", fields = { context = "..." } }` accepts the
answer. The `context` must be a string without control characters; Cowboy
appends it as a labeled verified-context block to the original answer, with
the combined supplement limited to 4096 bytes. A non-ready status, invalid
output, unavailable command or verifier error leaves the **same** input id
and wait in place, without loading or prompting ACP. Reply with the current
input id after fixing the source of the failed verification.

Use only trusted workflow code and commands that are genuinely safe to rerun
and read-only. `action.command` is **not** a sandbox; Cowboy cannot enforce
read-only effects, isolate descendants after a hard kill of Cowboy, or make
the authority read atomic with later agent work. The current answer cannot
choose the program, arguments, role or step through the native API; the
workflow author must likewise treat `ctx.input` and command output as
untrusted data when writing the verifier. Without `pre_input`, human-input
recovery is unchanged; missing/disabled `agent_human_input` never executes
the guard.

### `action.command { program, args, status_map, timeout_ms }`

Runs one command-line program directly, without a shell. Cowboy passes `program` and `args` to the OS process spawner as an explicit argument vector; it does not interpret quotes, variables, pipes, redirects, globs, or command substitution.

```lua
return action.command {
  program = "git",
  args = { "status", "--short" },
  status_map = {
    ["0"] = "clean",
    ["1"] = "dirty",
    ["_"] = "failed",
  },
  timeout_ms = 5000
}
```

Fields:

- `program` (required): non-empty executable name or path.
- `args` (optional): array of string arguments; defaults to `{}`.
- `status_map` (optional): table mapping an exit code (integer or numeric-string key) to the output status used for that code. The catch-all `"_"` key covers any exit code without an exact match, plus spawn errors and timeouts, which have no exit code. Defaults to `{ ["0"] = "success", ["_"] = "failed" }`. A `status_map` missing a `"_"` entry falls back to `"failed"` for unmatched codes.
- `timeout_ms` (optional): positive integer wall-clock deadline covering spawn, parent exit, and full stdout/stderr drain. If either pipe remains open past the deadline, `success = false` and `timed_out = true` even if the parent exited with code 0. Captured prefixes remain available, but incomplete streams are not complete command output. The status still follows `status_map`: a custom `"_" = "success"` can route a timeout to `success`, so downstream steps must check `ctx.prev.fields.success` and `ctx.prev.fields.timed_out` before trusting its output. On Unix Cowboy starts a separate process group and kills it on timeout, capture failure, or cancellation, including descendants that inherited pipes. On Windows Cowboy kills the direct child on timeout or cancellation but cannot guarantee descendant termination.

The command runs from `RuntimeConfig.cwd`. Workflows cannot override cwd,
environment, or stdin for this action; stdin is closed. Cowboy clears the child
environment, then copies only names from top-level `allowed_env` that are
present in Cowboy's process when the command starts. Omitting the key preserves
the compatibility default (`PATH`, `PATHEXT`, `SystemRoot`, `USERPROFILE`,
`LOCALAPPDATA`, `APPDATA`, `TEMP`, `TMP`, and `HOME`); explicit `allowed_env = []`
forwards nothing. Cowboy does not synthesize missing values, and every other
ambient variable remains removed. There is no workflow-authored environment
override.

ACP backend children use the same global list plus the selected
`[[agents]].allowed_env` list. Per-agent names are additive and cannot remove a
global name. Selection, request-topic generation, role turns, retries,
reconnect/recovery, and workflow improvement all use this policy. Multiple
roles selecting one named agent share that agent's policy; configure separate
agent entries for distinct policies. For example:

```toml
allowed_env = ["PATH", "COWBOY_EXAMPLE_GLOBAL_CA"]

[[agents]]
name = "planner"
command = "copilot"
args = ["--acp"]
allowed_env = ["COWBOY_EXAMPLE_PLANNER_TOKEN"]

[agents.model]
id = "planner-model"
provider = "configured-provider"
```

Security warning: `action.command` is not a sandbox. Use it only for trusted
workflows and trusted commands. The child process still runs as the Cowboy OS
user from `RuntimeConfig.cwd`; it can read or write files and use any network
resources available to the Cowboy process, even though Cowboy does not invoke
a shell and sanitizes environment/stdin. Explicitly allow-list required
authentication and tool variables; each allowed name exposes its current value
to the selected child.

The completed `StepOutput.fields` contains:

- `program`, `args`
- `success`
- `exit_code` (`null` for spawn errors or signal-only exits)
- `stdout`, `stderr`
- `timed_out`
- `stdout_truncated`, `stderr_truncated` (capture limit exceeded)
- `stdout_incomplete`, `stderr_incomplete` (EOF not observed; for example, timeout)
- `spawn_error` when the process could not be started

Captured stdout and stderr are bounded. The truncation flags report capture-limit overflow; the incomplete flags report a stream stopped before EOF. `StepOutput.body` is stdout on success; on failure it is stderr when present, otherwise stdout, otherwise the spawn error or timeout text.

Command `program`, `args`, captured stdout, and captured stderr are persisted in the step output so later workflow steps can read them through `ctx.prev.fields`. `program` is persisted too and may expose an absolute or private path; prefer bare executable names or non-sensitive paths when persisted run records may be shared. Do not put secrets, tokens, personal data, private local paths, or proprietary content in command metadata or command output.

Route both success and failure statuses when the workflow should continue after command execution:

```lua
run_tests:on("success", summarize)
run_tests:on("failed", diagnose)
```

### `cowboy.json.decode(text)`

The public `cowboy` Lua module groups workflow utilities under submodules. Decode
JSON command output with `cowboy.json.decode(ctx.prev.fields.stdout)`; the result
contains Lua strings, booleans, numbers, arrays (1-based tables), and objects
(string-keyed tables). JSON `null` is the distinct `cowboy.json.null` sentinel,
including inside arrays and objects. Passing that sentinel in `action.status`
fields persists JSON `null`; Lua `nil` removes a table entry. Only decode
untruncated successful output. Invalid JSON, trailing content, inputs over 64
KiB, or nesting over 64 levels raise a Lua error. This API does not expose
`json` as a global or add filesystem/process access to the Lua sandbox.

Decoded empty objects remain objects (`{}`), distinct from empty arrays (`[]`),
including when nested in status fields and passed to subsequent steps. JSON
`null` in `ctx.prev.fields` remains `cowboy.json.null` across persisted steps;
ordinary request/context nulls retain their Lua `nil` behavior. Ordinary
workflow-authored empty Lua tables retain their existing array interpretation.
JSON integers outside Lua's signed 64-bit range are rejected instead of rounded;
floating-point numbers with magnitude above 2^53 are rejected.

```lua
local fetch = step("fetch")
fetch.run = function(ctx)
  return action.command { program = "gh", args = { "api", "repos/OWNER/REPO/issues/123" } }
end

local decode = step("decode")
decode.run = function(ctx)
  local output = ctx.prev.fields
  if output.stdout_truncated then
    return action.fail { reason = "JSON output truncated" }
  end

  local issue = cowboy.json.decode(output.stdout)
  return action.status {
    status = "success",
    fields = { number = issue.number, title = issue.title, milestone = issue.milestone }
  }
end

local consume = step("consume")
consume.run = function(ctx)
  return action.status { status = "success", fields = { issue = ctx.prev.fields } }
end

fetch:on("success", decode)
decode:on("success", consume)
return workflow("issue-example", fetch)
```

### `action.status { status, fields, body }`

Completes the step immediately without calling an agent.

```lua
return action.status {
  status = "success",
  fields = {
    summary = "Nothing to do",
    files = {}
  },
  body = "The request was already satisfied."
}
```

Fields:

- `status` (required): routing status string.
- `fields` (optional): JSON-compatible Lua value exposed as `ctx.prev.fields`; omitted fields become `null`.
- `body` (optional): Markdown/prose exposed as `ctx.prev.body`; omitted body becomes `""`.

Use this for deterministic branching, summaries, adapters around previous output, and final terminal records.

### `action.wait_for_input { id, message, choices, status, fields }`

Pauses the run until trusted external input arrives. When input is provided, the runtime completes the wait-for-input action into a normal step record. The following step receives `ctx.prev.action == "wait_for_input"`, `ctx.prev.status == "provided"` unless overridden, and `ctx.prev.fields.input` plus any fields supplied on the wait action.

Internally, the waiting run stores prompt metadata plus a durable `ResumeCallback` descriptor. When trusted external input arrives, the runtime validates the input id and finite choices, dispatches the registered callback by kind, and applies the resulting wait-for-input `StepRecord` through normal status-based routing. The supplied value is a hint, not proof that an external approval or condition is satisfied; route to a follow-up step that rechecks the authoritative condition.

```lua
local ask_scope = step("ask_scope")
ask_scope.run = function(ctx)
  return action.wait_for_input {
    id = "scope",
    message = "Should Cowboy update docs only or code and docs?",
    choices = { docs = "Update only the docs", ["code-and-docs"] = "Update code and docs" },
    fields = { source = "triage" }
  }
end

local route_scope = step("route_scope")
route_scope.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.status {
    status = tostring(fields.input),
    fields = { scope = fields.input, source = fields.source }
  }
end

ask_scope:on("provided", route_scope)
```

Fields:

- `id` (required): stable input id used for validation and UI/event display.
- `message` (required): text shown to the user or trusted external system.
- `choices` (optional): finite allowed input values, given as a table mapping each accepted input key to a human-readable description (e.g. `{ yes = "Approve the release", no = "Reject the release" }`). If present, values outside the key set are rejected; each accepted choice carries both its key and description to the UI/event display.
- `status` (optional): output status for the completed wait-for-input record; defaults to `"provided"`.
- `fields` (optional): structured fields copied into the completed wait-for-input output before `fields.input` is merged.

Always route the wait-for-input step's `provided` status to a follow-up step that reads `ctx.prev.fields.input`; otherwise the workflow will keep pausing on every visit.

### `action.fail { reason }`

Marks the run failed immediately.

```lua
return action.fail { reason = "Cannot continue without a repository checkout." }
```

Fields:

- `reason` (required): human-readable failure reason.

`fail` does not create a step output record and does not use transitions.


### `action.workflow { workflow, request }`

Invokes another catalog workflow as a durable child run and, when the child
finishes, completes the calling step with the child's terminal output.

```lua
return action.workflow {
  workflow = "review/security",
  request = "Review the current implementation",
}
```

Fields:

- `workflow` (required): the catalog workflow id shown by `/workflows`, exactly
  as used with `cowboy run --workflow` — not necessarily the Lua-declared
  display name. Must be a non-blank string.
- `request` (required): the initial request handed to the child workflow,
  preserved byte-for-byte (including leading/trailing whitespace and newlines).

Behavior:

- The child is a normal, durable `WorkflowRun`. It uses the same catalog, store,
  run locks, agent factory, config-set resolution, `WorkflowRunner`, event bus,
  retry policy, and cancellation path as a top-level run, and runs until it
  completes, fails, is cancelled, or asks for input.
- On completion, the calling step's `StepOutput` copies the child terminal
  step's `status`, `fields`, `body`, and `raw` unchanged, so the parent routes
  directly on the status the child returned and its next step reads the child
  result through `ctx.prev`. The target workflow id, exact request, and child
  run id are recorded under the parent step's `StepInput.context`, with
  `StepDetail.backend = "workflow"` and `StepDetail.session_id = <child run id>`.
- A child `RunStatus::Failed` becomes a completed parent output with status
  `"failed"` (carrying the child run id and reason). A child cancellation becomes
  status `"cancelled"`. Route these like any other status.
- If the child waits for input, its prompt is mirrored on the parent's own
  `WaitingForInput` state; providing the parent input (`cowboy provide-input <parent-run-id> <input-id> <input>`) forwards the value and continues the child. If the child asks again, the parent
  prompt refreshes; when the child becomes terminal, the parent workflow action
  completes and the parent continues automatically.
- Child creation is idempotent across retries, resume, and process interruption:
  the child run id is a stable UUID-v5 derived from the parent run id, the
  calling step id, and the parent's previous head. Retrying or resuming reuses
  the same child rather than creating a new one.
- Direct (`A -> A`) and indirect (`A -> B -> A`) workflow-call cycles are
  rejected with a clear error before any child lock is acquired.
- Parent and child keep separate event logs; the active parent reports child
  lifecycle progress (`child workflow <id> started/waiting for input/resumed/
  finished`).


## Transitions

Transitions route completed `agent`, `command`, `status`, `workflow`, or provided `wait_for_input` step outputs by status.

```lua
implement:on("success", finish)
implement:on("failed", failed)
implement:on("needs_fix", fix)
```

Rules:

- Use `step:on(status, target_step)` after both steps have been declared.
- `status` must be a non-empty string.
- `target_step` can be a step table or step id string.
- Validation rejects unknown target steps.
- If a completed step declares no transitions at all, it is terminal: the workflow completes with whatever status the step returned (this is how a child workflow completes with a domain status such as the one its caller routes on).
- If a completed step declares transitions and returns `success` without an explicit `success` transition, the workflow completes.
- If a completed step declares transitions and returns any other status without a matching transition, the run errors with an unknown runtime transition.
- `wait_for_input` and `fail` are run-state changes, not completed step outputs, so transition tables are not consulted when they initially block or fail the run. The completed wait-for-input record produced after an input is routed by its output status.

A common pattern is to normalize agent outputs into terminal status steps:

```lua
implement:on("success", finish)
implement:on("failed", failed)
implement:on("needs_fix", needs_fix)
```

## Scoped `require`

Cowboy replaces Lua's normal `require` with a workflow-root-scoped loader.

```lua
-- main.lua
local roles = require("roles.lua")
local steps = require("steps/implementation.lua")

local implement = steps.implement(roles.developer)
return workflow("modular", implement)
```

```lua
-- roles.lua
return {
  developer = role("developer", "Implement the requested change.")
}
```

Rules:

- Paths are relative to the workflow root, not to the importing file.
- Absolute paths, empty paths, and `..` parent-directory segments are rejected.
- `./` segments are normalized away.
- Required files are evaluated and may return any Lua value.
- Loaded sources are captured in the workflow source snapshot so resumed runs use the same source bundle.
- The sandbox only exposes allowlisted pure helpers (`assert`, `error`, `ipairs`, `next`, `pairs`, `select`, `tonumber`, `tostring`, `type`, selected `string.*`, and selected `table.*`) plus Cowboy's workflow API.

## Agent frontmatter output expectations

Agent responses must begin with YAML frontmatter. A Markdown body may follow the
closing delimiter, but should normally be empty.

```markdown
---
status: success
summary: Implemented workflow docs
files:
  - docs/workflow-authoring.md
---
```

Parsing rules:

- The first non-whitespace characters must be `---` followed by a newline.
- The frontmatter must be a YAML mapping.
- `status` must be a string. `$status` is accepted as a legacy alias if `status` is absent.
- Every non-status frontmatter key becomes part of `ctx.prev.fields`.
- The body after the closing `---` is trimmed and stored as `ctx.prev.body`.
- The full raw response is stored as `ctx.prev.raw`.

When you specify `action.agent.output`, Cowboy appends delivery instructions like this to the agent prompt:

```markdown
## Deliverable Format

Your response MUST begin with valid YAML frontmatter. A Markdown body may follow
the closing delimiter, but should normally be empty.

Allowed status values: success, failed, needs_fix

Frontmatter fields:
- status: routing status string
- summary: string
- files: array

Return only the workflow result required by this contract. Include `status`,
required fields, and optional fields needed by the next step. Omit undeclared,
redundant, empty, and null fields. Keep values short without dropping exact
paths, commands, evidence, errors, or other correctness-critical details. Use a
body only when essential information cannot fit the declared fields, and do not
repeat the task, reasoning, logs, command output, or file contents there.
```

Design statuses as workflow-routing values, not prose. Put necessary
human-readable detail in declared fields. The trimmed body remains available as
`ctx.prev.body` when a body is genuinely needed.

## Complete examples

### Minimal deterministic workflow

```lua
local start = step("start")
start.run = function(ctx)
  return action.status {
    status = "success",
    fields = {
      summary = "Completed without agent work",
      request = ctx.request
    },
    body = "No additional work was required."
  }
end

return workflow("minimal", start, "Immediate success workflow")
```

Because `start` returns `success` and has no `success` transition, the run completes.

### Agent implementation workflow

```lua
local developer = role("developer", {
  instructions = [[You are a careful Rust engineer. Keep changes focused and verify behavior.]]
})

local implement = step("implement", { role = developer })
implement.run = function(ctx)
  return action.agent {
    role = developer,
    prompt = [[Implement the user's request in the current project.

Request:
]] .. tostring(ctx.request) .. [[

Return status success when complete, failed when blocked, or needs_fix when follow-up work is required.]],
    output = {
      status = { "success", "failed", "needs_fix" },
      fields = {
        summary = "string",
        files = "array"
      }
    }
  }
end

local finish = step("finish")
finish.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.status {
    status = "success",
    fields = {
      summary = fields.summary or "Workflow completed",
      files = fields.files or {}
    },
    body = ctx.prev and ctx.prev.body or ""
  }
end

local failed = step("failed")
failed.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.fail {
    reason = fields.summary or "Workflow failed"
  }
end

local needs_fix = step("needs_fix")
needs_fix.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.fail {
    reason = fields.summary or "Workflow needs follow-up fixes"
  }
end

implement:on("success", finish)
implement:on("failed", failed)
implement:on("needs_fix", needs_fix)

return workflow("developer-flow", implement, {
  description = "Single-agent implementation workflow with explicit terminal run states"
})
```

### Wait-for-input branching workflow

```lua
local triage = step("triage")
triage.run = function(ctx)
  return action.wait_for_input {
    id = "intent",
    message = "What kind of work is this?",
    choices = { feature = "New feature work", bug = "Bug fix", docs = "Documentation only" }
  }
end

local route = step("route")
route.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  local value = tostring(fields.input)
  return action.status {
    status = value,
    fields = { intent = value }
  }
end

local feature = step("feature")
feature.run = function(ctx)
  return action.status { status = "success", fields = { lane = "feature" } }
end

local bug = step("bug")
bug.run = function(ctx)
  return action.status { status = "success", fields = { lane = "bug" } }
end

local docs = step("docs")
docs.run = function(ctx)
  return action.status { status = "success", fields = { lane = "docs" } }
end

triage:on("provided", route)
route:on("feature", feature)
route:on("bug", bug)
route:on("docs", docs)

return workflow("triage", triage, "Ask the user once, then route by input")
```

### Modular workflow with scoped `require`

```lua
-- roles.lua
return {
  developer = role("developer", "Implement focused code changes."),
  reviewer = role("reviewer", "Review for correctness and maintainability.")
}
```

```lua
-- steps/review.lua
return function(reviewer)
  local review = step("review", { role = reviewer })
  review.run = function(ctx)
    local summary = ctx.prev and ctx.prev.fields and ctx.prev.fields.summary or ""
    return action.agent {
      role = reviewer,
      prompt = "Review the previous result. Summary: " .. tostring(summary),
      output = {
        status = { "approved", "rejected" },
        fields = {
          summary = "string",
          comments = "array"
        }
      }
    }
  end
  return review
end
```

```lua
-- main.lua
local roles = require("roles.lua")
local make_review = require("steps/review.lua")

local implement = step("implement", { role = roles.developer })
implement.run = function(ctx)
  return action.agent {
    role = roles.developer,
    prompt = "Implement: " .. tostring(ctx.request),
    output = {
      status = { "success", "failed" },
      fields = {
        summary = "string",
        files = "array"
      }
    }
  }
end

local review = make_review(roles.reviewer)

local failed = step("failed")
failed.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.fail { reason = fields.summary or "Implementation failed" }
end

implement:on("success", review)
implement:on("failed", failed)
review:on("approved", step("finish", {
  run = function(ctx)
    return action.status {
      status = "success",
      fields = (ctx.prev and ctx.prev.fields) or {}
    }
  end
}))
review:on("rejected", implement)

return workflow("modular-review", implement, "Implement, review, and loop on rejection")
```

For larger workflows, prefer declaring named steps before transitions instead of creating inline target steps; named locals make validation failures and charts easier to read.
