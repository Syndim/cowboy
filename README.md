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
- Async SQLx/SQLite workflow store with content-addressed source, step, and turn objects.
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
cargo run -- runs [partial-run-id]     # list workflow runs, optionally filtered by id
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

List existing runs, optionally filtering by a literal partial run id:

```bash
cowboy runs [partial-run-id]
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
recoverable-retry budget; its failed step stays current. `cowboy resume` and
`cowboy step` retry that retained current step (`resume` continues until the run
blocks, fails, or completes; `step` takes one fresh attempt), which grants one
fresh initial attempt that can succeed or deterministically re-fail if the step
budget is still exhausted. Use `cowboy resolve` instead to force a manual status
rather than retrying the failed work. Without a `<status>`, this lists the
statuses the failed step can be resolved to along with the fields each requires:

```bash
cowboy resolve <run-id>
cowboy resolve <run-id> <status> [--field <name> <value>]... [--body <text>]
```

Repeat `--field` for each output field. Field names are exact and may include
spaces, `=`, or a leading `-`; quote them when needed. Ordinary values are
strings, while valid JSON literals retain their types:

```bash
cowboy resolve <run-id> planned --field summary "manual resolution" --field retry false --field files '["src/a.rs"]'
```

Recoverable step failures (for example, an agent reply missing its YAML
frontmatter, or a transient backend error) consume both the run-wide
`max_retries_per_run` budget and the current step id's cumulative
`max_retries_per_step` budget. Initial attempts do not count as retries, and
retries do not consume step or visit budgets.

## TUI

![Cowboy TUI snapshot](docs/assets/tui-snapshot.svg)

Plain text submitted in the composer starts a workflow run. When a workflow is waiting for input, typing the answer directly submits it to the pending prompt; `/answer` remains available for explicit answers.

### TUI commands

```text
/run [--step] [--workflow <workflow-id>] <request>  start a workflow run
/step <run-id>                                    execute exactly one more step
/resume <run-id>                                  continue a run until blocked
/answer <run-id> <prompt-id> <answer>             answer a waiting prompt explicitly
/runs [partial-run-id]                         list workflow runs
/workflows                                        list known workflows
/improve <run-id>                                 improve workflow source from a run
/resolve <run-id>                                 list statuses a failed run can resolve to
/resolve <run-id> <status> [--field <name> <value>]... [--body <text>]  resolve a failed step and continue the run
/cancel                                           cancel active background tasks
/help                                             show built-in commands
/exit                                             quit Cowboy
```

`step` advances exactly one workflow step. `resume` keeps executing a running workflow until it waits for input, fails, suspends, or completes. Both also re-execute the retained current step of any non-terminal run — `Running`, `Failed` (for example one that gave up after exhausting its recoverable-retry budget), and `WaitingForInput`: `step` takes one fresh attempt and `resume` continues until the run blocks, fails, or completes. Re-executing a `WaitingForInput` run re-prompts its retained `ask_user` step and safely replaces the durable pending callback. Only `Completed` and `Cancelled` runs are non-resumable no-ops and left unchanged; `answer` remains the way to supply a prompt answer.

`/run --workflow <workflow-id> <request>` uses the catalog workflow id shown by `/workflows`, not necessarily the name declared inside a Lua workflow file.

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
- workflow store: `${XDG_STATE_HOME:-~/.local/state}/cowboy/data.db`
- agent command: `copilot --acp`
- user workflows: `${XDG_CONFIG_HOME:-~/.config}/cowboy/workflows`

Example config:

```toml
state_dir = "~/.local/state/cowboy"
workflow_store = "~/.local/state/cowboy/data.db"
workflow_dirs = [".cowboy/workflows", "~/.config/cowboy/workflows"]
mouse_scroll_lines = 3

[config_sets.default]
max_steps_per_run = 100
max_visits_per_step = 20
max_retries_per_run = 200
max_retries_per_step = 2

[config_sets.careful]
# Omitted fields independently inherit 100, 20, 200, and 2.
max_retries_per_run = 20
max_retries_per_step = 4

[[agents]]
name = "default"
command = "copilot"
args = ["--acp"]

[agents.model]
id = "opus-4.8-1m"
provider = "github-copilot"

[[agents]]
name = "planner"
command = "omp"
args = ["--model=github-copilot/claude-opus-4.8", "--thinking=xhigh", "acp"]

[[agents]]
name = "investigator"
command = "omp"
args = ["--model=github-copilot/claude-opus-4.8", "--thinking=xhigh", "acp"]

[[agents]]
name = "reviewer"
command = "omp"
args = ["--model=github-copilot/gpt-5.6-sol", "--thinking=high", "acp"]

[[agents]]
name = "implementer"
command = "omp"
args = ["--model=github-copilot/claude-opus-4.8", "--thinking=medium", "acp"]

[[agents]]
name = "tester"
command = "omp"
args = ["--model=github-copilot/claude-sonnet-5", "--thinking=medium", "acp"]

