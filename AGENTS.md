# AGENTS.md — Codebase Guide for AI Agents

## What is this?

**Cowboy** is a Rust-based, workflow-first AI agent orchestrator.

It has one binary with two interfaces:

- `cowboy` with no subcommand launches the interactive terminal UI.
- `cowboy <subcommand>` runs a non-interactive CLI command against the same workflow runtime and persisted state.

Workflows are Lua files. A workflow step can run an ACP-compatible coding agent, run a command-line program with explicit args, return a status immediately, ask the user for input, or fail.

## Quick Start

```bash
cargo build
cargo test
cargo run                              # launch TUI
cargo run -- run add a /healthz route  # start a workflow from CLI
cargo run -- runs                      # list workflow runs
```

Default config path: `${XDG_CONFIG_HOME:-~/.config}/cowboy/config.toml`.

If no config exists, Cowboy uses defaults:

- state dir: `${XDG_STATE_HOME:-~/.local/state}/cowboy`
- workflow store: `${XDG_STATE_HOME:-~/.local/state}/cowboy/workflow.redb`
- agent command: `copilot --acp`
- user workflows: `${XDG_CONFIG_HOME:-~/.config}/cowboy/workflows`

## CLI Interface

```bash
cowboy                                  # launch TUI
cowboy tui                              # launch TUI explicitly
cowboy run <request...>                 # start a run; --step runs only the first step
cowboy run --workflow <workflow-id> <request...>  # start a specific catalog workflow id
cowboy step <run-id>                    # execute exactly one further workflow step
cowboy resume <run-id>                  # continue a run until it blocks, fails, or completes
cowboy answer <run-id> <prompt-id> <answer>  # answer an ask-user prompt
cowboy improve <run-id>                 # summarize and apply workflow-file improvements
cowboy resolve <run-id>                 # list statuses a failed run can resolve to
cowboy resolve <run-id> <status> [--fields <json>] [--body <text>]  # resolve a failed step
cowboy runs                             # list workflow runs
```

`--workflow <workflow-id>` uses the catalog id shown by `/workflows` or other catalog listings, not necessarily the Lua-declared workflow name.

Recoverable step failures (missing agent frontmatter, transient backend errors)
are auto-retried up to `max_retries_per_step` before a run gives up as `Failed`;
the failed step stays current so `cowboy resolve` can continue the run.

## TUI Interface

The TUI accepts plain requests by default. When a workflow is waiting for
`ask_user`, type the answer directly and press `Enter`.

Slash command parsing and suggestions come from `cowboy-command-parser`; the TUI
app crate owns dispatch, pending-prompt fallback, rendering, and background task
cancellation.

Built-in slash commands:

```text
/run [--step] [--workflow <workflow-id>] <request>
/step <run-id>
/resume <run-id>
/answer <run-id> <prompt-id> <answer>
/improve <run-id>
/resolve <run-id>
/resolve <run-id> <status> [fields-json]
/runs
/workflows
/cancel
/help
/exit
```

`/run --workflow <workflow-id> <request>` uses the catalog workflow id shown by `/workflows`, not necessarily the Lua-declared workflow name.

Keys currently supported:

| Key | Action |
| --- | --- |
| `Enter` | Submit current input |
| `Shift+Enter` / `Ctrl+Enter` | Insert a newline in the input |
| `↑` / `↓` | Browse command history |
| `PgUp` / `PgDn` | Scroll event history |
| `End` | Follow latest event |
| `Ctrl+C` | Cancel active background task |
| `Backspace` | Delete one input character |
| `Esc` / `q` | Quit |

## Project Layout

