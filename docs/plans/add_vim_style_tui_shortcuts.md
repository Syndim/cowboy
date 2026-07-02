# Plan

Replace the TUI transcript scrolling shortcuts with vim-style control-key bindings. `Ctrl+U` scrolls upward to older transcript entries, and `Ctrl+D` scrolls downward toward newer transcript entries. `PgUp` and `PgDn` must stop scrolling; the UI should advertise only the vim-style scroll shortcuts.

Scope this change to modified control-key shortcuts. Do not add plain `j`/`k`/`g` bindings because unmodified character keys currently belong to the composer text input.

# Changes

- Update `crates/tui/src/app/input.rs` key handling so:
  - `Ctrl+U` calls `AppState::scroll_events_up()` and returns `KeyHandling::Continue`;
  - `Ctrl+D` calls `AppState::scroll_events_down()` and returns `KeyHandling::Continue`;
  - `KeyCode::PageUp` and `KeyCode::PageDown` no longer call the scroll methods and instead behave as ignored keys;
  - composer input, history navigation, submit, newline, `Ctrl+C` exit, and `Esc` cancellation behavior stay unchanged.
- Keep the scroll amount and follow-latest semantics centralized in `AppState` instead of duplicating scroll-offset math in the input handler.
- Update TUI status-strip copy in `crates/tui/src/app/controls/status.rs` so transcript-scroll hints use `Ctrl-U/Ctrl-D scroll` and no longer mention `PgUp/PgDn scroll`.
- Update README TUI key documentation so the shortcut table lists `Ctrl+U` / `Ctrl+D` as transcript scroll shortcuts and removes `PgUp` / `PgDn` as supported transcript-scroll shortcuts.
- Leave `/help` slash-command metadata unchanged unless implementation discovers it has a keyboard-shortcut section; current `/help` output is command-focused, not keybinding-focused.

# Tests to be added/updated

- Add focused `crates/tui/src/app/input.rs` unit tests proving `Ctrl+U` scrolls the transcript upward, does not mutate the composer, and returns `KeyHandling::Continue`.
- Add focused `crates/tui/src/app/input.rs` unit tests proving `Ctrl+D` scrolls the transcript downward, restores follow-latest when the offset reaches zero, does not mutate the composer, and returns `KeyHandling::Continue`.
- Add focused input-handler tests proving `PgUp` and `PgDn` no longer change the transcript scroll offset.
- Add or update status-line rendering coverage if existing tests cover shortcut hint text; otherwise keep verification to focused input-handler tests and README review.

# How to verify

- Run `cargo test -p cowboy input::tests` to verify key-handler behavior.
- Run `cargo test -p cowboy` if status-line rendering tests are added outside the input module or if the focused test selector is not accepted by Cargo.
- Manual smoke check in the TUI:
  - launch `cargo run`;
  - produce enough transcript lines to scroll;
  - press `Ctrl+U` and confirm the transcript moves to older lines;
  - press `Ctrl+D` and confirm the transcript moves back toward the latest lines;
  - press `PgUp` and `PgDn` and confirm neither key scrolls the transcript;
  - confirm the status strip and README document only `Ctrl+U` / `Ctrl+D` for transcript scrolling.

# TODO

- [x] Add `Ctrl+U` handling in `crates/tui/src/app/input.rs` that delegates to `AppState::scroll_events_up()`.
- [x] Add `Ctrl+D` handling in `crates/tui/src/app/input.rs` that delegates to `AppState::scroll_events_down()`.
- [x] Remove `PgUp` scroll handling from `crates/tui/src/app/input.rs`.
- [x] Remove `PgDn` scroll handling from `crates/tui/src/app/input.rs`.
- [x] Preserve composer input, history navigation, submit, newline, `Ctrl+C` exit, and `Esc` cancellation behavior.
- [x] Add input-handler unit coverage for `Ctrl+U` scrolling upward without mutating input.
- [x] Add input-handler unit coverage for `Ctrl+D` scrolling downward and restoring follow-latest at offset zero without mutating input.
- [x] Add input-handler unit coverage proving `PgUp` and `PgDn` do not scroll.
- [x] Update status-strip shortcut text in `crates/tui/src/app/controls/status.rs` to remove `PgUp/PgDn` and show `Ctrl-U/Ctrl-D`.
- [x] Update README TUI key documentation to remove `PgUp` / `PgDn` scrolling and document `Ctrl+U` / `Ctrl+D` scrolling.
- [x] Run the focused TUI input-handler tests.
- [x] Smoke test the TUI scrolling shortcuts manually.

# Verification evidence

- Manual TUI smoke rerun on 2026-07-02: launched `target/debug/cowboy` in a PTY, generated transcript content with `/help`, sent `Ctrl+U`, `Ctrl+D`, `PgUp`, and `PgDn`, then exited with `Ctrl+C`; process exit code was `0`; captured screen output showed the `Ctrl-U/Ctrl-D` scroll hint and no `PgUp/PgDn scroll` hint.
