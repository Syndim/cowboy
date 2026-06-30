# Cowboy module map

Current workspace/module structure. The TUI crate is intentionally thin; workflow runtime logic lives under `crates/workflow/*`.

## Workspace crates

```text
.
├── Cargo.toml
├── crates/
│   ├── agent/
│   │   ├── client/          # provider-neutral agent client trait/types
│   │   └── acp/             # ACP JSON-RPC implementation + transports
│   ├── workflow/
│   │   ├── core/            # workflow data model, traits, graph validation, step engine
│   │   ├── catalog/         # built-in + filesystem workflow catalog
│   │   ├── engine/          # product runtime used by UI/CLI
│   │   ├── lua/             # sandboxed Lua workflow loader/runtime
│   │   ├── store/           # redb-backed RunStore
│   │   └── agent/           # agent action executor + output parsing
│   └── tui/                 # cowboy CLI/TUI shell and ratatui controls
├── docs/
└── LICENSE
```

## Crate: `cowboy` (`crates/tui`)

Package name: `cowboy`.

This crate owns only CLI/configuration and terminal rendering. It should not own workflow semantics, session persistence, runner state, selector/summarizer behavior, Lua execution, storage, or agent protocol details.

| Module | Responsibility |
| --- | --- |
| `main.rs` | Shared binary entrypoint. No subcommand launches the TUI; subcommands call `cowboy-workflow-engine::WorkflowRuntime`. |
| `lib.rs` | Public exports for the TUI crate: config and `run_tui`. |
| `config.rs` | Load TOML config and convert it into engine `RuntimeConfig`. |
| `app.rs` | Terminal startup, event loop, and top-level vertical layout only. |
| `app/commands.rs` | Slash command registry, command completion, command dispatch, and runtime task spawning. |
| `app/input.rs` | Keyboard handling, multiline input editing, history movement, scroll keys, and cancellation keys. |
| `app/state.rs` | TUI state projection: active run, current step, pending prompt, transcript entries, command history, scroll offset, and background tasks. |
| `app/events.rs` | Converts typed workflow events into human-readable transcript text. |
| `app/styles.rs` | Shared ratatui colors/styles and width-safe truncation helpers. |
| `app/controls/header.rs` | Header view showing state, step, run, workflow, and task count. |
| `app/controls/transcript.rs` | Transcript view and waiting-for-input cards. |
| `app/controls/status.rs` | Status strip and context-sensitive hints. |
| `app/controls/composer.rs` | Composer view, multiline input rendering, cursor placement, and slash-command suggestions. |

## Crate: `cowboy-workflow-engine`

Package name: `cowboy-workflow-engine`.

This is the product runtime between UI/CLI and lower-level workflow crates.

| Module | Responsibility |
| --- | --- |
| `runtime.rs` | `WorkflowRuntime`: start/resume/step/answer/improve/list workflow runs, wire store/catalog/Lua/agent execution, persist event logs. |
| `events.rs` | `WorkflowEvent`, `WorkflowEventKind`, and broadcast `EventBus`. |
| `input.rs` | `InputRouter`; validates and applies answers for `RunStatus::WaitingForInput`. |
| `runner.rs` | `WorkflowRunner<S, E, P>` wrapper over `cowboy-workflow-core::execute_step`; emits events. Also `LuaStepActionProvider`. |
| `workflow.rs` | Selector/summarizer adapters: deterministic selector, agent-backed selector, agent-backed summarizer. |
| `lib.rs` | Public runtime interface exported to UI/CLI and future frontends. |

Important seams:

- `WorkflowRuntime` is the high-level application interface.
- `WorkflowRunner<S, E, P>` depends on `RunStore`, `ActionExecutor`, and `StepActionProvider`.
- `LuaStepActionProvider` adapts `cowboy-workflow-lua::run_step` into `StepActionProvider`.
- `InputRouter` only mutates `WorkflowRun.resume/status`; it does not execute Lua or agents.
- `AgentWorkflowSelector` and `AgentWorkflowSummarizer` depend only on `cowboy-agent-client::Client`.

## Crate: `cowboy-workflow-catalog`

Package name: `cowboy-workflow-catalog`.

Owns workflow catalog policy.

| Module | Responsibility |
| --- | --- |
| `lib.rs` | Built-in default workflow source, `.lua` workflow directory loading, safe source materialization, `WorkflowImprovement` application. |

Public concepts include `WorkflowCatalogLoader`, `CatalogRoot`, `LoadedWorkflowSource`, `AppliedWorkflowImprovement`, `load_source_ref`, and `apply_improvement`.

## Crate: `cowboy-workflow-core`

Owns workflow domain data and pure execution rules.

