# Plan

Implement partial run-id filtering for the shared `runs` command so both product CLI `cowboy runs <partial-run-id>` and TUI slash command `/runs <partial-run-id>` list only runs whose persisted run id contains the supplied substring. Preserve current unfiltered behavior for `cowboy runs` and `/runs` with no argument.

Use the existing ownership boundaries:

- `cowboy-command-parser` owns the shared command grammar and generated slash help/suggestions.
- `cowboy-workflow-engine::WorkflowRuntime` owns run-list projection from persisted run heads into `RunSummaryLine` values.
- `crates/tui/app` owns CLI/TUI dispatch and transcript rendering only.

The filter should be literal, case-sensitive, and applied to the run id only. Do not filter by workflow name, status, topic, current step, or record id. Do not add regex, fuzzy matching, persistence changes, or a new store API.

# Changes

- In `crates/tui/command-parser/src/lib.rs`, replace the unit `SharedCommand::Runs` variant with a typed args variant, for example `Runs(RunsArgs)`, where `RunsArgs` has one optional positional `partial_run_id: Option<String>` value named `partial-run-id` in help/usage.
- Update shared CLI and slash parsing tests so `cowboy runs`, `cowboy runs abc`, `/runs`, and `/runs abc` all parse through the same `SharedCommand::Runs` value shape.
- In `crates/workflow/engine/src/runtime.rs`, change `WorkflowRuntime::list_runs` to accept an optional partial run-id filter, apply it with `run_id.contains(partial)` before loading the full run, and keep existing summary projection unchanged for matching runs.
- Update all current `runtime.list_runs()` callsites in product code, test apps, and tests to pass `None` for unfiltered listing or `Some(partial.as_str())` for filtered listing.
- In `crates/tui/app/src/main.rs`, pass the parsed filter from `cowboy runs <partial-run-id>` into the runtime and render the existing `render_run_summary_lines` output for returned summaries.
- In `crates/tui/app/src/app/commands.rs` and `crates/tui/app/src/app/state.rs`, pass the parsed `/runs <partial-run-id>` filter through the background task and preserve the current non-blocking behavior. For a filtered empty result, render an explicit empty-state card such as `matching runs for <partial-run-id>: 0`; for an unfiltered empty result, preserve `known runs: 0`.
- Update user-facing command references in `README.md` so CLI and TUI examples show the new optional filter; do not change runtime semantics in docs.

# Tests to be added/updated

- Add command-parser unit coverage in `crates/tui/command-parser/src/lib.rs` for optional runs filter parsing:
  - `cowboy runs` and `/runs` parse to `Runs` with `partial_run_id == None`.
  - `cowboy runs abc` and `/runs abc` parse to `Runs` with `partial_run_id == Some("abc")`.
  - Generated slash usage advertises `/runs [partial-run-id]`.
- Add workflow-engine unit coverage in `crates/workflow/engine/src/runtime.rs` that seeds multiple run heads and asserts `list_runs(Some("wait"))` returns only matching run ids while `list_runs(None)` returns all seed runs.
- Update existing `list_runs` runtime tests in `crates/workflow/engine/src/runtime.rs` to pass `None` and preserve current unfiltered expectations.
- Add or update TUI command tests in `crates/tui/app/src/app/commands.rs` so `/runs <partial-run-id>` stays a background task, renders only matching run cards, and does not leak nonmatching run ids into matching cards or filtered empty-state cards.
- Add a focused CLI integration test at `crates/tui/app/tests/runs_cli.rs` named `cli_runs_filters_by_partial_run_id` to prove `cowboy runs <partial-run-id>` accepts a partial id from persisted state and prints only matching summaries.

# How to verify

Run the narrowest checks first:

```bash
cargo test -p cowboy-command-parser runs
cargo test -p cowboy-workflow-engine list_runs
cargo test -p cowboy --test runs_cli cli_runs_filters_by_partial_run_id
```

Then run a CLI smoke test against an isolated temporary config/state directory after the implementation adds the filter. Create two persisted runs, capture one full `run-...` id from unfiltered output, choose a unique substring from that id, and run:

