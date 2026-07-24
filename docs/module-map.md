# Cowboy module map

Current workspace/module structure. The TUI app crate is intentionally thin; workflow runtime logic lives under `crates/workflow/*`, and command grammar lives in `crates/tui/command-parser`.

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
│   │   ├── store/           # SQLx/SQLite WorkflowStore implementation
│   │   └── agent/           # agent action executor + output parsing
│   └── tui/
│       ├── app/             # cowboy CLI/TUI shell and ratatui controls
│       └── command-parser/  # clap-backed CLI and slash command parsing
├── docs/
└── LICENSE
```

## Crate: `cowboy` (`crates/tui/app`)

Package name: `cowboy`.

This crate owns config loading, logging setup, runtime dispatch, and terminal rendering. It should not own command grammar, workflow semantics, session persistence, runner state, selector/summarizer behavior, Lua execution, storage, or agent protocol details.

| Module | Responsibility |
| --- | --- |
| `main.rs` | Shared binary entrypoint. Uses `cowboy-command-parser` for CLI parsing; the default command and `tui` subcommand launch the TUI, while other subcommands call `cowboy-workflow-engine::WorkflowRuntime`. |
| `lib.rs` | Public exports for the TUI app crate: config and `run_tui`. |
| `config.rs` | Load and validate TOML, including per-agent watchdog policy, materialize named config sets, and convert them into engine `RuntimeConfig`. |
| `app.rs` | Terminal startup, event loop, and top-level vertical layout only. |
| `app/commands.rs` | Slash command dispatch, runtime task spawning, help/status rendering, plain-text submission, and pending-prompt fallback. |
| `app/input.rs` | Keyboard handling, multiline input editing, history movement, scroll keys, and cancellation keys. |
| `app/history.rs` | TUI-owned persisted composer input history: locked append-only JSON-lines storage under `state_dir`. |
| `app/state.rs` | TUI state projection: active run, current step, pending prompt, transcript entries, command history, scroll offset, and background tasks. |
| `app/events.rs` | Converts typed workflow events into human-readable transcript text. |
| `app/styles.rs` | Shared ratatui colors/styles and width-safe truncation helpers. |
| `app/controls/header.rs` | Header view showing state, step, run, workflow, and task count. |
| `app/controls/transcript.rs` | Transcript view and waiting-for-input cards. |
| `app/controls/status.rs` | Status strip and context-sensitive hints. |
| `app/controls/composer.rs` | Composer view, multiline input rendering, cursor placement, and slash-command suggestions sourced from `cowboy-command-parser`. |

## Crate: `cowboy-command-parser` (`crates/tui/command-parser`)

Package name: `cowboy-command-parser`.

This crate owns clap-backed parsing for product CLI argv and interactive TUI slash commands. It exposes typed command enums, parse errors, generated command rows, and suggestion helpers. It must stay independent of `cowboy-workflow-engine`, ratatui, crossterm, tui-input, app state, and config loading.

| Module | Responsibility |
| --- | --- |
| `lib.rs` | `Cli`, `CliCommand`, shared `SharedCommand`, `SlashCommand`, `SlashParseError`, generated slash help/completion helpers, and quote/hash-preserving slash argv tokenization. |

## Crate: `cowboy-workflow-actions`

Package name: `cowboy-workflow-actions`.

Owns reusable host-action runners and the dispatcher that maps `StepAction` variants to `ActionResult` values.

| Module | Responsibility |
| --- | --- |
| `lib.rs` | `EngineActionDispatcher`, `ResumeCallbackRegistry`, and public runner exports. |
| `agent.rs` | `AgentActionRunner` adapter over `cowboy-workflow-agent::AgentExecutor`. |
| `command.rs` | `CommandActionRunner` for direct non-shell process execution from runtime cwd. |
| `ask_user.rs` | `AskUserActionRunner`, callback payload metadata, and resume handling into `StepRecord`. |
| `status.rs` | `StatusActionRunner` for immediate completed records. |
| `workflow.rs` | `WorkflowActionHandler` trait and `WorkflowActionRunner` adapter for `StepAction::Workflow`; routed to the engine's runtime handler. |
| `fail.rs` | `FailActionRunner` for failed run statuses. |

## Crate: `cowboy-workflow-engine`

Package name: `cowboy-workflow-engine`.

This is the product runtime between UI/CLI and lower-level workflow crates.

| Module | Responsibility |
| --- | --- |
| `runtime.rs` | `WorkflowRuntime`: start/resume/step/answer/improve/resolve/list workflow runs; resolve explicit/default config-set names before new-run persistence; resolve effective limits live from current config per operation (`resolve_limits`); wire store/catalog/Lua/action dispatch/agent execution; persist event logs. |
| `events.rs` | `WorkflowEvent`, `WorkflowEventKind`, and broadcast `EventBus`. |
| `input.rs` | `ResumeRouter`; validates answers for `RunStatus::WaitingForInput` and dispatches persisted resume callbacks. |
| `runner.rs` | `WorkflowRunner<S, D, P>` wrapper over `cowboy-workflow-core::execute_step`; emits visit-local retry events, durably reserves cumulative run/per-step retry budgets, and persists `Failed` on give-up. Also `LuaStepActionProvider`. |
| `workflow.rs` | Selector/summarizer adapters: deterministic selector, agent-backed selector, agent-backed summarizer. |
| `lib.rs` | Public runtime interface exported to UI/CLI and future frontends. |

Important seams:

- `WorkflowRuntime` is the high-level application interface.
- `WorkflowRunner<S, D, P>` depends on the async typed store capabilities it uses, plus `ActionDispatcher` and `StepActionProvider`.
- `LuaStepActionProvider` adapts `cowboy-workflow-lua::run_step` into `StepActionProvider` and delivers ask-user answers through `ctx.prev.fields.answer`.
- `ResumeRouter` does not mutate `WorkflowRun.resume`; it validates a waiting prompt answer and dispatches the stored resume callback for the common record-routing path.
- `AgentWorkflowSelector` and `AgentWorkflowSummarizer` depend only on `cowboy-agent-client::Client`.

Runner-policy contract: TOML uses `[config_sets.<name>]` with
`max_steps_per_run`, `max_visits_per_step`, `max_retries_per_run`, and
`max_retries_per_step`, independently defaulting to `100`, `20`, `200`, and
`2`. `default` always exists; retry limits may be zero; step/visit limits may
not. Lua selects a nonblank name through `workflow(..., { config_set = ... })`.
Only the resolved set **name** is durable; effective limits are resolved live
from current config on every lifecycle path, while cumulative retry counters
remain durable. Old top-level runner-limit keys are rejected.

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
| `definition.rs` | `WorkflowCatalog`, `WorkflowSourceRef`, `WorkflowDefinition` (including optional config-set selection), roles, steps, transitions, validation. |
| `action.rs` | Declarative `StepAction` variants: `agent`, `command`, `status`, `ask_user`, `workflow`, `fail`. |
| `state.rs` | Durable `WorkflowRun`, name-only config-set pointer (`ConfigSetRef`), retry counters, `RunStatus`, `ResumeCallback`, `StepRecord`, `StepOutput`, `RunHead`, `RoleSession`, object kinds. |
| `summary.rs` | `WorkflowSummary` and `WorkflowImprovement` used after a run. |
| `traits.rs` | Interfaces implemented by outer crates, including object-safe async `WorkflowStateStore`, `WorkflowObjectStore`, `AgentSessionStore`, `TurnStore`, `UserPromptStore`, `PromptWindowStore`, and composite `WorkflowStore`. |
| `engine.rs` | Serializable/defaulted `RunnerLimits`, `execute_step`, and step/visit budget enforcement. |
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

Owns the SQLx-backed SQLite implementation of the async `WorkflowStore`
capabilities. Schema bootstrap/version checks happen before the cloneable pool is
returned; writes use transactions and busy/locked retry with cancellation.

| Module | Responsibility |
| --- | --- |
| `sqlite_store.rs` | `SqliteWorkflowStore`; typed transactional state, object, session, turn, prompt, and prompt-window operations. |
| `schema.rs` | SQLite schema version 1 bootstrap, validation, WAL, and SQLx pool policy. |
| `contract.rs` | Reusable public-interface behavioral tests. |
| `hash.rs` | Canonical JSON object envelope and BLAKE3 object hashes. |
| `error.rs` | Store-specific errors mapped into core errors by the trait implementation. |
| `bin/store-cli.rs` | Async test app for saving/loading/deleting typed store objects and runs. |

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
| `client.rs` | ACP client implementing `cowboy-agent-client::Client`, including inactivity reset, `session/cancel`, same-session `"Continue"`, and bounded restart/resume recovery. |
| `messages.rs` | ACP JSON-RPC message types and parser. |
| `transport/` | stdio and Zellij line transports, including recorded-PID force termination before replacement startup with `--resume=<session-id>`. |
| `bin/acp-chat.rs` | Test app for chatting with an ACP agent. |
| `bin/watchdog-fixture.rs` | Deterministic soft/hard watchdog smoke fixture and authenticated cleanup verifier. |

<!-- cowboy-agent-watchdog-contract:start -->
```toml
[agents.watchdog]
response_timeout_seconds = 100
cancel_timeout_seconds = 10
recovery_operation_timeout_seconds = 30
```

Parsed ACP activity resets the inactivity deadline. Recovery first sends exactly
one `session/cancel` and, when cancellation is confirmed, sends `"Continue"` on
the same session. If cancellation fails or times out, Cowboy kills the recorded
PID, waits for exit, restarts the agent with `--resume=<session-id>`, initializes
ACP, and sends `"Continue"`. The recovery-operation timeout separately bounds
termination, restart, initialization, and continuation dispatch. This ACP
recovery does not consume workflow retry budgets. All values must be greater
than zero, and Cowboy must be restarted after watchdog configuration changes.
<!-- cowboy-agent-watchdog-contract:end -->

## Current flow

```text
CLI/TUI command
  -> cowboy-workflow-engine WorkflowRuntime
  -> catalog chooses/loads workflow source
  -> workflow-lua compiles/snapshots workflow source
  -> engine resolves workflow config_set name (or default); limits resolved live per operation
  -> WorkflowRun persisted through async WorkflowStore capabilities
  -> WorkflowRunner loops execute_step
  -> LuaStepActionProvider returns StepAction
  -> ActionDispatcher/action runners handle initial StepAction values
  -> ResumeRouter dispatches waiting answers through ResumeCallbackRegistry
  -> WorkflowStore transactions save run/head/objects
  -> EventBus emits WorkflowEvent
  -> TUI renders events or CLI prints report
```

## Refactoring guidance

- Keep `crates/tui/app` as config/runtime-dispatch/UI only and `crates/tui/command-parser` as runtime/UI-independent command grammar.
- Keep application runtime orchestration in `cowboy-workflow-engine`.
- Keep catalog policy in `cowboy-workflow-catalog`.
- Keep workflow semantics in `cowboy-workflow-core`.
- Keep Lua VM setup and import policy in `cowboy-workflow-lua`.
- Keep backend session management in `cowboy-workflow-agent`.
- Do not reintroduce the old hardcoded `pipeline`/`SubTask` model into the TUI crate.
