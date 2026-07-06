# Cowboy

Cowboy is a workflow-first terminal AI agent orchestrator for ACP-compatible coding agents.

It has one binary with two interfaces:

- `cowboy` launches the interactive terminal UI.
- `cowboy <subcommand>` runs non-interactive CLI commands against the same workflow runtime and persisted state.

Workflows are Lua files. A workflow step can run an agent, return a status, request input, or fail. Waiting input stores a durable resume descriptor so answers can continue through the same runtime path as other actions. Runs, step outputs, agent turns, role sessions, source snapshots, and event logs are persisted so the TUI and CLI see the same state.

## Project status

Cowboy is an early developer-preview project.

Current capabilities:

- Lua-authored workflow graphs with status-based transitions.
- Built-in default developer workflow plus user/project workflow directories.
- ACP-backed agent execution through commands such as `copilot --acp`, `claude --acp`, or `omp acp`.
- Reused backend agent sessions per `(run_id, role_id)`.
- Redb-backed run store with content-addressed source, step, and turn objects.
- TUI event transcript for workflow lifecycle, prompts, agent thinking/responses, and tool calls.

## Requirements

- Rust `1.85` or newer.
- An authenticated ACP-compatible coding-agent CLI on `PATH`.
- Cargo and Git access to this repository.

## Install

Install from GitHub:

```bash
cargo install --git https://github.com/syndim/cowboy.git cowboy
```

For local development:

```bash
cargo build
cargo run                              # launch TUI
cargo run -- run add a /healthz route  # start a workflow from CLI
cargo run -- runs                      # list workflow runs
```

## Quick start

Start the TUI:

```bash
cowboy
```

Start a workflow run from the CLI. Add `--workflow <workflow-id>` to bypass
agent-backed selection and run the catalog id shown by `/workflows` or other
catalog listings.

```bash
cowboy run add a /healthz route
cowboy run --workflow <workflow-id> add a /healthz route
```

List existing runs:

```bash
cowboy runs
```

Execute one additional step for a run, or continue it until it blocks, fails, or completes:

```bash
cowboy step <run-id>
cowboy resume <run-id>
```

Answer a waiting prompt:

```bash
cowboy answer <run-id> <prompt-id> <answer>
```

Ask Cowboy to summarize and apply workflow-file improvements from a completed run:

```bash
cowboy improve <run-id>
```

Resolve a failed run. A run gives up as `Failed` only after exhausting the
recoverable-retry budget; its failed step stays current so it can be resolved
manually. Without a `<status>`, this lists the statuses the failed step can be
resolved to along with the fields each requires:

```bash
cowboy resolve <run-id>
cowboy resolve <run-id> <status> [--fields '<json object>'] [--body <text>]
```

Recoverable step failures (for example, an agent reply missing its YAML
frontmatter, or a transient backend error) are retried automatically up to
`max_retries_per_step` times before the run is marked `Failed`.

## TUI views

The TUI is organized into four vertical views:

| View | Purpose |
| --- | --- |
| Header | Shows Cowboy state, active step, short run id, workflow name, and background task count when space allows. |
| Transcript | Shows the workflow event stream: run/step lifecycle, prompt sent to the agent, agent thinking, agent responses, tool calls, tool updates, step output, waiting-for-input cards, failures, suspensions, and completion. |
| Status strip | Shows current state plus context-sensitive hints such as scroll keys, pending prompt handling, background tasks, and cancellation. |
| Composer | Accepts plain workflow requests, slash commands, and prompt answers. It supports multiline input and slash-command suggestions. |

Plain text submitted in the composer starts a workflow run. When a workflow is waiting for input, typing the answer directly submits it to the pending prompt; `/answer` remains available for explicit answers.

### TUI commands

```text
/run <request>                         start a workflow run
/run-workflow <workflow-id> <request>  start a catalog workflow id directly
/run-step <request>                    run only the first workflow step
/step <run-id>                         execute exactly one more step
/resume [run-id]                       continue a run until blocked
/answer <run-id> <prompt-id> <answer>  answer a waiting prompt explicitly
/runs                                  list workflow runs
/workflows                             list known workflows
/improve <run-id>                      improve workflow source from a run
/resolve <run-id>                      list statuses a failed run can resolve to
/resolve <run-id> <status>             resolve a failed step and continue the run
/cancel                                cancel active background tasks
/help                                  show built-in commands
/exit                                  quit Cowboy
```

