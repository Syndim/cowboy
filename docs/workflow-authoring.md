# Workflow authoring

Cowboy workflows are Lua files that compile into a durable workflow graph. A run starts at the workflow head step, each step returns one `action.*` value, and the action output status routes to the next step.

```text
.lua source
  -> role(...) declarations
  -> step(...) declarations with run(ctx)
  -> workflow(name, head)
  -> status transitions: step:on(status, next_step)
```

The Lua VM is sandboxed and recreated for each compile or step execution. Persist run state through action outputs and `ctx.prev`; do not depend on mutable Lua globals surviving between steps. `ctx.resume` is inactive legacy state and is not the ask-user answer path.

## File placement

Custom workflows are `.lua` files under a configured `workflow_dirs` entry. By default, Cowboy scans `${XDG_CONFIG_HOME:-~/.config}/cowboy/workflows`, so a user workflow can live at `~/.config/cowboy/workflows/my-workflow.lua`. For project-local workflows, add a project directory such as `.cowboy/workflows` to `workflow_dirs` in `${XDG_CONFIG_HOME:-~/.config}/cowboy/config.toml`, then put `.lua` files under that directory:

```toml
workflow_dirs = [".cowboy/workflows", "~/.config/cowboy/workflows"]
```

Cowboy scans configured workflow directories recursively and uses each relative `.lua` path as the workflow id without the `.lua` suffix; for example, `.cowboy/workflows/review/security.lua` is cataloged as `review/security`. The built-in default workflow is always available, so custom workflows only need to be added when the default developer flow is not enough.

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
    prompt = "Implement: " .. tostring(ctx.request),
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
| `ctx.run_id` | Stable run id. |
| `ctx.workflow.name` | Current workflow name. |
| `ctx.workflow.head` | Workflow head step id. |
| `ctx.current_step` | Current step id. |
| `ctx.step.id` | Current step id. |
| `ctx.step.role` | Current step's configured role id, or `nil`. |
| `ctx.step.properties` | Non-reserved fields from the step config. |
| `ctx.resume` | Inactive legacy state retained for old serialized runs; do not use for new workflows. |
| `ctx.prev` | Latest completed step output, including completed ask-user answers, or `nil` on the first step. |
| `ctx.steps_executed` | Number of already-executed steps in the run. |

`ctx.prev` has this shape when a previous step completed:

```lua
{
  record_id = "run-...-2",
  step = "implement",
  action = "agent",       -- or "status"
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

[config_sets.careful]
# Omitted fields independently inherit 100, 20, 200, and 2.
max_retries_per_run = 20
max_retries_per_step = 4
```

The built-in `default` set always exists. Each omitted field inherits its shown
built-in value. Retry limits may be `0`; step and visit limits must be greater
than zero. Blank names and unknown fields are rejected. An unknown workflow
selection fails before a new run is persisted and reports the available sets.

Cowboy snapshots the selected set name and all four effective limits into a
new run. Later resume, step, answer, resolve, and resolution-option operations
use that snapshot even if the host config changes or removes the set. Retry
dispatches are durable and cumulative across the run and across every visit to
the same step id; initial attempts do not count, and retries consume neither
step nor visit budgets. `StepRetrying` events keep visit-local attempts and a
fixed `max_attempts` computed from both remaining retry ceilings.

Top-level runner-limit keys from older configs are no longer accepted. Move
them under `[config_sets.default]`.

## Actions

Each `step.run(ctx)` must return exactly one action table created by `action.agent`, `action.command`, `action.status`, `action.ask_user`, or `action.fail`.

### `action.agent { role, prompt, output }`

Runs an ACP-compatible coding agent and parses the agent's YAML-frontmatter response into a step output.

```lua
return action.agent {
  role = developer,
  prompt = "Review the implementation and classify the result.",
  output = {
    status = { "approved", "rejected" },
    fields = {
      summary = "string",
      comments = "array"
    }
  }
}
```

Fields:

- `role` (required): role table or role id string.
- `prompt` (required): full task prompt sent to the agent.
- `output` (optional): instructions for expected frontmatter output.
  - `output.status`: either one status string or an array of allowed status strings.
  - `output.fields`: table describing expected fields; string values become human-readable field descriptions in the prompt.

The output spec is prompt guidance. The runtime parses frontmatter and then routes by the returned status; it does not currently enforce a JSON Schema for `fields`.

### `action.command { program, args, success_status, failure_status, timeout_ms }`

Runs one command-line program directly, without a shell. Cowboy passes `program` and `args` to the OS process spawner as an explicit argument vector; it does not interpret quotes, variables, pipes, redirects, globs, or command substitution.

```lua
return action.command {
  program = "git",
  args = { "status", "--short" },
  success_status = "clean",
  failure_status = "failed",
  timeout_ms = 5000
}
```

Fields:

- `program` (required): non-empty executable name or path.
- `args` (optional): array of string arguments; defaults to `{}`.
- `success_status` (optional): output status for exit code `0`; defaults to `"success"`.
- `failure_status` (optional): output status for non-zero exit, spawn error, or timeout; defaults to `"failed"`.
- `timeout_ms` (optional): positive integer wall-clock timeout. On timeout, Cowboy kills the child and completes the step with `failure_status`.

