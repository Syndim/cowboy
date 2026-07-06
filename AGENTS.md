# AGENTS.md — Codebase Guide for AI Agents

## What is this?

**Cowboy** is a Rust-based, workflow-first AI agent orchestrator.

It has one binary with two interfaces:

- `cowboy` with no subcommand launches the interactive terminal UI.
- `cowboy <subcommand>` runs a non-interactive CLI command against the same workflow runtime and persisted state.

Workflows are Lua files. A workflow step can run an ACP-compatible coding agent, return a status immediately, ask the user for input, fail, or suspend.

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
cowboy run <request...>                 # start a run; --step runs only the first step
cowboy run --workflow <workflow-id> <request...>  # start a specific catalog workflow id
cowboy step <run-id>                    # execute exactly one further workflow step
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

Built-in slash commands:

```text
/run <request>
/run-workflow <workflow-id> <request>
/improve <run-id>
/resolve <run-id>
/resolve <run-id> <status>
/runs
/workflows
/help
/exit
```

`/run-workflow <workflow-id> <request>` uses the catalog workflow id shown by `/workflows`, not necessarily the Lua-declared workflow name.

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
│   │   ├── agent/                # StepAction::Agent execution, session reuse, output parsing
│   │   ├── catalog/              # built-in + filesystem workflow catalog and updates
│   │   ├── core/                 # workflow model, traits, graph validation, execute_step
│   │   ├── engine/               # product runtime used by CLI/TUI
│   │   ├── lua/                  # sandboxed Lua workflow loader/runtime
│   │   └── store/                # redb-backed RunStore
│   └── tui/                      # cowboy package: CLI/config + ratatui shell only
├── demo-config.toml              # project-root demo config for local runs
├── docs/                         # architecture, module map, remaining work
└── README.md
```

## Crate Responsibilities

### `cowboy` (`crates/tui`)

UI/CLI shell only.

Runtime behavior does **not** belong here. This crate should contain:

- CLI argument parsing
- config loading and conversion to engine config
- ratatui rendering
- input handling that delegates to the runtime

Current modules:

- `main.rs` — clap entrypoint; initializes file logging; no subcommand launches TUI; subcommands call `WorkflowRuntime`.
- `app.rs` — ratatui shell; accepts plain requests and slash commands; renders live workflow/progress events while runtime calls run in background tasks.
- `config.rs` — TOML config and conversion to `cowboy-workflow-engine::RuntimeConfig`.
- `lib.rs` — TUI crate exports.

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
- ask-user answer routing
- selector/summarizer adapters
- runtime wiring for catalog, Lua, redb store, and ACP-backed agent execution

Important modules:

- `runtime.rs` — `WorkflowRuntime`, runtime config, start/resume/step/answer/improve/resolve/list operations, recoverable-retry give-up persistence, catalog/store/Lua/agent wiring, and event-log persistence.
- `runner.rs` — `WorkflowRunner<S, E, P>` over `cowboy-workflow-core::execute_step`; owns the bounded recoverable-retry loop (`max_retries_per_step`) and persists `Failed` on give-up; `LuaStepActionProvider` builds Lua `ctx`, including `ctx.prev` from the latest completed step.
- `events.rs` — `WorkflowEvent`, `WorkflowEventKind`, live `StepProgress`, `EventBus`.
- `input.rs` — `InputRouter` for `RunStatus::WaitingForInput` answers.
- `workflow.rs` — deterministic/agent selectors and agent summarizer.

### `cowboy-workflow-catalog` (`crates/workflow/catalog`)

Workflow catalog policy.

Owns:

- built-in default developer workflow
- `.lua` workflow directory scanning
- safe relative `.lua` path normalization
- workflow source loading/materialization
- applying `WorkflowImprovement` to workflow files

Important modules:

- `builtin.rs` — built-in default developer workflow source.
- `loader.rs` — `WorkflowCatalogLoader`, catalog roots, and `.lua` scanning.
- `source.rs` — source-ref loading/materialization and safe workflow path normalization.
- `improvement.rs` — applying `WorkflowImprovement` to workflow files.
- `bin/catalog-cli.rs` — catalog test app.

### `cowboy-workflow-core` (`crates/workflow/core`)

Pure workflow domain model and execution rules.

Owns:

- `WorkflowCatalog`, `WorkflowSourceRef`, `WorkflowDefinition`
- `RoleDefinition`, `StepDefinition`, `StepTransitions`
- `StepAction`: `agent`, `status`, `ask_user`, `fail`, `suspend`
- `WorkflowRun`, `RunStatus`, `RunHead`, `StepRecord`, `TurnRecord`
- `RunStore`, `ActionExecutor`, `StepActionProvider` (receives previous `StepRecord`), `WorkflowSelector`, `WorkflowSummarizer`
- `execute_step`, which loads the previous step record from `run.head` before evaluating the next action

Important modules:

- `ids.rs` — workflow/run/role/step/record/turn/object id aliases.
- `definition.rs` — workflow catalog, source refs, roles, steps, transitions, and graph validation.
- `action.rs` — declarative `StepAction` variants.
- `state.rs` — durable run, status, record, output, head, session, and object state.
- `traits.rs` — core traits for stores, executors, providers, selectors, and summarizers.
- `engine.rs` — pure `execute_step` semantics.
- `summary.rs` — workflow improvement/summary types.

Core must stay independent of TUI, Lua, redb, ACP, and SDK/provider details.

### `cowboy-workflow-lua` (`crates/workflow/lua`)

Sandboxed Lua workflow support.

Owns:

- workflow authoring globals: `role`, `step`, `workflow`, `action`, scoped `require`
- workflow source loading and import snapshotting
- one-step Lua runtime execution
- conversion from Lua tables to core Rust data/actions
- Lua step context fields: `request`, `run_id`, `workflow`, `current_step`, `step`, `resume`, `prev`, and `steps_executed`

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
CLI/TUI request
  -> cowboy-workflow-engine::WorkflowRuntime
  -> cowboy-workflow-catalog loads/selects WorkflowSourceRef
  -> cowboy-workflow-lua compiles/snapshots Lua source
  -> WorkflowRun saved through RunStore
  -> WorkflowRunner loops execute_step until terminal/waiting/suspended/failed
  -> EventBus emits WorkflowEvent / StepProgress
  -> CLI prints report/progress or TUI renders live events
```