```text
.
├── Cargo.toml                    # workspace root
├── crates/
│   ├── agent/
│   │   ├── acp/                  # cowboy-agent-acp: ACP JSON-RPC client + transports
│   │   └── client/               # cowboy-agent-client: provider-neutral Client trait/types
│   ├── log/                      # cowboy-log: shared file-based tracing/log setup
│   ├── workflow/
│   │   ├── actions/              # StepAction dispatchers and reusable action runners
│   │   ├── agent/                # StepAction::Agent execution, session reuse, output parsing
│   │   ├── catalog/              # built-in + filesystem workflow catalog and updates
│   │   ├── core/                 # workflow model, traits, graph validation, execute_step
│   │   ├── engine/               # product runtime used by CLI/TUI
│   │   ├── lua/                  # sandboxed Lua workflow loader/runtime
│   │   └── store/                # redb-backed RunStore
│   └── tui/
│       ├── app/                  # cowboy package: CLI/config + ratatui shell only
│       └── command-parser/       # CLI argv and interactive slash command grammar
├── demo-config.toml              # project-root demo config for local runs
├── docs/                         # architecture, module map, workflow authoring, plans
├── examples/                     # example Lua workflows
└── README.md
```

## Crate Responsibilities

### `cowboy` (`crates/tui/app`)

UI/CLI shell only.

Runtime behavior and command grammar do **not** belong here. This crate should contain:

- config loading and conversion to engine config
- runtime dispatch for parsed CLI and slash commands
- ratatui rendering and input handling
- TUI state projection, transcript rendering, and input history persistence
- logging initialization for the product binary

Current modules:

- `main.rs` — binary entrypoint; uses `cowboy-command-parser` for CLI parsing; default command and `tui` launch the TUI, other subcommands call `WorkflowRuntime`.
- `app.rs` — terminal startup, event loop, and top-level layout.
- `app/commands.rs` — slash command dispatch, runtime task spawning, help/status rendering, plain-text submission, and pending-prompt fallback.
- `app/input.rs` — keyboard handling, multiline input editing, history movement, scroll keys, and cancellation keys.
- `app/history.rs` — locked append-only JSON-lines composer history under `state_dir`.
- `app/state.rs` — active run, current step, pending prompt, transcript entries, command history, scroll offset, and background task state.
- `app/events.rs` — workflow event projection into transcript text.
- `app/markup.rs` — lightweight transcript markup parsing/rendering helpers.
- `app/styles.rs` — shared ratatui colors/styles and width-safe truncation helpers.
- `app/controls/*` — header, transcript, status strip, and composer widgets.
- `config.rs` — TOML config and conversion to `cowboy-workflow-engine::RuntimeConfig`.
- `lib.rs` — TUI crate exports.

### `cowboy-command-parser` (`crates/tui/command-parser`)

Runtime/UI-independent command grammar.

Owns:

- clap-backed product CLI parsing
- interactive TUI slash command parsing
- slash command metadata and suggestions
- quote/hash-preserving slash tokenization

This crate must stay independent of `cowboy-workflow-engine`, ratatui, crossterm,
tui input state, app state, and config loading.

### `cowboy-log` (`crates/log`)

Shared file-based diagnostic logging setup for binaries and test apps.

Owns:

- `init_file_logging`
- `DEFAULT_DIRECTIVE = "info"` for the final `cowboy` product binary
- `TEST_APP_DIRECTIVE = "debug"` for local diagnostic test apps such as `engine-cli`
- `COWBOY_LOG` / `RUST_LOG` env-filter wiring

### `cowboy-workflow-engine` (`crates/workflow/engine`)

Product runtime module between UI/CLI and lower-level workflow crates.

Owns:

- `WorkflowRuntime`
- workflow event projection and event logs
- ask-user answer routing through `ResumeRouter`
- selector/summarizer adapters
- runtime wiring for catalog, Lua, redb store, action dispatch, and ACP-backed agent execution

Important modules:

- `runtime.rs` — `WorkflowRuntime`, runtime config, start/resume/step/answer/improve/resolve/list operations, recoverable-retry give-up persistence, catalog/store/Lua/action/agent wiring, and event-log persistence.
- `runner.rs` — `WorkflowRunner<S, D, P>` over `cowboy-workflow-core::execute_step`; owns event emission and the bounded recoverable-retry loop (`max_retries_per_step`); persists `Failed` on give-up; `LuaStepActionProvider` builds Lua `ctx`.
- `events.rs` — `WorkflowEvent`, `WorkflowEventKind`, live `StepProgress`, `EventBus`.
- `input.rs` — `ResumeRouter` for `RunStatus::WaitingForInput` answers and persisted resume callbacks.
- `workflow.rs` — deterministic/agent selectors and agent summarizer.

