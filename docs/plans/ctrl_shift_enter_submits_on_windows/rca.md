# Bug behavior

On Windows, pressing Shift+Enter or Ctrl+Enter in the TUI composer is reported to submit the current input instead of inserting a newline. The UI advertises `Shift/Ctrl-Enter newline`, so modified Enter must leave the composer in edit mode and append `\n`.

The repository already has a Linux/unit-level happy-path test for `KeyCode::Enter` with `KeyModifiers::SHIFT` or `KeyModifiers::CONTROL`, and that path inserts a newline. The Windows failure is grounded at the terminal setup boundary: the Windows keyboard-enhancement path is disabled before key events reach the input handler.

# Root cause

`crates/tui/app/src/app.rs` has a Windows-specific `push_keyboard_enhancement_flags` implementation that returns `Ok(false)` and does not request `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES`.

`crates/tui/app/src/app/input.rs` only treats Enter as newline when the received `KeyEvent` has `SHIFT` or `CONTROL` in `key.modifiers`. Otherwise, plain `KeyCode::Enter` falls through to submit when the composer accepts submission.

Therefore, when the Windows terminal path does not preserve or request disambiguated modified-Enter events, Shift+Enter/Ctrl+Enter can arrive as plain Enter and the input handler submits. [INFERENCE] The user-visible Windows symptom follows from this source path because the current Linux test covers only already-disambiguated events, not the Windows setup path that enables those modifiers to be observed.

# Reproduction steps

1. Inspect `crates/tui/app/src/app.rs` and observe that `#[cfg(windows)] fn push_keyboard_enhancement_flags` returns `Ok(false)`.
2. Inspect `crates/tui/app/src/app/input.rs` and observe that newline insertion requires `key.modifiers` to contain `SHIFT` or `CONTROL`; plain `Enter` submits.
3. Run the focused regression test below. It fails on the current source because Windows keyboard enhancement is a no-op.

# Regression test

- Test file path: `crates/tui/app/src/app/tests.rs`
- Test name: `app::tests::windows_keyboard_enhancement_path_preserves_modified_enter_support`
- Command: `cargo test -p cowboy windows_keyboard_enhancement_path_preserves_modified_enter_support`
- Expected failure before the fix: the test panics because the Windows `push_keyboard_enhancement_flags` implementation still returns `Ok(false)`, proving the Windows setup path disables the keyboard enhancement needed to preserve modified Enter behavior.

# Current failing result

```text
running 1 test
failures:

---- app::tests::windows_keyboard_enhancement_path_preserves_modified_enter_support stdout ----

thread 'app::tests::windows_keyboard_enhancement_path_preserves_modified_enter_support' (830162) panicked at crates/tui/app/src/app/tests.rs:179:5:
Windows terminal setup disables keyboard enhancement entirely; without DISAMBIGUATE_ESCAPE_CODES, Shift/Ctrl-Enter can arrive as plain Enter and submit instead of inserting a newline
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::tests::windows_keyboard_enhancement_path_preserves_modified_enter_support

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 268 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

# Fix constraints

- Do not change the input-handler contract that plain Enter submits and modified Enter inserts a newline.
- Preserve the existing Windows legacy-console guard: crossterm's `PushKeyboardEnhancementFlags` WinAPI execution path returns `Unsupported` for the legacy Windows API, so the fix must not unconditionally execute that unsupported command through the WinAPI path.
- The fix should make the Windows terminal setup path preserve Shift+Enter/Ctrl+Enter modifiers where supported, while keeping terminal startup/restoration safe when enhancement is unavailable.
- Keep runtime logic in `crates/tui/app`; do not move terminal input policy into workflow crates.
