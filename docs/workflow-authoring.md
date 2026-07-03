# Workflow authoring

Cowboy workflows are Lua files that compile into a durable workflow graph. A run starts at the workflow head step, each step returns one `action.*` value, and the action output status routes to the next step.

```text
.lua source
  -> role(...) declarations
  -> step(...) declarations with run(ctx)
  -> workflow(name, head)
  -> status transitions: step:on(status, next_step)
```

The Lua VM is sandboxed and recreated for each compile or step execution. Persist run state through action outputs, `ctx.prev`, and `ctx.resume`; do not depend on mutable Lua globals surviving between steps.

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
| `ctx.resume` | Answers from `action.ask_user`, keyed by prompt id. |
| `ctx.prev` | Latest completed step output, or `nil` on the first step. |
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
```

Rules:

- `name` must be non-empty.
- `head` must be a declared step table.
- `description` is optional and used by workflow selection/catalog display.
- All declared steps are compiled; validation rejects an unknown head, unknown roles, unknown transition targets, and empty transition statuses.

## Actions

Each `step.run(ctx)` must return exactly one action table created by `action.agent`, `action.status`, `action.ask_user`, `action.fail`, or `action.suspend`.

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

### `action.ask_user { id, message, choices }`

Pauses the run and asks the user for input. The same step is re-evaluated after the answer is stored in `ctx.resume[id]`.

```lua
local clarify = step("clarify")
clarify.run = function(ctx)
  local answer = ctx.resume and ctx.resume.scope
  if answer then
    return action.status {
      status = "answered",
      fields = { scope = answer }
    }
  end

  return action.ask_user {
    id = "scope",
    message = "Should Cowboy update docs only or code and docs?",
    choices = { "docs", "code-and-docs" }
  }
end
```

Fields:

- `id` (required): stable prompt id; this becomes the key in `ctx.resume`.
- `message` (required): text shown to the user.
- `choices` (optional): finite allowed answers. If present, answers outside the list are rejected.

Always check `ctx.resume[id]` before asking again; otherwise the workflow will keep pausing on every resume.

### `action.fail { reason }`

Marks the run failed immediately.

```lua
return action.fail { reason = "Cannot continue without a repository checkout." }
```

Fields:

- `reason` (required): human-readable failure reason.

`fail` does not create a step output record and does not use transitions.

### `action.suspend { reason }`

Stops the run without marking it failed.

```lua
return action.suspend { reason = "Waiting for an external deployment to finish." }
```

Fields:

- `reason` (required): human-readable suspension reason.

`suspend` records the current step and reason. It does not create a step output record and does not use transitions.

## Transitions

Transitions route completed `agent` or `status` step outputs by status.

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
- `ask_user`, `fail`, and `suspend` are run-state changes, not completed step outputs, so transition tables are not consulted for those actions.

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
  return action.suspend {
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
  local answer = ctx.resume and ctx.resume.intent
  if not answer then
    return action.ask_user {
      id = "intent",
      message = "What kind of work is this?",
      choices = { "feature", "bug", "docs" }
    }
  end

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

triage:on("feature", feature)
triage:on("bug", bug)
triage:on("docs", docs)

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