[[agents]]
name = "committer"
command = "omp"
args = ["--model=github-copilot/claude-haiku-4.5", "--thinking=low", "acp"]
```

`agents.model` is optional. When omitted, Cowboy does not send a model hint or
change the model through ACP; the backend's launch arguments or own default stay
authoritative. When provided, Cowboy selects that model through ACP session
configuration and fails if the agent does not offer it.

The workflow roles select dedicated `planner`, `investigator`, `reviewer`,
`implementer`, `tester`, and `committer` agents. Their backend launch arguments
set thinking to `xhigh`, `xhigh`, `high`, `medium`, `medium`, and `low`, respectively;
`[agents.model]` does not control it.

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

Every config-set field is optional and defaults independently to the values
shown above. The built-in `default` set always exists, even when the file only
declares custom sets. Set either retry limit to `0` to disable that retry
scope; `max_steps_per_run` and `max_visits_per_step` must be greater than zero.
Blank set names and unknown fields are rejected.

`mouse_scroll_lines` controls how many transcript visual rows one mouse-wheel
detent scrolls in the TUI. It defaults to `3` and must be greater than zero.


Workflows select a set with
`workflow(name, head, { config_set = "careful" })`; omission selects `default`.
An unknown selection fails before the new run is persisted. A run persists only
its selected config-set **name**; effective limits are resolved from current
config on every operation. So resuming or stepping an existing run after a
config edit — including a raised retry budget — applies the current limits, and
a config-set that was deleted falls back to `default` limits (a warning is
logged, and if `default` is also gone the built-in defaults apply). Retry
counters are durable and cumulative across visits to the same step id, so a
raised limit adds budget without resetting accounting. Retry events retain
visit-local attempt numbers (`2..=max_attempts`) and use one fixed
`max_attempts` for that visit.

A long-lived TUI still loads config once per process, so **new** runs pick up
config edits only after a restart.

SQLite persistence is a clean cutover with no automatic conversion of an old
store file. Preserve the old file and choose a new SQLite `workflow_store` path,
or stop all Cowboy processes before clearing the configured path. The default is
`${XDG_STATE_HOME:-~/.local/state}/cowboy/data.db`, which may be configured
outside `state_dir`. Event logs remain under `<state_dir>/events`.

This is a clean cutover: old top-level `max_steps_per_run`,
`max_visits_per_step`, and `max_retries_per_step` keys are rejected. Move them
under `[config_sets.default]`.

Any ACP-compatible coding-agent CLI can be used by changing an `[[agents]]` entry's `command` and `args`. Roles may select a named agent with `agent = "name"`.

## Workflows

Cowboy always includes a built-in default developer workflow. Custom workflows are optional and live as `.lua` files under configured `workflow_dirs`.

Copy starter workflows from `examples/workflows` into a configured workflow directory:

```bash
mkdir -p ~/.config/cowboy/workflows
cp -R examples/workflows/* ~/.config/cowboy/workflows/
```

The starter set includes `feature`, `bugfix`, and `dev-loop`. Plans give every
implementation task a stable `TODO-NN` identifier, exact subject text,
reproducible procedure, and observable expected result. Each source emits exactly
one typed evidence record per subject; a record contains the complete ordered
procedure, while command records map individual command steps by procedure index.
Implementers attach evidence to each completed TODO, and testers independently
reproduce the same procedure.

Review is globally gated and runs in two passes. First, reviewers durably assess
every proof for relevance, sufficiency, safety, currentness, falsifiability, and
non-circularity, and separately validate the submitted evidence. No reviewer
command runs until every subject has a sound proof and valid submission. Only
then does the reviewer independently reproduce every procedure and compare the
observations before approval.

The `dev-loop` workflow treats the run request as the Goal, asks the user for the
exact validation method, and creates a per-request
`docs/plans/<snake_case_summary>/` folder containing the implementation plan at
`plan.md` and the validation guide at `validation.md`. Validation steps and exit
criteria use stable `VAL-NN` identifiers; validators record one criterion-keyed
evidence record, and reviewers assess and reproduce those criteria through the
same two-pass gate. The validator must complete the guide before the loop can
finish.

Across all three starter workflows, blocked agent steps first go to a dedicated
blocker reviewer; recoverable blockers return to the originating step with
agent-side recovery instructions, and only blockers requiring external input
prompt the user. After copying the examples, start dev-loop with
`cowboy run --workflow workflows/dev-loop <goal>`.

Read [Workflow authoring](docs/workflow-authoring.md) for the Lua API, runtime context, step actions, transitions, imports, examples, and debugging tools.

## Persistence and logs

Cowboy stores runtime state under `state_dir`:

```text
data.db                          # SQLite runs, heads, objects, turns, sessions, and prompts
data.db.locks/<run-id>.lock      # sidecar same-run execution guards
events/<run-id>.json             # persisted workflow event log for display/debugging
logs/cowboy.<YYYY-MM-DD>.<pid>.log  # diagnostic log per process and UTC date
```

The store uses SQLx's Tokio SQLite driver, schema versioning, WAL, and a small
connection pool. Mutable domain operations use transactions: saving a run also
updates its derived head, and completing a step stores the record and advances
the run/head atomically. Busy/locked writes retry asynchronously with cancellable
wait notifications; sidecar locks still prevent two Cowboy processes from
advancing the same run.

Logging defaults to `info`. Set `COWBOY_LOG` or `RUST_LOG` for more detail, for example:

```bash
COWBOY_LOG="info,cowboy_agent_acp=debug" cowboy
```

## How it works

```text
CLI/TUI request
  -> workflow catalog selects a Lua workflow
  -> Lua source is snapshotted and compiled into a WorkflowDefinition
  -> engine records the selected config_set name (limits resolved live per operation)
  -> WorkflowRun is persisted through async WorkflowStore capabilities in SQLite
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
