# Plan

Persist the TUI composer input history in a TUI-owned append-only history file under Cowboy's existing `state_dir` so `↑` / `↓` history navigation can reuse entries from previous TUI runs. Keep the feature scoped to the `cowboy` TUI crate: do not add workflow-store tables, do not route UI line-editor history through `WorkflowRuntime`, and do not change workflow execution, Lua, ACP, redb workflow persistence, or CLI command behavior.

Use a line-oriented append file instead of rewriting one whole JSON document. A whole-file JSON array/object has a read-modify-write race when multiple Cowboy TUI instances submit input at the same time. Name the file `<state_dir>/input_history`, with no `.log` extension. This follows the convention used by command-line tools where the filename names the data, not the storage implementation: Bash defaults to `.bash_history`, fish defaults to `fish_history`, and zsh uses the user-configured `HISTFILE` path. Use `<state_dir>/input_history.lock` as the advisory lock file. Each accepted input appends one complete versioned JSON-line record while holding the exclusive lock; loading holds a shared lock and reads the newest valid records. This preserves cross-run history without introducing redb schema/runtime responsibility for UI-only line-editor state.

`AppState::new(config)` should load persisted history from the configured state directory. Accepted non-empty submissions should update the in-memory history and append one locked history record. Loading and saving should be best-effort: missing, corrupt, locked, or temporarily unwritable history files must not prevent Cowboy from launching, submitting prompt answers, or starting workflow requests.

# Changes

- Add a TUI-local history persistence module, preferably `crates/tui/src/app/history.rs`.
- Add a small file-locking dependency to `crates/tui/Cargo.toml`, for example `fs2`, unless the implementation uses a standard-library-only lock strategy that provides equivalent cross-process exclusion.
- Store history at stable paths under `AppConfig.state_dir`:
  - `<state_dir>/input_history` for append-only records;
  - `<state_dir>/input_history.lock` for advisory locking.
- Store one versioned JSON record per line, for example `{ "version": 1, "entry": "..." }`, so multiline input is escaped safely and future record formats have room.
- Do not store a whole JSON array/object that requires read-modify-write for every submission.
- Do not use a `.log` extension for the history file; the file is a user input-history data file, not a diagnostic log.
- Do not modify `crates/workflow/store/src/tables.rs`, `cowboy_workflow_store::RedbRunStore`, `cowboy_workflow_core::RunStore`, or `cowboy_workflow_engine::WorkflowRuntime` for this feature.
- Do not add a redb dependency path to the TUI for input history; keep the persistence boundary local to the TUI crate.
- Load persisted entries in `AppState::new(config)` and initialize the existing `history: Vec<String>` from the newest valid records in the history file.
- Keep existing navigation semantics in `AppState::history_previous` and `AppState::history_next`: `↑` moves to older entries, `↓` moves toward newer entries and clears the composer after the newest entry.
- Persist only after `take_submitted_input` accepts a non-empty trimmed input.
- Preserve adjacent-duplicate behavior under the lock: before appending, compare the accepted input with the newest valid on-disk entry and skip the append when they match.
- Add a fixed in-memory load limit for restored history; keep the newest entries when reading.
- Add compaction under the exclusive lock when the file exceeds a size or record-count threshold: rewrite a temporary compacted file containing only the newest retained entries, then rename it over the old history file while still holding the lock.
- Create `state_dir` before creating the lock or history files.
- On append, open the history file with append/create semantics, write one newline-terminated record, and flush the file before releasing the lock.
- Treat lock, load, append, and compaction failures as non-fatal and log them with `tracing::warn!`; do not surface history persistence errors as workflow submission failures.
- After a successful append or duplicate skip, synchronize `AppState` history from the newest on-disk records returned by the history module so one TUI can pick up entries submitted by another TUI instance.
- Update README TUI key documentation to clarify that `↑` / `↓` browse persisted input history from previous TUI runs.
- Update README persistence documentation to list `input_history` and `input_history.lock` under `state_dir` as TUI input history files, and keep `workflow.redb` scoped to workflow runtime data.
- Update `docs/module-map.md` only if a new `app/history.rs` module is added.

# Tests to be added/updated

- Add `crates/tui/src/app/history.rs` unit tests for missing history file load returning empty history.
- Add history module tests for valid versioned JSON-line history loading, including multiline input encoded inside one record.
- Add history module tests for corrupt or unsupported lines being skipped non-fatally while valid records are preserved.
- Add history module tests for append creating `state_dir`, the lock file, and the history file.
- Add history module tests proving append writes one complete newline-terminated record per accepted input.
- Add history module tests proving the file name is exactly `input_history` and not `input-history.log` or another `.log` file.
- Add history module tests for adjacent duplicate suppression using the newest on-disk entry.
- Add history module tests for load-limit truncation keeping the newest entries.
- Add history module tests for locked compaction preserving the newest retained entries and replacing the history file atomically.
- Add a concurrent append test with multiple threads or processes writing through the history module to prove entries are not lost or interleaved.
- Add history module tests proving the history paths are under `state_dir` and separate from the configured `workflow_store` path.
- Add `crates/tui/src/app/state.rs` or `crates/tui/src/app/input.rs` tests proving a fresh `AppState` with the same `state_dir` restores previously saved history with `KeyCode::Up`.
- Add coverage proving `KeyCode::Down` after restored history keeps the current clear-composer behavior.
- Add coverage proving empty submissions are still inert and do not create or append to the history file.
- Update existing tests that construct `AppState::new(AppConfig { state_dir: tempfile, ... })` only as needed for file-backed initialization.