### Step execution

```text
WorkflowRun.current_step
  -> core loads previous StepRecord from run.head, if present
  -> LuaStepActionProvider evaluates step.run(ctx)
       ctx.resume contains ask-user answers
       ctx.prev contains previous output status/fields/body/raw, or null
  -> StepAction
       agent    -> AgentExecutor -> ACP Client -> StepRecord + StepProgress events
       status   -> StepRecord
       ask_user -> RunStatus::WaitingForInput
       fail     -> RunStatus::Failed
       suspend  -> RunStatus::Suspended
  -> RunStore saves WorkflowRun + RunHead + objects
```

### Resume / answer

```text
cowboy answer <run-id> <prompt-id> <answer>
  -> InputRouter validates prompt id and choices
  -> writes run.resume[prompt_id] = answer
  -> marks run Running
  -> runtime resumes same step with ctx.resume populated
```

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

- Keep `crates/tui` UI/CLI-only.
- Put runtime orchestration in `cowboy-workflow-engine`.
- Put catalog/file policy in `cowboy-workflow-catalog`.
- Put pure domain semantics in `cowboy-workflow-core`.
- Put Lua loading/execution in `cowboy-workflow-lua`.
- Put storage backend behavior in `cowboy-workflow-store`.
- Put agent step execution in `cowboy-workflow-agent`.
- Put provider-neutral agent contracts in `cowboy-agent-client`.
- Put ACP-specific protocol/transport code in `cowboy-agent-acp`.
- Put shared logging setup and default tracing directives in `cowboy-log`; binaries call it instead of configuring `tracing` directly.
- Do not reintroduce the old fixed `pipeline`/`SubTask` model.
- Do not add workflow runtime logic to the TUI crate just because UI calls it.

## Known Remaining Work

See `docs/remaining-work.md` for feature gaps and refinement tasks that are not complete yet.

## Docs

- `docs/architecture.md` — system overview
- `docs/module-map.md` — crate/module responsibility map
- `docs/remaining-work.md` — feature completeness gaps and future work
- `docs/workflow-migration.md` — workflow migration notes
- `docs/workflow-refactor-proposal.md` — workflow refactor proposal
