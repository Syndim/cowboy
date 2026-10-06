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

The shared `cowboy export <run-id>` CLI command and `/export <run-id>` TUI
command write `cowboy-export-<safe-run-id>.html` to the runtime working
directory. The standalone document contains cards collapsed by default,
per-card and global expansion controls, and case-insensitive search across
complete card headers and bodies without external resources.

| Module | Responsibility |
| --- | --- |
| `main.rs` | Shared binary entrypoint. Uses `cowboy-command-parser` for CLI parsing; the default command and `tui` subcommand launch the TUI, while other subcommands call `cowboy-workflow-engine::WorkflowRuntime`. Runs on an explicit Tokio runtime whose final teardown is bounded, so returning from `main` always terminates the process. |
| `lib.rs` | Public exports for the TUI app crate: config, `run_tui`, the bounded process-exit helper, and the application-level run export entry point. |
| `process_exit.rs` | `run_with_bounded_shutdown`: builds a multi-thread Tokio runtime, `block_on`s the work so persistence completes, then bounds runtime teardown with `Runtime::shutdown_timeout`. |
| `config.rs` | Load and validate TOML, including per-agent watchdog policy, materialize named config sets, and convert them into engine `RuntimeConfig`. |
| `export.rs` | Load a canonical run and persisted event log, replay projected cards, and atomically replace a sanitized standalone searchable HTML export in the runtime cwd. |
| `app.rs` | Terminal startup, event loop, and top-level vertical layout only. Awaits `WorkflowRuntime::shutdown` after the loop returns and before restoring the terminal. |
| `app/commands.rs` | Slash command dispatch, runtime task spawning, help/status rendering, plain-text submission, and pending-prompt fallback. |
| `app/input.rs` | Keyboard handling, multiline input editing, history movement, scroll keys, and cancellation keys. |
| `app/history.rs` | TUI-owned persisted composer input history: locked append-only JSON-lines storage under `state_dir`. |
| `app/state.rs` | TUI state projection plus reusable workflow-event coalescing and descriptor snapshots for live ingestion and persisted replay. |
| `app/events.rs` | Converts typed workflow events into shared untruncated semantic cards and terminal text. |
| `app/card.rs` | Semantic card headers/sections plus width-aware ratatui borders, wrapping, and styling. |
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
| `wait_for_input.rs` | `WaitForInputActionRunner`, callback payload metadata, and resume handling into `StepRecord`. |
| `status.rs` | `StatusActionRunner` for immediate completed records. |
| `workflow.rs` | `WorkflowActionHandler` trait and `WorkflowActionRunner` adapter for `StepAction::Workflow`; routed to the engine's runtime handler. |
| `fail.rs` | `FailActionRunner` for failed run statuses. |

## Crate: `cowboy-workflow-engine`

Package name: `cowboy-workflow-engine`.

This is the product runtime between UI/CLI and lower-level workflow crates.

| Module | Responsibility |
| --- | --- |
| `runtime.rs` | `WorkflowRuntime`: start/resume/step/provide-input/improve/resolve/list runs; live config-set policy; current-generation input CAS, terminal unsafe reconciliation of interrupted top-level and child agents, session-bound continuation, events and bounded shutdown. |
| `agent_input.rs` | Durable `agent_human_input` callback: safe failure category, original action/role, run/step/visit/head/generation binding and bounded answer validation. |
| `native_ownership.rs` | Adapter from frozen agent dispatch and native ACP shutdown receipt into the SQLite owner-generation ledger. |
| `preflight.rs` | Optional snapshot-bound read/verify hook before consuming an opted-in agent wait. |
| `events.rs` | `WorkflowEvent`, `WorkflowEventKind`, and broadcast `EventBus`. |
| `input.rs` | `ResumeRouter`; validates supplied input for `RunStatus::WaitingForInput` and dispatches persisted resume callbacks. |
| `runner.rs` | `WorkflowRunner<S, D, P>` over `execute_step`: disabled retry budgets, enabled first-failure agent waits without automatic retry, original-action capture, same-step continuation and event projection. |
| `workflow.rs` | Selector/summarizer adapters: deterministic selector, agent-backed selector, agent-backed summarizer. |
| `lib.rs` | Public runtime interface exported to UI/CLI and future frontends. |

Important seams:

- `WorkflowRuntime` is the high-level application interface.
- `WorkflowRunner<S, D, P>` depends on the async typed store capabilities it uses, plus `ActionDispatcher` and `StepActionProvider`.
- `LuaStepActionProvider` adapts `cowboy-workflow-lua::run_step` into `StepActionProvider` and delivers wait-for-input inputs through `ctx.prev.fields.input`.
- `ResumeRouter` validates a waiting prompt input and dispatches the stored resume callback for the common record-routing path without mutating step counters.
- `AgentWorkflowSelector` and `AgentWorkflowSummarizer` depend only on `cowboy-agent-client::Client`.

