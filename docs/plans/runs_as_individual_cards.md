# Plan

Refine the TUI `/runs` rendering so each workflow run is displayed as its own transcript card instead of flattening every run into one shared `Runs` card. Keep the existing CLI `cowboy runs` output unchanged by continuing to use `run_summary::render_run_summary_lines` for command-line summaries.

Repository grounding:

- `crates/tui/app/src/app/commands.rs::show_runs` currently calls `runtime.list_runs()`, sets the status to `N run(s)`, appends `known runs: N`, extends one `details` vector with every `render_run_summary_lines(&run)`, then calls `state.push_card("Runs", details)` once.
- `crates/tui/app/src/app/state.rs::TranscriptEntry::Card` already represents one card per transcript entry, and `AppState::push_card` appends one entry to the transcript.
- `crates/tui/app/src/run_summary.rs::render_run_summary_lines` already produces the structured per-run lines that both CLI and TUI can reuse.
- Existing tests in `crates/tui/app/src/app/commands.rs` seed completed, waiting, and failed runs and assert the current flattened `/runs` card content.

The desired behavior is: `/runs` still reports the total run count in the status line, but when runs exist it pushes one transcript card per run. An empty run list may keep a single summary/notice card because there is no run to display.

# Changes

1. Update `show_runs` in `crates/tui/app/src/app/commands.rs`:
   - Keep `let runs = runtime.list_runs()?` and `state.set_status(format!("{} run(s)", runs.len()))`.
   - For an empty list, keep a single `Runs` card with `known runs: 0` or an equivalent empty-state message.
   - For a non-empty list, remove the aggregate `details` vector and call `state.push_card(...)` once per run, using `render_run_summary_lines(&run)` as that card's body.
   - Do not change runtime listing, sorting, persistence, or CLI behavior.

2. Choose a stable per-run card title and keep styling intentional:
   - Prefer a simple title such as `Run` for every per-run card, with the run id remaining the first body line from `render_run_summary_lines`.
   - Update `app_card_status_and_tone` in `crates/tui/app/src/app/state.rs` if needed so the new run cards use normal Cowboy card chrome instead of the unknown `?` fallback.
   - Do not introduce a new card renderer or a second run-summary formatting path unless the existing `Card`/`push_card` path cannot express the UI cleanly.

3. Preserve structured run details:
   - Keep topic, workflow, current step, head, and expanded status fields exactly as structured text from `render_run_summary_lines`.
   - Keep waiting-for-input details (`status.waiting_step`, `status.prompt_id`, `status.message`, `status.choices`) and failed reasons.
   - Continue avoiding Rust debug payload leaks such as `WaitingForInput {`, `Failed {`, and `resume_callback:`.

4. Keep the diff scoped to TUI `/runs` rendering and its tests:
   - Do not move workflow runtime logic into `crates/tui/app`.
   - Do not change `cowboy-workflow-engine::WorkflowRuntime::list_runs`.
   - Do not change persisted run data or event-log formats.

# Tests to be added/updated

- Update `crates/tui/app/src/app/commands.rs::tests::runs_command_renders_structured_runtime_summaries` to assert per-run cards rather than one aggregate card:
  - after seeding three runs and calling `show_runs`, `state.status()` is still `3 run(s)`;
  - the transcript contains three run card entries for the three runs;
  - each run card contains its own run id and structured fields;
  - each run card does not contain the other seeded run ids;
  - no card leaks Rust debug fragments.

- Add or update an empty-list `/runs` test if one does not already exist:
  - with no seeded runs, `show_runs` sets status to `0 run(s)`;
  - the transcript renders one useful empty-state card/message, not zero output and not a misleading per-run card.

- Leave `crates/tui/app/src/run_summary.rs` tests intact unless the implementation intentionally changes the shared textual summary format. The preferred implementation should not require changing those tests.

# How to verify

Run the narrowest checks first, then formatting and linting for the touched crate:

```bash
cargo test -p cowboy runs_command
cargo test -p cowboy app::commands::tests
cargo fmt --check
cargo clippy -p cowboy --all-targets -- -D warnings
```

If the exact test filter differs after implementation, run the nearest equivalent targeted tests that execute the `/runs` command tests in `crates/tui/app/src/app/commands.rs`.

# TODO

- [x] Update `show_runs` to push one card per non-empty run list entry.
- [x] Preserve a clear empty-state card for zero runs.
- [x] Add or adjust card styling for the per-run card title if the default fallback icon/tone is used.
- [x] Update `/runs` command tests to assert separate cards and per-card isolation.
- [x] Add or update the zero-runs test coverage.
- [x] Run the targeted `/runs` tests.
- [x] Run `cargo fmt --check`.
- [x] Run `cargo clippy -p cowboy --all-targets -- -D warnings`.
