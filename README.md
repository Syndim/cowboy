# Cowboy

Cowboy is a workflow-first terminal AI agent orchestrator for ACP-compatible coding agents.

It has one binary with two interfaces:

- `cowboy` launches the interactive terminal UI.
- `cowboy <subcommand>` runs non-interactive CLI commands against the same workflow runtime and persisted state.

Workflows are Lua files. A workflow step can run an agent, return a status, ask the user for input, fail, or suspend. Runs, step outputs, agent turns, role sessions, source snapshots, and event logs are persisted so the TUI and CLI see the same state.

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

Because the repository is private for now, the command requires credentials with access to `https://github.com/syndim/cowboy.git`. After the repository becomes public, the same command will work without private-repository access.

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

Start a workflow run from the CLI:

```bash
cowboy run add a /healthz route
```

List existing runs:

```bash
cowboy runs
```

Execute one additional step for a run:

```bash
cowboy step <run-id>
```

Answer an `ask_user` prompt:

```bash
cowboy answer <run-id> <prompt-id> <answer>
```

Ask Cowboy to summarize and apply workflow-file improvements from a completed run:

```bash
cowboy improve <run-id>
```

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
/run-step <request>                    run only the first workflow step
/step <run-id>                         execute exactly one more step
/answer <run-id> <prompt-id> <answer>  answer a waiting prompt explicitly
/runs                                  list workflow runs
/workflows                             list known workflows
/improve <run-id>                      improve workflow source from a run
/cancel                                cancel active background tasks
/help                                  show built-in commands
/exit                                  quit Cowboy
```

### TUI keys

| Key | Action |
| --- | --- |
| `Enter` | Submit current input. |
| `Shift+Enter` / `Ctrl+Enter` | Insert a newline in the input. |
| `Tab` | Complete the first slash-command suggestion. |
| `↑` / `↓` | Browse command history. |
| `Ctrl+U` / `Ctrl+D` | Scroll the transcript. |
| `End` | Follow the latest transcript entry. |
| `Ctrl+C` | Quit Cowboy. |
| `Esc` | Cancel active background tasks. |
| `Backspace` | Delete one input character. |

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
logs/cowboy.log                  # diagnostic log
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
  -> WorkflowRunner executes steps until completed/failed/suspended/waiting
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

