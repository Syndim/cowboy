# Cowboy architecture

Cowboy is a workflow-first terminal agent orchestrator. It has one binary with two interfaces:

- `cowboy` with no subcommand launches the interactive TUI.
- `cowboy <subcommand>` runs a non-interactive CLI command against the same workflow runtime and state.

The current shape keeps UI/CLI code separate from workflow runtime logic:

```text
cowboy CLI/TUI
  -> cowboy-workflow-engine
       -> catalog loading / workflow selection / summarization
       -> WorkflowRun state orchestration
       -> Lua step action provider
       -> ACP-backed agent action executor / input router
       -> StepRecord + RunHead persistence
       -> workflow events + event log
  -> TUI renders workflow events and accepts commands
```

## Workspace crates

| Crate | Purpose |
| --- | --- |
| `cowboy` (`crates/tui`) | CLI argument parsing, config loading, and ratatui rendering only. |
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
- current step
- latest step-record hash
- run status
- resume data from user input
- step budget counters

`RunHead` is the small mutable pointer for quick lookup/resume. Immutable step/turn/source objects are stored by content hash in `cowboy-workflow-store`.

### Step execution

`WorkflowRuntime` in `cowboy-workflow-engine` is the product interface. It exposes:

- `start_run(request)`
- `resume_run(run_id)` / `step_run(run_id)`
- `answer_run(run_id, prompt_id, answer)`
- `improve_run(run_id)`
- `list_runs()` / `load_events(run_id)`

Internally it uses `WorkflowRunner`, which delegates step semantics to `cowboy-workflow-core::execute_step` and emits workflow events for UI/session consumers.

One loop iteration:

```text
current WorkflowRun
  -> StepActionProvider evaluates current step
  -> StepAction
      agent    -> AgentExecutor -> ACP Client -> StepRecord
      status   -> StepRecord
      ask_user -> RunStatus::WaitingForInput
      fail     -> RunStatus::Failed
      suspend  -> RunStatus::Suspended
  -> RunStore persists WorkflowRun + RunHead/objects
  -> EventBus emits WorkflowEvent
```

### Agent execution

`cowboy-workflow-agent` handles `StepAction::Agent`:

- builds the role prompt
- sends it through `cowboy-agent-client::Client`
- parses YAML frontmatter + Markdown body into `StepOutput`
- stores per-role backend sessions keyed by `(run_id, role_id)`
- captures visible output and turn records

`WorkflowRuntime` wires this to ACP via `cowboy-agent-acp` using the configured command, args, and model.

### User input

`cowboy-workflow-engine::InputRouter` handles `action.ask_user` answers:

1. validates the run is in `RunStatus::WaitingForInput`
2. validates prompt id and allowed choices
3. writes `run.resume[prompt_id] = answer`
4. marks the run `Running`
5. the next runner pass re-evaluates the same step with `ctx.resume`

No Lua coroutine or host-call replay cache is persisted.

### Event logs

Run bodies live in `RunStore` (`workflow_store`, currently `redb`). Workflow
events are persisted for display/debugging under:

```text
<state_dir>/events/<run-id>.json
```

## CLI

```bash
cowboy                                  # launch TUI
cowboy run <request...>                 # start a run; --step runs only the first step
cowboy step <run-id>                    # execute exactly one further workflow step
cowboy answer <run-id> <prompt-id> <answer>  # answer an ask-user prompt
cowboy improve <run-id>                 # summarize and apply workflow-file improvements
cowboy runs                             # list workflow runs
```

## TUI

The TUI accepts plain requests and slash commands in its composer. It delegates all runtime behavior to `cowboy-workflow-engine` and renders the workflow event stream from the runtime event bus.

Current vertical layout:

| View | Responsibility |
| --- | --- |
| Header | Active Cowboy state, step, run id, workflow name, and background task count. |
| Transcript | Workflow event stream: lifecycle events, exact agent prompt, agent thinking, agent responses, tool calls, tool updates, prompt cards, failures, suspensions, and completion. |
| Status strip | Context-sensitive state and key hints. |
| Composer | Plain requests, prompt answers, multiline input, and slash-command suggestions. |

Slash commands:

```text
/run <request>
/run-step <request>
/step <run-id>
/answer <run-id> <prompt-id> <answer>
/runs
/workflows
/improve <run-id>
/cancel
/help
/exit
```
