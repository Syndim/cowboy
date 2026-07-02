# Plan

Change the TUI exit keybinding so `Ctrl+C` exits the app, while plain `q` no longer quits and `Esc` cancels background tasks. Keep `/exit` as an explicit command and keep `/cancel` as the slash-command way to cancel background tasks.

# Changes

- Update `crates/tui/src/app/input.rs` so `KeyCode::Char('c')` with `KeyModifiers::CONTROL` returns `KeyHandling::Exit` instead of cancelling background tasks.
- Remove the `KeyCode::Char('q') | KeyCode::Esc => KeyHandling::Exit` match arm so:
  - plain `q` is handled by the existing character-input arm and is inserted into the composer;
  - `Esc` cancels active background tasks without exiting.
- Leave `/exit` command behavior in `crates/tui/src/app/commands.rs` unchanged because the request is about shortcuts, not slash commands.
- Leave `/cancel` behavior unchanged so background-task cancellation remains available after `Ctrl+C` becomes the exit shortcut.
- Update TUI status/help copy in `crates/tui/src/app/controls/status.rs` so it advertises `Ctrl-C exit` and does not imply `Ctrl-C cancel`.
- Update README TUI key documentation to remove `Esc` / `q` quit, document `Ctrl+C` as quit, and document `Esc` as background-task cancellation.

# Tests to be added/updated

- Add `crates/tui/src/app/input.rs` unit coverage proving `Ctrl+C` returns `KeyHandling::Exit`.
- Add or update input-handler unit coverage proving plain `q` appends `q` to the composer and returns `KeyHandling::Continue`.
- Add input-handler unit coverage proving `Esc` returns `KeyHandling::Continue`, leaves the composer unchanged, and routes to background-task cancellation.
- Update any status/help rendering tests if they exist or add focused coverage if status-line text is already tested nearby.

# How to verify

- Run `cargo test -p cowboy input::tests` to verify the key-handler behavior.
- Run `cargo test -p cowboy` if the focused test target is not accepted by Cargo or if status/help text tests are added outside the input module.
- Manual smoke check in the TUI:
  - launch `cargo run`;
  - type `q` and confirm it appears in the input box instead of exiting;
  - press `Esc` and confirm the app stays open and cancels active background tasks;
  - press `Ctrl+C` and confirm the app exits cleanly.

# TODO

- [x] Change `Ctrl+C` handling in `crates/tui/src/app/input.rs` to return `KeyHandling::Exit`.
- [x] Remove `q` and `Esc` from the exit shortcut match arm.
- [x] Add input-handler tests for `Ctrl+C`, plain `q`, and `Esc`.
- [x] Update TUI status/help text that mentions `Ctrl-C cancel` or old quit keys.
- [x] Update README TUI key documentation for the new shortcut behavior.
- [x] Run focused TUI crate tests covering the input handler.
- [x] Smoke test the TUI key behavior manually.
- [x] Route `Esc` to background-task cancellation and document it.