`step` advances exactly one workflow step. `resume` keeps executing a running workflow until it waits for input, fails, suspends, or completes.

`/run-workflow` uses the catalog workflow id shown by `/workflows`, not necessarily the name declared inside a Lua workflow file.

### TUI keys

| Key | Action |
| --- | --- |
| `Enter` | Submit current input. |
| `Shift+Enter` / `Ctrl+Enter` | Insert a newline in the input. |
| `Tab` | Complete the first slash-command suggestion. |
| `↑` / `↓` | Browse command history. |
| `←` / `→` | Move the input cursor. |
| `Ctrl+←` / `Ctrl+→` | Move the input cursor by word. |
| `Ctrl+U` / `Ctrl+D` | Scroll the transcript. |
| `End` | Follow the latest transcript entry. |
| `Ctrl+C` | Quit Cowboy. |
| `Esc` | Cancel active background tasks. |
| `Backspace` / `Delete` | Delete before or at the input cursor. |

## Configuration

Default config path:

```text
${XDG_CONFIG_HOME:-~/.config}/cowboy/config.toml
```

If no config exists, Cowboy uses defaults:

- state dir: `${XDG_STATE_HOME:-~/.local/state}/cowboy`
- workflow store: `${XDG_STATE_HOME:-~/.local/state}/cowboy/workflow.redb`
- agent command: `copilot --acp`
- user workflows: `${XDG_CONFIG_HOME:-~/.config}/cowboy/workflows`

Example config:

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

Any ACP-compatible coding-agent CLI can be used by changing an `[[agents]]` entry's `command` and `args`. Roles may select a named agent with `agent = "name"`.

## Workflows

Cowboy always includes a built-in default developer workflow. Custom workflows are optional and live as `.lua` files under configured `workflow_dirs`.

Copy starter workflows from `examples/workflows` into a configured workflow directory:

```bash
mkdir -p ~/.config/cowboy/workflows
cp -R examples/workflows/* ~/.config/cowboy/workflows/
```

Read [Workflow authoring](docs/workflow-authoring.md) for the Lua API, runtime context, step actions, transitions, imports, examples, and debugging tools.

## Persistence and logs

Cowboy stores runtime state under `state_dir`:

```text
workflow.redb                    # runs, heads, immutable source/step/turn objects, role sessions
events/<run-id>.json             # persisted workflow event log for display/debugging
logs/cowboy.<YYYY-MM-DD>.<pid>.log  # diagnostic log per process and UTC date
```

Logging defaults to `info`. Set `COWBOY_LOG` or `RUST_LOG` for more detail, for example:

```bash
COWBOY_LOG="info,cowboy_agent_acp=debug" cowboy
```

## How it works

```text
CLI/TUI request
  -> workflow catalog selects a Lua workflow
  -> Lua source is snapshotted and compiled into a WorkflowDefinition
  -> WorkflowRun is persisted through redb
  -> WorkflowRunner executes steps until completed/failed/waiting
  -> agent steps go through ACP and parse YAML-frontmatter output
  -> workflow events are emitted and persisted for UI/CLI display
```

## Development

Run tests:

```bash
cargo test
```

Build diagnostic test apps:

```bash
just test-apps
```

The helper binaries are copied into `target/debug/test-apps`:

- `workflow-chart`
- `store-cli`
- `execute-agent`
- `acp-chat`
- `catalog-cli`
- `engine-cli`

Live ACP integration tests require an authenticated backend CLI:

```bash
just acp-test copilot
just acp-test omp
```

## Documentation

- [Workflow authoring](docs/workflow-authoring.md) — Lua workflow authoring reference.
- [Architecture](docs/architecture.md) — Runtime model and data flow.
- [Module map](docs/module-map.md) — Workspace crate responsibilities and seams.
- [AGENTS.md](AGENTS.md) — Repository guide for AI coding agents.

## Inspiration

Cowboy is inspired by [United Workforce](https://github.com/shazhou-ww/united-workforce), a stateless workflow engine for multi-agent orchestration.

## License

Cowboy is licensed under the [MIT License](LICENSE).