### `cowboy-workflow-actions` (`crates/workflow/actions`)

Reusable host-action runners and dispatch policy for declarative `StepAction` values.

Owns:

- `EngineActionDispatcher`
- `ResumeCallbackRegistry`
- action runners for `agent`, `command`, `status`, `ask_user`, and `fail`
- `ask_user` resume callback payloads and callback-to-`StepRecord` handling

Important modules:

- `lib.rs` — dispatcher, resume callback registry, and public runner exports.
- `agent.rs` — `AgentActionRunner` adapter over `cowboy-workflow-agent::AgentExecutor`.
- `command.rs` — `CommandActionRunner` for direct non-shell process execution from runtime cwd.
- `ask_user.rs` — `AskUserActionRunner`, callback payload metadata, and resume handling into `StepRecord`.
- `status.rs` — `StatusActionRunner` for immediate completed records.
- `fail.rs` — `FailActionRunner` for failed run statuses.

### `cowboy-workflow-catalog` (`crates/workflow/catalog`)

Workflow catalog policy.

Owns:

- built-in default developer workflow
- `.lua` workflow directory scanning
- safe relative `.lua` path normalization
- workflow source loading/materialization
- applying `WorkflowImprovement` to workflow files

Public concepts include `WorkflowCatalogLoader`, `CatalogRoot`,
`LoadedWorkflowSource`, `AppliedWorkflowImprovement`, `load_source_ref`, and
`apply_improvement`.

### `cowboy-workflow-core` (`crates/workflow/core`)

Pure workflow domain model and execution rules.

Owns:

- `WorkflowCatalog`, `WorkflowSourceRef`, `WorkflowDefinition`
- `RoleDefinition`, `StepDefinition`, `StepTransitions`
- `StepAction`: `agent`, `command`, `status`, `ask_user`, `fail`
- `WorkflowRun`, `RunStatus`, `RunHead`, `StepRecord`, `TurnRecord`
- `ResumeCallback`, `ActionResult`, `ExecutionContext`, `RunStore`, `ActionDispatcher`, `StepActionProvider`, `WorkflowSelector`, `WorkflowSummarizer`
- `execute_step`, budget enforcement, and step-record/status application helpers

Important modules:

- `ids.rs` — workflow/run/role/step/record/turn/object id aliases.
- `definition.rs` — workflow catalog, source refs, roles, steps, transitions, and graph validation.
- `action.rs` — declarative `StepAction` variants.
- `state.rs` — durable run, status, resume callback, record, output, head, session, and object state.
- `traits.rs` — core traits for stores, dispatchers, providers, selectors, and summarizers.
- `engine.rs` — pure `execute_step` semantics and budget checks.
- `summary.rs` — workflow improvement/summary types.
- `error.rs` — workflow errors.

Core must stay independent of TUI, Lua, redb, ACP, and SDK/provider details.

### `cowboy-workflow-lua` (`crates/workflow/lua`)

Sandboxed Lua workflow support.

Owns:

- workflow authoring globals: `role`, `step`, `workflow`, `action`, scoped `require`
- workflow source loading and import snapshotting
- one-step Lua runtime execution
- conversion from Lua tables to core Rust data/actions
- Lua step context fields: `request`, `run_id`, `workflow`, `current_step`, `step`, `prev`, and `steps_executed`

Important modules:

- `api.rs` — workflow authoring API installed into the Lua sandbox.
- `convert.rs` — Lua table/value conversion to core workflow definitions and actions.
- `loader.rs` — filesystem and snapshot workflow compilation.
- `runtime.rs` — one-step `step.run(ctx)` execution.
- `imports.rs` — scoped workflow-root imports.
- `sandbox.rs` — allowlisted Lua runtime.
- `bin/workflow-chart.rs` — Lua workflow chart test app.

