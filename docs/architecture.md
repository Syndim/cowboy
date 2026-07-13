# Cowboy architecture

Cowboy is a workflow-first terminal agent orchestrator. It has one binary with two interfaces:

- `cowboy` with no subcommand launches the interactive TUI.
- `cowboy <subcommand>` runs a non-interactive CLI command against the same workflow runtime and state.

The current shape keeps command parsing, UI/CLI code, and workflow runtime logic separate:

```text
process argv / slash composer input
  -> cowboy-command-parser typed commands + metadata
  -> cowboy CLI/TUI app
       -> cowboy-workflow-engine
            -> catalog loading / workflow selection / summarization
            -> WorkflowRun state orchestration
            -> Lua step action provider
            -> ACP-backed agent action executor / input router
            -> StepRecord + RunHead persistence
            -> workflow events + event log
       -> TUI renders workflow events and accepts plain text/prompt answers
```

## Workspace crates

| Crate | Purpose |
| --- | --- |
| `cowboy` (`crates/tui/app`) | Config loading, logging setup, runtime dispatch, and ratatui rendering only. Uses `cowboy-command-parser` for command grammar. |
| `cowboy-command-parser` (`crates/tui/command-parser`) | Runtime/UI-independent clap-backed parsing for product CLI commands and interactive slash commands, plus metadata/suggestions. |
| `cowboy-workflow-engine` | Product runtime: starts/resumes/answers/improves workflow runs, emits events, wires ACP agent execution. |
| `cowboy-workflow-catalog` | Built-in workflow source plus project/user `.lua` workflow catalog loading and update application. |
| `cowboy-workflow-core` | Serializable workflow domain model, graph validation, `execute_step`, runner traits. |
| `cowboy-workflow-lua` | Sandboxed Lua workflow loader and one-step runtime. |
| `cowboy-workflow-store` | `redb`-backed `RunStore` for runs, run heads, immutable objects, turns, and role sessions. |
| `cowboy-workflow-agent` | Agent action executor, YAML-frontmatter output parsing, role-session reuse. |
| `cowboy-agent-client` | Provider-neutral `Client` trait and normalized agent events/types. |
| `cowboy-agent-acp` | ACP JSON-RPC client and stdio/Zellij transports implementing the agent client seam. |

## Runtime model

### Workflow catalog

`cowboy-workflow-catalog` provides:

- a built-in default developer workflow
- `.lua` workflow discovery from configured directories
- safe relative `.lua` path validation
- `WorkflowImprovement` application for updating/creating workflow files

The built-in workflow is always available. Project/user workflow directories extend or override selection by adding normal Lua workflow files.

### Workflow definition

Workflow definitions are authored as Lua files. The Lua loader compiles them into core data:

```text
WorkflowSourceRef
  -> WorkflowSourceSnapshot
  -> WorkflowDefinition
       optional config_set selector
       roles: RoleDefinition
       steps: StepDefinition
       transitions: status -> next step
```

The compiled definition is durable data. The Lua VM is not durable; step code is re-evaluated from the saved source snapshot whenever a step runs or a run resumes.

### Workflow run

`WorkflowRun` is the mutable run snapshot:

- run id
- workflow id/hash/source snapshot
- original request
- resolved config-set name and all four effective runner limits
- current step
- latest step-record hash
- run status, including pending ask-user metadata when blocked for input
- inactive legacy resume data retained for old serialized runs
- step/visit counters and durable total/per-step retry counters

`RunHead` is the small mutable pointer for quick lookup/resume. Immutable step/turn/source objects are stored by content hash in `cowboy-workflow-store`.

### Step execution

`WorkflowRuntime` in `cowboy-workflow-engine` is the product interface. It exposes:

- `start_run(request)`
- `resume_run(run_id)` / `step_run(run_id)`
- `answer_run(run_id, prompt_id, answer)`
- `improve_run(run_id)`
- `list_runs()` / `load_events(run_id)`

Internally it uses `WorkflowRunner`, which delegates step semantics to `cowboy-workflow-core::execute_step` and emits workflow events for UI/session consumers. `execute_step` evaluates Lua into a declarative `StepAction`, builds an action context, dispatches the action through `ActionDispatcher`, then applies the returned `ActionResult` uniformly.

One loop iteration:

```text
current WorkflowRun
  -> StepActionProvider evaluates current step
  -> ActionDispatcher runs the StepAction
      agent    -> AgentActionRunner -> AgentExecutor -> ACP Client -> completed StepRecord
      command  -> CommandActionRunner -> tokio::process::Command -> completed StepRecord
      status   -> StatusActionRunner -> completed StepRecord
      ask_user -> AskUserActionRunner -> WaitingForInput with ResumeCallback descriptor
      fail     -> FailActionRunner -> RunStatus::Failed
  -> ActionResult::Completed -> apply_step_record stores record and routes by output.status
  -> ActionResult::Blocked   -> apply_run_status persists waiting/failed status
  -> EventBus emits WorkflowEvent
```