Runner-policy contract: TOML uses `[config_sets.<name>]` with
`max_steps_per_run`, `max_visits_per_step`, `max_retries_per_run`, and
`max_retries_per_step`, defaulting independently to `100`, `20`, `200`, and
`2`. `agent_human_input` independently defaults to `false` and changes only
opted-in agent steps. Named sets do not inherit the default set's boolean;
only the selected set **name** is durable and deleted sets fall back to live
`default`, then built-in policy. Retry counters remain durable. Old top-level
runner-limit keys remain rejected.

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
| `definition.rs` | `WorkflowCatalog`, `WorkflowSource`, `WorkflowLocation`, `WorkflowDefinition` (including optional config-set selection), roles, steps, transitions, validation. |
| `action.rs` | Declarative `StepAction` variants: `agent`, `command`, `status`, `wait_for_input`, `workflow`, `fail`, including legacy agent prompts and structured agent task contracts with stable keys, recovery context, and minimal turns. |
| `state.rs` | Durable `Run` agent continuation checkpoint and unsafe-recovery fence, config-set pointer, retry counters, `RunStatus`, `ResumeCallback`, `StepRecord`, `StepOutput`, `RunHead`, `RoleSession` backend identity plus independent role/static-task/current-action delivery evidence. |
| `summary.rs` | `WorkflowSummary` and `WorkflowImprovement` used after a run. |
| `traits.rs` | Interfaces implemented by outer crates, including object-safe async `WorkflowStateStore`, `WorkflowObjectStore`, `AgentSessionStore`, `TurnStore`, `UserPromptStore`, `PromptWindowStore`, and composite `WorkflowStore`. |
| `engine.rs` | `RunnerLimits`, `execute_step`, continuation of the saved agent action with transactional completion, `next_step` routing and step/visit budget enforcement. |
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
| `sqlite_store.rs` | `SqliteWorkflowStore`; transactional run/head, object, session, turn, prompt and prompt-window operations; compare-and-save run generation for stale human-input rejection. |
| `schema.rs` | SQLite schema version 2 bootstrap, transactional v1 upgrade, validation, WAL, and SQLx pool policy. |
| `native_ownership.rs` | Versioned private writer/attempt ledger, session and immutable action linkage, and read-only redacted shutdown projection. |
| `contract.rs` | Reusable public-interface behavioral tests. |
| `hash.rs` | Canonical JSON object envelope and BLAKE3 object hashes. |
| `error.rs` | Store-specific errors mapped into core errors by the trait implementation. |
| `bin/store-cli.rs` | Async test app for saving/loading/deleting typed store objects and runs. |

## Crate: `cowboy-workflow-agent`

Owns execution of `StepAction::Agent`.

| Module | Responsibility |
| --- | --- |
| `executor.rs` | `AgentExecutor`, per-run-policy `ClientFactory`, native per-role/session reuse, backend identity/load guards, prompt-seal current-action fingerprints for raw task/new turn, and verified client cleanup. |
| `prompt.rs` | Composes role, task, recovery, turn, user-input, deliverable, and retry blocks; fingerprints static task instructions plus the output specification while excluding recovery and turn deltas. |
| `frontmatter.rs` | Parses YAML frontmatter + Markdown body into normalized `StepOutput`. |
| `error.rs` | Agent execution errors. |
| `bin/execute-agent.rs` | Test app for executing one agent step through an ACP command. |

## Crate: `cowboy-agent-client`

Provider-neutral seam between Cowboy and agent backends.

| Module | Responsibility |
| --- | --- |
| `traits.rs` | `Client` trait: session create/load, prompt, close, and explicit verified termination/policy failure gates for human-input recovery. |
| `types.rs` | `ModelInfo`, `AgentInfo`, `PromptContent`, `Event`, `StopReason`. |

## Crate: `cowboy-agent-acp`

ACP backend implementation.

| Module | Responsibility |
| --- | --- |
| `client.rs` | ACP client implementing the provider-neutral `Client`, with unchanged disabled watchdog/`Continue` behavior and per-run enabled bounded failures without automatic continuation/replacement. |
| `messages.rs` | ACP JSON-RPC message types and parser. |
| `process_tree.rs` | `ProcessTreeScope`: platform ownership of an agent's whole process tree (Windows Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; Unix `process_group(0)` + `killpg`), configured before spawn, attached after spawn, terminated idempotently and on drop. |
| `agent_processes.rs` | Process-wide registry of live agent process trees, exposed as `terminate_all_agent_processes(timeout)` for bounded shutdown. |
| `transport/` | stdio and Zellij ACP transports. Stdio preserves legacy direct-child kill after failed group termination; opted-in human waits require separate verified scoped group termination and direct-child reap/absence proof before releasing a run. |
| `bin/acp-chat.rs` | Test app for chatting with an ACP agent. |
| `bin/watchdog-fixture.rs` | Deterministic soft/hard watchdog smoke fixture and authenticated cleanup verifier. |

<!-- cowboy-agent-watchdog-contract:start -->
```toml
[agents.watchdog]
response_timeout_seconds = 100
cancel_timeout_seconds = 10
recovery_operation_timeout_seconds = 30
```

Parsed ACP activity resets the inactivity deadline. An in-flight ACP tool call
restarts the inactivity watchdog instead of triggering recovery, and that
restart is unbounded because deciding a tool call is stuck and aborting it is
the agent's responsibility rather than Cowboy's. Recovery first sends exactly
one `session/cancel` and, when the agent acknowledges it with any stop reason,
including `end_turn`, sends `"Continue"` on the same session. If cancellation
fails or times out, Cowboy kills the recorded PID, waits for exit, restarts the
agent with `--resume=<session-id>`, initializes ACP, and sends `"Continue"`.
The recovery-operation timeout separately bounds termination, restart,
initialization, and continuation dispatch.
This ACP recovery does not consume workflow retry budgets. All values must be
greater than zero, and Cowboy must be restarted after watchdog configuration
changes.
<!-- cowboy-agent-watchdog-contract:end -->

## Current flow

```text
CLI/TUI command
  -> cowboy-workflow-engine WorkflowRuntime
  -> catalog chooses/loads workflow source
  -> workflow-lua compiles/snapshots workflow source
  -> engine resolves workflow config_set name (or default); limits resolved live per operation
  -> Run persisted through async WorkflowStore capabilities
  -> WorkflowRunner loops execute_step
  -> LuaStepActionProvider returns StepAction
  -> ActionDispatcher/action runners handle initial StepAction values
  -> ResumeRouter dispatches waiting inputs through ResumeCallbackRegistry
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
