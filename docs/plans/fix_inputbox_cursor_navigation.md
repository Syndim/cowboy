# Plan

Fix the TUI input box by using a standard terminal-input editing model instead of hand-rolling cursor movement. The current composer is not a real editable input widget: `AppState` stores only `input: String`, `push_input` always appends, `pop_input_char` always removes the final character, and `composer::render` always places the terminal cursor at the end. `crates/tui/src/app/input.rs` also logs left/right keys but does not handle them. That is why a common input-box behavior is missing.

Use the existing `tui-input` crate for the common editor behavior: cursor position, left/right movement, control-left/control-right word jumps, insertion, backspace/delete, and Unicode-safe cursor accounting. Keep Cowboy-specific logic only where the app is different from a plain input widget: slash-command completion, command history, transcript scrolling, prompt submission, paste routing, and the custom ratatui composer layout.

# Changes

- Add `tui-input` to `crates/tui/Cargo.toml` with the `crossterm` backend feature and default features disabled, e.g. `tui-input = { version = "0.15.3", default-features = false, features = ["crossterm"] }`.
  - Do not enable `tui-input`'s default `ratatui-crossterm` feature because latest `tui-input` depends on `ratatui 0.30`, while Cowboy currently uses `ratatui 0.29`.
  - Use the crate as an input state/editor backend, not as a ratatui rendering widget.
- Replace `AppState`'s raw `input: String` storage in `crates/tui/src/app/state.rs` with `tui_input::Input` or a tiny wrapper around it.
  - Keep `AppState::input() -> &str` for existing callers by returning `Input::value()`.
  - Keep `push_input(&str)` as the paste/text insertion entry point, but implement it by inserting through `tui-input` at the current cursor rather than appending.
  - Replace `pop_input_char` internals with `tui-input`'s delete-previous-character behavior.
  - Add a delete-next-character helper for `KeyCode::Delete`.
  - Reset or move the editor cursor correctly after slash completion, command-history navigation, submit, and input clearing.
- Update `crates/tui/src/app/input.rs` so app-level shortcuts are still matched explicitly before editor delegation:
  - keep `Ctrl+C` exit;
  - keep `Esc` background-task cancellation;
  - keep `Enter` submit and modified-enter/`Ctrl+J` newline insertion;
  - keep `Tab` slash completion;
  - keep `Up`/`Down` command-history navigation;
  - keep `Ctrl+U`/`Ctrl+D` transcript scrolling;
  - keep `End` follow-latest because that key is already documented for transcript behavior.
- After those app-level keys, delegate common editing keys to `tui-input`'s crossterm event handling or `InputRequest` API:
  - plain `Left` / `Right` move the input cursor by one character;
  - `Ctrl+Left` / `Ctrl+Right` move the input cursor by one word;
  - typed characters insert at the cursor;
  - `Backspace` deletes before the cursor;
  - `Delete` deletes at the cursor.
- Update `crates/tui/src/app/controls/composer.rs` rendering to use the editor cursor instead of assuming the cursor is at the end.
  - Compute the visual row/column from the input value plus the `tui-input` cursor position.
  - Keep Cowboy's existing prompt prefixes (`> ` and continuation indentation), wrapping, slash suggestions, composer height cap, hidden-line marker, Unicode display-width handling, and cursor style.
  - When tall input is clipped, choose the visible rows so the editor cursor remains visible; if the cursor is at the end, preserve the current tail-oriented view.
- Update README TUI key documentation to add `←` / `→` for input cursor movement and `Ctrl+←` / `Ctrl+→` for input word jumps.
- Do not change workflow runtime crates, persisted state, event models, command dispatch behavior, or transcript scrolling semantics.

# Tests to be added/updated

- Add focused `crates/tui/src/app/input.rs` tests proving `Left` and `Right` move the input cursor without mutating text, clamp at the beginning/end, and allow insertion in the middle of existing input.
- Add focused input tests proving `Ctrl+Left` and `Ctrl+Right` jump by words and do not interfere with `Ctrl+U`/`Ctrl+D` transcript scrolling or `Up`/`Down` history navigation.
- Add focused input/state tests proving `Backspace` deletes before the cursor and `Delete` deletes at the cursor.
- Add paste/newline tests proving pasted text and modified-enter newline insertion go at the cursor rather than always appending.
- Add Unicode tests covering cursor movement and deletion across multibyte or wide characters so the editor does not split UTF-8 and rendered cursor columns stay correct.
- Add or update `crates/tui/src/app/tests.rs` draw-level cursor-position coverage for a moved cursor in a single-line input.
- Add or update wrapped/tall input cursor coverage proving the rendered cursor row/column follows the editor cursor and stays visible when the composer clips input rows.
- Keep existing tests for slash completion, submit, command history, transcript scrolling, `Ctrl+C` exit, `Esc` cancellation, wrapped input height, and cursor style passing.

