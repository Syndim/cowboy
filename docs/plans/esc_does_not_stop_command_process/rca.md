# Bug behavior

On Windows, when a workflow command action is running, the composer can show `No agent accepting prompts · draft retained · Esc cancels`. Pressing Esc is expected to cancel the active background workflow task, and Ctrl+C is expected to exit the TUI. User feedback reports that neither Esc nor Ctrl+C works in this state, so the visible cancellation affordance is misleading and the running command cannot be stopped from the keyboard.

# Root cause

The app-level key handling is correct once a key event reaches it: `handle_key_press` maps `KeyCode::Esc` to `AppState::cancel_background_tasks`, and `Ctrl+C` returns `KeyHandling::Exit` before composer edit gating.

The Windows terminal setup path can prevent those key events from reaching that handler. `crates/tui/app/src/app.rs` currently emits `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` as raw ANSI from the Windows `push_keyboard_enhancement_flags` path when `crossterm::ansi_support::supports_ansi()` is true. In crossterm 0.29, `PushKeyboardEnhancementFlags` documents that it makes Escape and modified keys use CSI-u sequences, but crossterm's Windows event source reads `WinAPI` console `KeyEventRecord` values through `event/source/windows.rs` and `event/sys/windows/parse.rs`; the CSI-u byte parser lives under the Unix parser. Emitting the keyboard-enhancement ANSI sequence on Windows can therefore switch the terminal into an input mode whose Esc/Ctrl+C reports are not decoded by the Windows event reader used by the TUI.

# Reproduction steps

1. Inspect the TUI loop: it handles only `Event::Key(key)` values with `key.kind == KeyEventKind::Press`, then delegates to `handle_key_press`.
2. Inspect `handle_key_press`: Esc and Ctrl+C behavior is present if those key events are delivered.
3. Inspect the Windows `push_keyboard_enhancement_flags` implementation: it emits `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` as ANSI when ANSI support is detected.
4. Inspect crossterm 0.29: Windows event reading consumes console `KeyEventRecord` events, while CSI-u parsing is implemented in the Unix parser.
5. Add a regression test that fails while the Windows terminal setup path can emit CSI-u keyboard enhancement sequences.

# Regression test

- Test file path: `crates/tui/app/src/app/tests.rs`
- Test name: `app::tests::windows_keyboard_enhancement_path_does_not_emit_csi_u_sequences`
- Command: `cargo test -p cowboy windows_keyboard_enhancement_path_does_not_emit_csi_u_sequences -- --nocapture`
- Expected failure before the fix: the test finds the Windows `push_keyboard_enhancement_flags` path still emitting `PushKeyboardEnhancementFlags` through `write_keyboard_enhancement_ansi`, which can make Esc/Ctrl+C arrive as unhandled CSI-u sequences.

# Current failing result

```text
running 1 test
thread 'app::tests::windows_keyboard_enhancement_path_does_not_emit_csi_u_sequences' (961577) panicked at crates/tui/app/src/app/tests.rs:199:5:
Windows crossterm reads WinAPI KeyEventRecord events, not CSI-u bytes; emitting keyboard enhancement ANSI can make Esc/Ctrl+C arrive as unhandled sequences: (stdout: &mut io::Stdout) -> Result<bool> {
    if !crossterm::ansi_support::supports_ansi() {
        return Ok(false);
    }

    let command = PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES);
    write_keyboard_enhancement_ansi(stdout, command)?;
    Ok(true)
}


note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test app::tests::windows_keyboard_enhancement_path_does_not_emit_csi_u_sequences ... FAILED

failures:

failures:
    app::tests::windows_keyboard_enhancement_path_does_not_emit_csi_u_sequences

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 281 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

# Fix constraints

- Do not advertise `Esc cancels` on Windows unless Esc is delivered to the TUI input handler in the command-action running state.
- Preserve Ctrl+C as the app exit shortcut; the fix must not rely on composer submission or edit acceptance because Ctrl+C is a global control.
- Do not emit CSI-u keyboard enhancement sequences from the Windows terminal setup path unless the Windows event reader can decode the resulting input events.
- Keep terminal setup/input policy in `crates/tui/app`; do not move TUI keyboard behavior into workflow runtime or command action crates.
- Reconcile this fix with modified-Enter behavior on Windows explicitly; preserving Shift/Ctrl+Enter must not break Esc cancellation or Ctrl+C exit.