### Config-set resolution and retry policy

`cowboy` parses `[config_sets.<name>]`; each set independently defaults
`max_steps_per_run`, `max_visits_per_step`, `max_retries_per_run`, and
`max_retries_per_step` to `100`, `20`, `200`, and `2`. The built-in `default`
set is always present. Retry limits may be zero, while step and visit limits
must be nonzero. The old top-level runner-limit keys are rejected.

After Lua compilation, `WorkflowRuntime` resolves the workflow's optional
`config_set` (or `default`) before persisting a new `WorkflowRun`. Unknown sets
fail before run persistence. The resolved name and limits are durable run
state, so every later resume, step, answer, resolve, and resolution-options
path is independent of current process configuration.

`WorkflowRunner` reserves each recoverable retry by incrementing the run-wide
and per-step-id counters and saving the run before emitting `StepRetrying` or
dispatching. Both budgets are cumulative; repeated visits share the per-step
remainder. Initial attempts and non-recoverable failures consume no retry
budget, and retries consume no step/visit budget. Event attempts stay local to
the current visit, with one fixed `max_attempts` derived from the smaller
remaining retry budget. Run-budget exhaustion takes precedence when both
ceilings are exhausted.

### Agent execution

`cowboy-workflow-agent` handles `StepAction::Agent`:

- builds the role prompt
- sends it through `cowboy-agent-client::Client`
- parses YAML frontmatter + Markdown body into `StepOutput`
- stores per-role backend sessions keyed by `(run_id, role_id)`
- captures visible output and turn records

`WorkflowRuntime` wires this to ACP via `cowboy-agent-acp` using the configured command, args, and model.

### User input

`cowboy-workflow-engine::ResumeRouter` handles `action.ask_user` answers:

1. validates the run is in `RunStatus::WaitingForInput`
2. validates prompt id and allowed choices
3. dispatches the persisted `ResumeCallback` through `ResumeCallbackRegistry`
4. applies the callback-produced `ActionResult` through the same `apply_step_record` / `apply_run_status` paths as other actions
5. emits and persists the ask-user `StepCompleted` event before resumed-step events

Answering does not increment step budgets. The next Lua step receives the answer as `ctx.prev.fields.answer` with `ctx.prev.action == "ask_user"`; `ctx.resume` is inactive legacy state.

No Lua coroutine or host-call replay cache is persisted.

### Event logs

Run bodies live in `RunStore` (`workflow_store`, currently `redb`). Workflow
events are persisted for display/debugging under:

```text
<state_dir>/events/<run-id>.json
```

## CLI

The `cowboy` binary parses argv through `cowboy-command-parser` and dispatches parsed commands in the app crate:

```bash
cowboy                                  # launch TUI
cowboy tui                              # launch TUI explicitly
cowboy run <request...>                 # start a run; --step runs only the first step
cowboy run --workflow <workflow-id> <request...>  # start a specific catalog workflow id
cowboy step <run-id>                    # execute exactly one further workflow step
cowboy resume <run-id>                  # continue until the workflow blocks, fails, or completes
cowboy answer <run-id> <prompt-id> <answer>  # answer an ask-user prompt
cowboy improve <run-id>                 # summarize and apply workflow-file improvements
cowboy resolve <run-id>                 # list statuses a failed run can resolve to
cowboy resolve <run-id> <status> [--fields <json>] [--body <text>]  # resolve a failed step
cowboy runs                             # list workflow runs
```

Recoverable step failures consume the snapshotted cumulative run-wide and
per-step-id retry budgets described above. Exhaustion persists `Failed` while
keeping the current step available to `cowboy resolve`.

## TUI

The TUI accepts plain requests and slash commands in its composer. Slash command parsing and completion metadata come from `cowboy-command-parser`; the app crate owns dispatch, pending-prompt fallback, and rendering. Runtime behavior is delegated to `cowboy-workflow-engine`, and the TUI renders the workflow event stream from the runtime event bus.

Current vertical layout:

| View | Responsibility |
| --- | --- |
| Header | Active Cowboy state, step, run id, workflow name, and background task count. |
| Transcript | Workflow event stream: lifecycle events, exact agent prompt, agent thinking, agent responses, tool calls, tool updates, prompt cards, failures, suspensions, and completion. |
| Status strip | Context-sensitive state and key hints. |
| Composer | Plain requests, prompt answers, multiline input, and slash-command suggestions. |

Slash commands:

```text
/run [--step] [--workflow <workflow-id>] <request>
/step <run-id>
/resume <run-id>
/answer <run-id> <prompt-id> <answer>
/runs
/workflows
/improve <run-id>
/resolve <run-id>
/resolve <run-id> <status> [fields-json]
/cancel
/help
/exit
```