# How to verify

- Run `cargo test -p cowboy input::tests` to verify key handling and editor behavior.
- Run `cargo test -p cowboy app::tests` to verify draw-level composer cursor placement.
- Run `cargo test -p cowboy app::controls::composer` if composer-local wrapping/cursor tests are added or changed.
- Run `cargo test -p cowboy` after focused tests pass because the input state is shared by command dispatch, history, completion, paste, and rendering.
- Manual smoke check in the TUI:
  - launch `cargo run -p cowboy`;
  - type `hello world`, press `Left` several times, type text, and confirm insertion happens at the visible cursor instead of appending;
  - press `Ctrl+Left` and `Ctrl+Right` and confirm word jumps;
  - use `Backspace` and `Delete` around the middle of the input and confirm the expected adjacent character is removed;
  - paste text while the cursor is in the middle and confirm paste inserts at the cursor;
  - insert a newline with `Shift+Enter` or `Ctrl+Enter` in the middle and confirm it inserts at the cursor;
  - test a multibyte/wide character such as `é` or `中` and confirm movement/deletion/rendering remain correct;
  - confirm `Up`/`Down` still browse history and `Ctrl+U`/`Ctrl+D` still scroll the transcript.

# Manual smoke evidence

2026-07-03 follow-up PTY smoke launched `cargo run -q -p cowboy` and verified:

- `Left` movement plus mid-input insertion rendered `hello Xworld`.
- `Ctrl+Left`, `Delete`, and `Backspace` verified word-jump positioning and adjacent deletion.
- `Ctrl+Right` moved to the word/end position before inserting `Z`.
- Bracketed paste inserted `PASTE` at the cursor, rendering `hello PASTEworldZ`.
- `Ctrl+Enter` inserted a newline at the cursor, splitting input into `> ab` and `> cd` rows.
- Unicode editing/rendering deleted `é` safely and rendered `中` at the cursor.
- `Ctrl+U`/`Ctrl+D` changed the transcript viewport upward and returned it.
- `Up`/`Down` restored `/help` from command history and then cleared it.

# TODO

- [x] Add `tui-input` to `crates/tui/Cargo.toml` with `default-features = false` and `features = ["crossterm"]`.
- [x] Replace `AppState` raw `input: String` storage with `tui_input::Input` or a thin wrapper around it.
- [x] Preserve `AppState::input() -> &str` by exposing the editor value.
- [x] Reimplement `AppState::push_input(&str)` as cursor-position insertion through the editor model.
- [x] Reimplement `AppState::pop_input_char()` as delete-previous-character through the editor model.
- [x] Add an `AppState` helper for delete-next-character through the editor model.
- [x] Reset or position the editor cursor correctly after slash completion.
- [x] Reset or position the editor cursor correctly after command-history previous/next.
- [x] Reset or position the editor cursor correctly after submit and input clearing.
- [x] Keep app-level shortcuts in `crates/tui/src/app/input.rs` ahead of editor delegation.
- [x] Delegate `Left` and `Right` input editing to `tui-input`.
- [x] Delegate `Ctrl+Left` and `Ctrl+Right` word jumps to `tui-input`.
- [x] Delegate typed character insertion to `tui-input`.
- [x] Delegate `Backspace` and `Delete` editing to `tui-input`.
- [x] Route paste and modified-enter newline insertion through cursor-aware insertion.
- [x] Update composer cursor placement to use the editor cursor position rather than the input end.
- [x] Keep wrapped/tall composer rendering centered on the visible editor cursor when needed.
- [x] Preserve prompt prefixes, wrapping, slash suggestions, composer height cap, hidden-line marker, Unicode width handling, and cursor style.
- [x] Update README TUI key documentation for left/right and control-left/control-right input navigation.
- [x] Add input-handler tests for plain left/right movement, clamping, and mid-input insertion.
- [x] Add input-handler tests for control-left/control-right word jumps.
- [x] Add tests for backspace/delete at the cursor.
- [x] Add tests for paste and newline insertion at the cursor.
- [x] Add Unicode cursor movement/deletion/rendering regression coverage.
- [x] Add draw-level cursor-position tests for moved single-line input.
- [x] Add wrapped/tall input cursor visibility coverage.
- [x] Run the focused TUI input-handler tests.
- [x] Run the focused TUI draw/composer tests.
- [x] Run the full `cowboy` crate test suite.
- [x] Manually smoke-test cursor movement, word jumps, mid-input editing, paste, newline insertion, Unicode editing, history, and transcript scrolling in the TUI.
