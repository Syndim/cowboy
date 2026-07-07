## Plan

Fix the Windows TUI startup failure described in `docs/plans/windows_keyboard_progressive_enhancement_error/rca.md` by preventing crossterm keyboard progressive enhancement commands from being executed on Windows. Keep the investigator-added repro test `crates/tui/src/app/tests.rs::windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows` unchanged and use it as the guardrail for the fix.

The implementation should stay inside `crates/tui/src/app.rs` because the bug is terminal setup/restoration behavior, not workflow runtime behavior.

## Changes

- Update `TerminalModeGuard` in `crates/tui/src/app.rs` to track whether keyboard enhancement flags were successfully pushed, for example with a `keyboard_enhancement_active: bool` field in addition to `restored`.
- Move `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` out of the unconditional `execute!` call in `TerminalModeGuard::enter` and into a small helper or `#[cfg(not(windows))]` block that is visibly guarded for non-Windows platforms.
- Move `PopKeyboardEnhancementFlags` out of the unconditional restore `execute!` call and into a matching helper or `#[cfg(not(windows))]` block that only runs when keyboard enhancement was pushed.
- On Windows, make the push/pop keyboard enhancement path a no-op while preserving the existing raw mode, alternate screen, bracketed paste, cursor style, and cursor show restoration behavior.
- Keep the existing Unix behavior: continue enabling `DISAMBIGUATE_ESCAPE_CODES` on entry and popping the keyboard enhancement flags during restore.
- If imports become platform-specific, gate the keyboard enhancement imports with `#[cfg(not(windows))]` or otherwise avoid unused-import warnings on Windows.

## Tests to be added/updated

- Do not rewrite or replace the existing repro test `crates/tui/src/app/tests.rs::windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows`.
- No additional test is required unless the implementation introduces a separately testable helper; if so, add only a narrow unit test for that helper's push/pop symmetry.

## How to verify

- Run the existing repro test:

```bash
cargo test -p cowboy windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows
```

- Run the relevant TUI app tests to catch regressions in nearby terminal/UI behavior:

```bash
cargo test -p cowboy app::tests
```

- If a Windows environment is available, smoke-test `cowboy` with no subcommand and verify the TUI starts instead of failing with `Keyboard progressive enhancement not implemented for the legacy Windows API.`

## TODO

- [x] In `crates/tui/src/app.rs`, add state to `TerminalModeGuard` recording whether keyboard enhancement was pushed.
- [x] Gate `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` so it is never executed on Windows.
- [x] Gate `PopKeyboardEnhancementFlags` so it only executes on non-Windows when a matching push occurred.
- [x] Preserve non-keyboard terminal setup/restoration behavior for raw mode, alternate screen, bracketed paste, cursor style, and cursor visibility.
- [x] Adjust keyboard enhancement imports to compile cleanly on both Windows and non-Windows targets.
- [x] Run `cargo test -p cowboy windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows`.
- [x] Run `cargo test -p cowboy app::tests`.

## Reviewer feedback TODO

- [x] Fix adjacent import-block spacing in `crates/tui/src/app.rs`.
- [x] Make keyboard enhancement command invocations visible to the existing Windows regression scan while keeping them guarded away from Windows.
- [x] Re-run `cargo test -p cowboy windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows`.
- [x] Re-run `cargo test -p cowboy app::tests`.