```bash
cargo run -- --config <temp-config.toml> runs <unique-partial-run-id>
```

Expected smoke result: the filtered `cargo run -- runs <partial-run-id>` output includes the matching full `run-...` id and does not include unrelated persisted run ids.

# TODO

- [x] TODO-01: Add optional partial run-id grammar to the shared runs command.
  - Procedure: Run `cargo test -p cowboy-command-parser runs` after adding `RunsArgs`, updating `SharedCommand::Runs`, and covering both CLI and slash parsing.
  - Expected result: The parser test run passes, unfiltered commands produce `partial_run_id == None`, filtered commands produce the supplied substring, and slash usage includes `[partial-run-id]`.
  - Implementer observed result: `cargo test -p cowboy-command-parser runs` exited 0; the focused parser run passed with the new `RunsArgs` grammar, `cowboy runs`/`/runs` unfiltered parsing, `cowboy runs abc`/`/runs abc` filtered parsing, and `/runs [partial-run-id]` generated usage coverage.

- [x] TODO-02: Add runtime filtering to run summary listing.
  - Procedure: Run `cargo test -p cowboy-workflow-engine list_runs` after changing `WorkflowRuntime::list_runs` and updating existing runtime tests to pass the new argument.
  - Expected result: Unfiltered listing returns every seeded run summary, filtered listing returns only summaries whose `run_id` contains the requested substring, and existing status/topic summary assertions still pass.
  - Implementer observed result: `cargo test -p cowboy-workflow-engine list_runs` exited 0 before and after final formatting edits; unfiltered runtime listing returned all seeded runs, `list_runs(Some("wait"))` returned only the run id containing `wait`, the case-sensitive nonmatch returned no runs, and existing structured status/topic summary checks passed.

- [x] TODO-03: Wire filtered runs through product CLI dispatch.
  - Procedure: Run `cargo test -p cowboy --test runs_cli cli_runs_filters_by_partial_run_id` after adding the focused CLI integration test and passing `RunsArgs.partial_run_id` from `crates/tui/app/src/main.rs` into the runtime.
  - Expected result: `cowboy runs <partial-run-id>` succeeds, prints the matching persisted run summary, and omits nonmatching run ids; `cowboy runs` remains unchanged.
  - Implementer observed result: `cargo test -p cowboy --test runs_cli cli_runs_filters_by_partial_run_id` exited 0; the integration test created two persisted runs, selected a unique substring from one run id, and observed filtered CLI output containing only the matching full run id while the unfiltered listing still exposed both run ids for setup.

- [x] TODO-04: Wire filtered runs through TUI slash-command dispatch and rendering.
  - Procedure: Run `cargo test -p cowboy runs_submission` after passing `/runs <partial-run-id>` through `spawn_runs_list` and `AppState` background-task completion.
  - Expected result: `/runs <partial-run-id>` creates a non-workflow background task, renders only matching run cards, preserves unfiltered empty text `known runs: 0`, and renders a distinct filtered empty result when no run id matches.
  - Implementer observed result: The first `cargo test -p cowboy runs_submission` run exited 101 because the existing unfiltered empty-state assertion still expected one transcript entry despite the preserved loading card plus empty card; after updating that assertion, the next two reruns exited 0. The passing runs covered `/runs <partial-run-id>` as a non-workflow background task, matching-only run cards, preserved unfiltered `known runs: 0`, and filtered empty text `matching runs for missing: 0` without leaked run ids.

- [x] TODO-05: Update user-facing command documentation for the new optional filter.
  - Procedure: Inspect `README.md` command examples and TUI command table after edits, then run `cargo test -p cowboy-command-parser slash_suggestions` to keep generated usage aligned with parser metadata.
  - Expected result: Documentation shows `cowboy runs [partial-run-id]` and `/runs [partial-run-id]`, while generated slash suggestions still advertise the same optional argument shape.
  - Implementer observed result: `README.md` now shows `cargo run -- runs [partial-run-id]`, `cowboy runs [partial-run-id]`, and `/runs [partial-run-id]`; `cargo test -p cowboy-command-parser slash_suggestions` exited 0 and confirmed generated slash suggestions include `/runs [partial-run-id]`.
