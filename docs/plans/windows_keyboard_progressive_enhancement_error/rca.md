# Windows keyboard progressive enhancement error RCA

## Bug behavior

Starting the interactive `cowboy` TUI on Windows can fail during terminal initialization with:

```text
Keyboard progressive enhancement not implemented for the legacy Windows API.
```

The failure happens before the workflow run loop starts because terminal mode setup returns an error.

## Root cause

`crates/tui/src/app.rs` enters TUI mode through `TerminalModeGuard::enter`, which unconditionally sends crossterm keyboard enhancement commands:

- `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` while entering terminal mode.
- `PopKeyboardEnhancementFlags` while restoring terminal mode.

In crossterm `0.29.0`, both commands explicitly return `std::io::ErrorKind::Unsupported` on Windows legacy API with the reported message. `PushKeyboardEnhancementFlags` also reports that ANSI code support is unavailable on Windows, so crossterm dispatches to the unsupported Windows API path instead of writing an ANSI sequence.

The TUI startup path does not gate these commands by platform or terminal capability, so Windows reaches the unsupported crossterm command during startup.

## Reproduction steps

1. Start the interactive TUI on Windows by running `cowboy` with no subcommand.
2. The CLI calls `run_tui` in `crates/tui/src/app.rs`.
3. `TerminalModeGuard::enter` enables raw mode and executes the terminal setup command list.
4. The setup list includes `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)`.
5. Crossterm returns `Unsupported` on Windows with `Keyboard progressive enhancement not implemented for the legacy Windows API.`

This investigation was grounded without a Windows host by tracing the repository startup path and the vendored dependency source for crossterm `0.29.0`, which contains the exact error string.

## Regression test

- Test file path: `crates/tui/src/app/tests.rs`
- Test name: `windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows`
- Command: `cargo test -p cowboy windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows`
- Expected failure before the fix: the test fails while `TerminalModeGuard` still executes `PushKeyboardEnhancementFlags` or `PopKeyboardEnhancementFlags` without a non-Windows guard.

## Current failing result

```text
running 1 test
failures:

---- app::tests::windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows stdout ----

thread 'app::tests::windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows' panicked at crates/tui/src/app/tests.rs:116:5:
Windows legacy console rejects crossterm keyboard enhancement commands with `Keyboard progressive enhancement not implemented for the legacy Windows API.`; gate these commands away from Windows before entering or restoring terminal mode: line 74: PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),; line 93: PopKeyboardEnhancementFlags,
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    app::tests::windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 119 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p cowboy --lib`
```

## Fix constraints

- Do not send crossterm keyboard enhancement commands on Windows unless a safe Windows-supported path is proven.
- Keep terminal restoration symmetric with terminal entry: only pop keyboard enhancement flags when they were pushed.
- Preserve the existing Unix behavior for escape-code disambiguation.
- Keep runtime workflow behavior out of `crates/tui`; this fix belongs in TUI terminal setup only.
- The regression test added for this investigation must remain failing until product code gates or removes the unsupported Windows keyboard enhancement commands.