# How to verify

- Run `cargo test -p cowboy app::history`.
- Run `cargo test -p cowboy app::state` if state-level history tests are added there.
- Run `cargo test -p cowboy app::input` if input-handler history tests are added there.
- Run `cargo test -p cowboy app::commands` if command-submit history tests are added there.
- Run `cargo test -p cowboy` after focused tests pass.
- Manual smoke test:
  - launch two `cargo run -p cowboy` TUI instances with the same temporary `XDG_STATE_HOME`;
  - submit a distinctive request or slash command in each instance;
  - exit both with `Ctrl+C`;
  - relaunch with the same `XDG_STATE_HOME`;
  - press `↑` repeatedly and confirm both previous submissions appear in the composer;
  - press `↓` and confirm the composer clears after the newest restored history entry;
  - confirm `input_history` and `input_history.lock` exist under the temporary state directory;
  - confirm no `input-history.log` file is created and `workflow.redb` was not used for the input history.


# Verification evidence

- `cargo build -p cowboy` passed before the manual smoke test.
- Manual two-instance smoke test passed on 2026-07-06 using `target/debug/cowboy` and a temporary shared `XDG_STATE_HOME` (`cowboy-history-smoke-9n9ffkfd`):
  - launched two TUI instances against the same temporary state root;
  - submitted `/help` in the first instance and `/cancel` in the second instance;
  - exited both instances with `Ctrl+C`;
  - relaunched a third TUI with the same `XDG_STATE_HOME`;
  - pressed `↑` and observed `/cancel` restored;
  - pressed `↑` again and observed `/help` restored;
  - pressed `↓` and observed navigation toward `/cancel`;
  - pressed `↓` again and observed the composer clear after the newest restored history entry;
  - confirmed `input_history` exists;
  - confirmed `input_history.lock` exists;
  - confirmed `input-history.log` is absent;
  - confirmed `workflow.redb` is absent and was not used for TUI input history;
  - confirmed `input_history` contains exactly two records: `{ "version": 1, "entry": "/help" }` and `{ "version": 1, "entry": "/cancel" }`.

# TODO

- [x] Add a TUI-local history persistence module for locked append-only history under `state_dir`.
- [x] Add a file-locking dependency or equivalent cross-process lock implementation to the TUI crate.
- [x] Keep input history out of `cowboy-workflow-store`, `RunStore`, and `WorkflowRuntime`.
- [x] Use `<state_dir>/input_history` and `<state_dir>/input_history.lock` as the stable history paths.
- [x] Avoid `.log` for the history file name because it is not a diagnostic log.
- [x] Implement loading from `<state_dir>/input_history` with empty-history behavior for missing files.
- [x] Implement JSON-line parsing with corrupt or unsupported lines skipped non-fatally and warning logs.
- [x] Implement append-only saving of one versioned, newline-terminated record per accepted input.
- [x] Use `<state_dir>/input_history.lock` to serialize appends and compaction across multiple Cowboy TUI instances.
- [x] Preserve adjacent-duplicate suppression using the newest valid on-disk entry while holding the lock.
- [x] Add a fixed load limit and newest-entry truncation for restored history.
- [x] Add exclusive-lock compaction to bound history-file growth without racing concurrent writers.
- [x] Create `state_dir` before creating the lock file or history file.
- [x] Flush appended history records before releasing the lock.
- [x] Wire `AppState::new(config)` to load persisted history from the configured state directory.
- [x] Wire accepted non-empty submissions to append updated history without changing dispatch behavior.
- [x] Synchronize `AppState` history from the history module after successful append or duplicate skip.
- [x] Preserve existing `↑` / `↓`, empty-submit, slash-command, prompt-answer, and workflow-dispatch behavior.
- [x] Make lock, load, append, and compaction failures non-fatal in the TUI and log warnings.
- [x] Add focused history module tests for missing, valid, corrupt, multiline, append, filename, duplicate, truncation, compaction, concurrent-append, and path-separation cases.
- [x] Add focused TUI state or input tests for restored `↑` / `↓` navigation across fresh `AppState` instances.
- [x] Add focused TUI coverage proving empty submissions do not create or append to the history file.
- [x] Leave README unchanged for this feature per confirm-result feedback.
- [x] Update module-map documentation if a new history module is added.
- [x] Run focused TUI tests and the full `cowboy` crate test suite.
- [x] Manually smoke-test cross-run `↑` / `↓` history restoration with two concurrent TUI instances.