| Module | Responsibility |
| --- | --- |
| `ids.rs` | String aliases for workflow/run/role/step/record/turn ids and object hashes. |
| `definition.rs` | `WorkflowCatalog`, `WorkflowSourceRef`, `WorkflowDefinition`, roles, steps, transitions, validation. |
| `action.rs` | Declarative `StepAction` variants: `agent`, `status`, `ask_user`, `fail`, `suspend`. |
| `state.rs` | Durable `WorkflowRun`, `RunStatus`, `StepRecord`, `StepOutput`, `RunHead`, `RoleSession`, object kinds. |
| `summary.rs` | `WorkflowSummary` and `WorkflowImprovement` used after a run. |
| `traits.rs` | Interfaces implemented by outer crates: loader, selector, executor, summarizer, run store. |
| `engine.rs` | `execute_step` and budget enforcement. |
| `error.rs` | `WorkflowError` and `Result`. |

Core must remain independent of TUI, Lua, storage backends, and agent protocols.

## Crate: `cowboy-workflow-lua`

Owns Lua workflow definition loading and step evaluation.

| Module | Responsibility |
| --- | --- |
| `api.rs` | Installs workflow authoring functions: `role`, `step`, `workflow`, `action`, scoped `require`. |
| `sandbox.rs` | Creates a restricted Lua environment. |
| `imports.rs` | Resolves workflow-local imports and snapshots imported source files. |
| `loader.rs` | Loads/compiles workflow sources into `CompiledWorkflow`. |
| `runtime.rs` | Runs one snapshotted Lua step and returns a `StepAction`. |
| `convert.rs` | Converts Lua role/step/workflow/action tables into core Rust types. |
| `error.rs` | Lua loader/runtime errors. |
| `bin/workflow-chart.rs` | Test app that prints a workflow graph. |

## Crate: `cowboy-workflow-store`

Owns the first durable `RunStore` implementation.

| Module | Responsibility |
| --- | --- |
| `redb_store.rs` | `RedbRunStore`; saves runs, heads, role sessions, immutable objects, turn indexes. Cloneable handles share one redb `Database` to avoid reopening locks inside one runtime. |
| `hash.rs` | Canonical JSON object envelope and BLAKE3 object hashes. |
| `tables.rs` | redb table definitions. |
| `error.rs` | Store-specific errors mapped into core errors by the trait implementation. |
| `bin/store-cli.rs` | Test app for saving/loading/deleting store objects and runs. |

## Crate: `cowboy-workflow-agent`

Owns execution of `StepAction::Agent`.

| Module | Responsibility |
| --- | --- |
| `executor.rs` | `AgentExecutor`, `ClientFactory`, per-`(run_id, role_id)` client/session reuse, turn capture. |
| `prompt.rs` | Builds role/action prompt with output instructions. |
| `frontmatter.rs` | Parses YAML frontmatter + Markdown body into normalized `StepOutput`. |
| `error.rs` | Agent execution errors. |
| `bin/execute-agent.rs` | Test app for executing one agent step through an ACP command. |

## Crate: `cowboy-agent-client`

Provider-neutral seam between Cowboy and agent backends.

| Module | Responsibility |
| --- | --- |
| `traits.rs` | `Client` trait: session create/load, prompt, events, close. |
| `types.rs` | `ModelInfo`, `AgentInfo`, `PromptContent`, `Event`, `StopReason`. |

## Crate: `cowboy-agent-acp`

ACP backend implementation.

| Module | Responsibility |
| --- | --- |
| `client.rs` | ACP client implementing `cowboy-agent-client::Client`. |
| `messages.rs` | ACP JSON-RPC message types and parser. |
| `transport/` | stdio and Zellij line transports. |
| `bin/acp-chat.rs` | Test app for chatting with an ACP agent. |

## Current flow

```text
CLI/TUI command
  -> cowboy-workflow-engine WorkflowRuntime
  -> catalog chooses/loads workflow source
  -> workflow-lua compiles/snapshots workflow source
  -> WorkflowRun persisted through RunStore
  -> WorkflowRunner loops execute_step
  -> LuaStepActionProvider returns StepAction
  -> AgentExecutor/InputRouter handles action
  -> RunStore saves run/head/objects
  -> EventBus emits WorkflowEvent
  -> TUI renders events or CLI prints report
```

## Refactoring guidance

- Keep `crates/tui` as UI/CLI only.
- Keep application runtime orchestration in `cowboy-workflow-engine`.
- Keep catalog policy in `cowboy-workflow-catalog`.
- Keep workflow semantics in `cowboy-workflow-core`.
- Keep Lua VM setup and import policy in `cowboy-workflow-lua`.
- Keep backend session management in `cowboy-workflow-agent`.
- Do not reintroduce the old hardcoded `pipeline`/`SubTask` model into the TUI crate.