### `cowboy-workflow-store` (`crates/workflow/store`)

redb-backed `RunStore` implementation.

Owns:

- mutable workflow runs
- run heads
- immutable content-addressed objects
- turn indexes
- role sessions
- low-level cleanup helpers

Important modules:

- `redb_store.rs` — redb-backed `RunStore`, object, turn, and role-session operations.
- `tables.rs` — table definitions.
- `hash.rs` — content-addressed object hashing.
- `error.rs` — store-specific errors.
- `bin/store-cli.rs` — store inspection test app.

### `cowboy-workflow-agent` (`crates/workflow/agent`)

Agent action execution.

Owns:

- `AgentExecutor`
- `ClientFactory`
- per-`(run_id, role_id)` backend session reuse, persisted through role sessions
- role/action prompt construction
- YAML-frontmatter + Markdown output parsing into `StepOutput`
- turn capture
- live `AgentProgress` / `ProgressSink` updates for UI-visible agent/tool progress
- `execute-agent` test app

Important modules:

- `executor.rs` — agent action execution, session reuse/load/create, progress emission, and turn collection.
- `prompt.rs` — role/task prompt and required frontmatter instructions.
- `frontmatter.rs` — YAML frontmatter + Markdown body parsing.
- `error.rs` — agent execution errors.
- `bin/execute-agent.rs` — agent executor test app.

### `cowboy-agent-client` (`crates/agent/client`)

Provider-neutral agent backend interface.

Owns:

- `Client` trait
- `ModelInfo`, `AgentInfo`, `PromptContent`, `Event`, `StopReason`

Important modules:

- `traits.rs` — provider-neutral `Client` trait.
- `types.rs` — model, agent, prompt, event, and stop-reason types.

### `cowboy-agent-acp` (`crates/agent/acp`)

ACP backend implementation.

Owns:

- ACP JSON-RPC client implementing `cowboy-agent-client::Client`
- ACP messages and parsing
- stdio and Zellij transports
- backend preset resolution
- `acp-chat` test app

Important modules:

- `client.rs` — ACP JSON-RPC client, session/new/load/prompt, permission handling.
- `messages.rs` — ACP envelope and session/update parsing.
- `transport/stdio.rs` — local subprocess JSON-RPC over stdio.
- `transport/zellij.rs` — Zellij-backed transport.
- `backend.rs` — backend preset resolution.
- `bin/acp-chat.rs` — ACP chat test app.

## Data Flow

### Starting a workflow run

```text
CLI argv or TUI composer input
  -> cowboy-command-parser parses CLI/slash command grammar
  -> cowboy app dispatches to cowboy-workflow-engine::WorkflowRuntime
  -> catalog loads/selects WorkflowSourceRef
  -> workflow-lua compiles/snapshots Lua source
  -> WorkflowRun is saved through RunStore
  -> WorkflowRunner loops execute_step until terminal/waiting/failed
  -> ActionDispatcher maps StepAction to ActionResult
  -> EventBus emits WorkflowEvent / StepProgress
  -> CLI prints report/progress or TUI renders live events
```

### Step execution

```text
WorkflowRun.current_step
  -> core loads previous StepRecord from run.head, if present
  -> LuaStepActionProvider evaluates step.run(ctx)
       ctx.prev contains previous output status/fields/body/raw, or null
  -> StepAction
       agent    -> AgentActionRunner -> AgentExecutor -> ACP Client -> StepRecord
       command  -> CommandActionRunner -> tokio::process::Command -> StepRecord
       status   -> StatusActionRunner -> StepRecord
       ask_user -> AskUserActionRunner -> RunStatus::WaitingForInput + ResumeCallback
       fail     -> FailActionRunner -> RunStatus::Failed
  -> ActionResult::Completed stores a StepRecord and routes by output.status
  -> ActionResult::Blocked stores the run status
  -> RunStore saves WorkflowRun + RunHead + objects
```

### Resume / answer