The command runs from `RuntimeConfig.cwd`. Workflows cannot override cwd, environment, or stdin for this action; stdin is closed. Cowboy clears the child environment and passes through only `PATH` when Cowboy itself has `PATH`, so commands must not rely on inherited credentials or ambient environment variables.

Security warning: `action.command` is not a sandbox. Use it only for trusted workflows and trusted commands. The child process still runs as the Cowboy OS user from `RuntimeConfig.cwd`; it can read or write files and use any network resources available to the Cowboy process, even though Cowboy does not invoke a shell and sanitizes environment/stdin.

The completed `StepOutput.fields` contains:

- `program`, `args`
- `success`
- `exit_code` (`null` for spawn errors or signal-only exits)
- `stdout`, `stderr`
- `timed_out`
- `stdout_truncated`, `stderr_truncated`
- `spawn_error` when the process could not be started

Captured stdout and stderr are bounded. The truncation flags tell following steps whether a captured stream was cut. `StepOutput.body` is stdout on success; on failure it is stderr when present, otherwise stdout, otherwise the spawn error or timeout text.

Command `program`, `args`, captured stdout, and captured stderr are persisted in the step output so later workflow steps can read them through `ctx.prev.fields`. `program` is persisted too and may expose an absolute or private path; prefer bare executable names or non-sensitive paths when persisted run records may be shared. Do not put secrets, tokens, personal data, private local paths, or proprietary content in command metadata or command output.

Route both success and failure statuses when the workflow should continue after command execution:

```lua
run_tests:on("success", summarize)
run_tests:on("failed", diagnose)
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

### `action.ask_user { id, message, choices, status, fields }`

Pauses the run and asks the user for input. When answered, the runtime completes the ask-user action into a normal step record. The following step receives `ctx.prev.action == "ask_user"`, `ctx.prev.status == "answered"` unless overridden, and `ctx.prev.fields.answer` plus any fields supplied on the ask action.

Internally, the waiting run stores prompt metadata plus a durable `ResumeCallback` descriptor. When an answer arrives, the runtime validates the prompt id and choices, dispatches the registered callback by kind, and applies the resulting ask-user `StepRecord` through normal status-based routing.

```lua
local ask_scope = step("ask_scope")
ask_scope.run = function(ctx)
  return action.ask_user {
    id = "scope",
    message = "Should Cowboy update docs only or code and docs?",
    choices = { "docs", "code-and-docs" },
    fields = { source = "triage" }
  }
end

local route_scope = step("route_scope")
route_scope.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  return action.status {
    status = tostring(fields.answer),
    fields = { scope = fields.answer, source = fields.source }
  }
end

ask_scope:on("answered", route_scope)
```

Fields:

- `id` (required): stable prompt id used for validation and UI/event display.
- `message` (required): text shown to the user.
- `choices` (optional): finite allowed answers. If present, answers outside the list are rejected.
- `status` (optional): output status for the completed ask-user record; defaults to `"answered"`.
- `fields` (optional): structured fields copied into the completed ask-user output before `fields.answer` is merged.

Always route the ask-user step's `answered` status to a follow-up step that reads `ctx.prev.fields.answer`; otherwise the workflow will keep pausing on every visit.

### `action.fail { reason }`

Marks the run failed immediately.

```lua
return action.fail { reason = "Cannot continue without a repository checkout." }
```

Fields:

- `reason` (required): human-readable failure reason.

`fail` does not create a step output record and does not use transitions.


## Transitions

Transitions route completed `agent`, `command`, `status`, or answered `ask_user` step outputs by status.

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
- If a completed step returns status `success` and there is no explicit `success` transition, the workflow completes.
- If a completed step returns any other status without a matching transition, the run errors with an unknown runtime transition.
- `ask_user` and `fail` are run-state changes, not completed step outputs, so transition tables are not consulted when they initially block or fail the run. The completed ask-user record produced after an answer is routed by its output status.

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

Agent responses must begin with YAML frontmatter followed by a Markdown body.

```markdown
---
status: success
summary: Implemented workflow docs
files:
  - docs/workflow-authoring.md
---

Detailed Markdown body visible to later steps as `ctx.prev.body`.
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

Your response MUST begin with YAML frontmatter followed by Markdown body.

Allowed status values: success, failed, needs_fix

Frontmatter fields:
- status: routing status string
- summary: string
- files: array
```

Design statuses as workflow-routing values, not prose. Put human-readable detail in other fields and the Markdown body.

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

### Ask-user branching workflow

```lua
local triage = step("triage")
triage.run = function(ctx)
  return action.ask_user {
    id = "intent",
    message = "What kind of work is this?",
    choices = { "feature", "bug", "docs" }
  }
end

local route = step("route")
route.run = function(ctx)
  local fields = (ctx.prev and ctx.prev.fields) or {}
  local answer = tostring(fields.answer)
  return action.status {
    status = answer,
    fields = { intent = answer }
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

triage:on("answered", route)
route:on("feature", feature)
route:on("bug", bug)
route:on("docs", docs)

return workflow("triage", triage, "Ask the user once, then route by answer")
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