```text
cowboy answer <run-id> <prompt-id> <answer>
  -> ResumeRouter validates RunStatus::WaitingForInput, prompt id, and choices
  -> ResumeRouter dispatches the persisted ResumeCallback
  -> callback produces an ActionResult through the same record-routing path
  -> ask-user StepRecord is stored and surfaced as ctx.prev to the next Lua step
```

Answering does not mutate `run.resume` or increment step budgets. New workflows
should read ask-user answers from `ctx.prev.fields.answer`; `ctx.resume` is only
legacy serialized state.

### Retry / resolve

```text
Recoverable step failure (e.g. MissingFrontmatter, transient Client error)
  -> WorkflowRunner retries the current step up to max_retries_per_step
       (budget-safe: retries don't consume max_visits_per_step)
       (agent retries append a corrective frontmatter nudge, reusing the session)
  -> on success: run continues
  -> on give-up: run persisted RunStatus::Failed { reason }, current_step retained

cowboy resolve <run-id>                 # list resolvable statuses + required fields
cowboy resolve <run-id> <status> [--fields <json>] [--body <text>]
  -> WorkflowRuntime::resolution_options recomputes the failed step's action
       (via LuaStepActionProvider) to recover valid statuses + OutputSpec fields
  -> resolve_run validates status routes via next_step and required fields present
  -> synthesizes a completed StepRecord, flips to Running, and continues
```

## Configuration

Example `~/.config/cowboy/config.toml`:

```toml
state_dir = "~/.local/state/cowboy"
workflow_store = "~/.local/state/cowboy/workflow.redb"
workflow_dirs = [".cowboy/workflows", "~/.config/cowboy/workflows"]
max_steps_per_run = 100
max_visits_per_step = 20
max_retries_per_step = 2

[[agents]]
name = "default"
command = "copilot"
args = ["--acp"]

[agents.model]
id = "claude-sonnet-4.5"
provider = "anthropic"
```

`workflow_dirs` are optional. The built-in default workflow is always available.

## Persistence

Cowboy stores workflow runtime state under `state_dir`:

```text
workflow.redb                    # runs, run heads, immutable step/source/turn objects, role sessions
events/<run-id>.json             # persisted workflow event log for display/debugging
logs/cowboy.log                  # diagnostic log; level from COWBOY_LOG / RUST_LOG
```

The final `cowboy` binary defaults to `info` logging. Test apps default to
`debug` because they are diagnostic tools. Override either with `COWBOY_LOG` or
`RUST_LOG`.

## Test Apps

`just test-apps` builds helper binaries into `target/debug/test-apps`:

- `workflow-chart`
- `store-cli`
- `execute-agent`
- `acp-chat`
- `catalog-cli`
- `engine-cli`

## Design Rules for Future Agents

- Keep `crates/tui/app` UI/CLI/config/runtime-dispatch-only.
- Keep `crates/tui/command-parser` independent from runtime, terminal UI, config, and app state.
- Put runtime orchestration in `cowboy-workflow-engine`.
- Put action dispatchers and reusable action runners in `cowboy-workflow-actions`.
- Put catalog/file policy in `cowboy-workflow-catalog`.
- Put pure domain semantics in `cowboy-workflow-core`.
- Put Lua loading/execution in `cowboy-workflow-lua`.
- Put storage backend behavior in `cowboy-workflow-store`.
- Put agent step execution in `cowboy-workflow-agent`.
- Put provider-neutral agent contracts in `cowboy-agent-client`.
- Put ACP-specific protocol/transport code in `cowboy-agent-acp`.
- Put shared logging setup and default tracing directives in `cowboy-log`; binaries call it instead of configuring `tracing` directly.
- For every code change, run the narrowest relevant checks and fix all Rust compiler warnings and Clippy warnings before yielding.
- Do not reintroduce the old fixed `pipeline`/`SubTask` model.
- Do not add workflow runtime logic to either TUI crate just because UI calls it.

## Docs

- `docs/architecture.md` — system overview
- `docs/module-map.md` — crate/module responsibility map
- `docs/workflow-authoring.md` — Lua workflow authoring guide
- `docs/plans/` — implementation and investigation plans
