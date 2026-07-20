## Plan

Base the fix on `docs/plans/ctrl_shift_enter_submits_on_windows/rca.md` and keep `crates/tui/app/src/app/tests.rs::app::tests::windows_keyboard_enhancement_path_preserves_modified_enter_support` as the repro input. Fix the terminal setup boundary in `crates/tui/app/src/app.rs`; do not change the input-handler contract in `crates/tui/app/src/app/input.rs` where plain Enter submits and Shift/Ctrl+Enter inserts `\n` when modifiers are present.

The root cause is that the Windows `push_keyboard_enhancement_flags` path currently returns `Ok(false)`, so the TUI never requests `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` on Windows terminals that can preserve modified Enter. The fix should request the keyboard enhancement on ANSI-capable Windows terminals while preserving the legacy-console guard that prevents crossterm from executing `PushKeyboardEnhancementFlags` or `PopKeyboardEnhancementFlags` through the unsupported WinAPI path.

## Changes

- In `crates/tui/app/src/app.rs`, make the Windows keyboard-enhancement imports available to the Windows implementation, including the crossterm `Command` trait if direct ANSI emission is used.
- Replace the Windows `push_keyboard_enhancement_flags` no-op with an ANSI-capability-gated implementation:
  - If `crossterm::ansi_support::supports_ansi()` is false, return `Ok(false)` so `TerminalModeGuard` skips pop on restore.
  - If ANSI is supported, write the ANSI representation of `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` directly to stdout, flush it, and return `Ok(true)`.
  - Do not call `execute!`, `queue!`, or `ExecutableCommand::execute` for this command on Windows because crossterm 0.29 marks `PushKeyboardEnhancementFlags` as not ANSI-supported on Windows and routes those APIs to the unsupported `execute_winapi` path.
- Replace the Windows `pop_keyboard_enhancement_flags` no-op with an active-aware restore implementation:
  - Return immediately when `active` is false.
  - When `active` is true, write the ANSI representation of `PopKeyboardEnhancementFlags` directly to stdout and flush it.
  - Keep restore best-effort behavior centralized in `TerminalModeGuard::restore_with`; do not add terminal lifecycle logic outside `crates/tui/app`.
- Keep the non-Windows push/pop implementations functionally unchanged except for any shared helper extraction needed to avoid duplication.
- Leave `crates/tui/app/src/app/input.rs` behavior unchanged; the correct fix is to preserve modifiers before events reach the handler.

## Tests to be added/updated

- Keep the investigator-added repro test `crates/tui/app/src/app/tests.rs::app::tests::windows_keyboard_enhancement_path_preserves_modified_enter_support` unchanged as the regression gate.
- Keep `crates/tui/app/src/app/tests.rs::app::tests::windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows` passing so the fix cannot reintroduce unguarded crossterm execution of unsupported Windows keyboard enhancement commands.
- Add or update only focused `crates/tui/app/src/app/tests.rs` coverage if the implementation introduces a shared ANSI helper; the test should assert the helper emits the expected keyboard enhancement push/pop sequences without executing the WinAPI path. Do not replace the repro test.
- Existing input-handler tests in `crates/tui/app/src/app/input.rs` should remain unchanged and passing, especially `modified_enter_adds_newline_without_submitting` and `plain_enter_requests_submit_without_mutating_input`.

## How to verify

Run the narrow checks first:

```bash
cargo test -p cowboy windows_keyboard_enhancement_path_preserves_modified_enter_support
cargo test -p cowboy windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows
cargo test -p cowboy modified_enter_adds_newline_without_submitting
cargo test -p cowboy plain_enter_requests_submit_without_mutating_input
```

Then run formatting and the narrow crate lint gate required for a Rust code change:

```bash
cargo fmt --all -- --check
cargo clippy -p cowboy --all-targets -- -D warnings
```

If a Windows machine or CI runner is available, also smoke-test the TUI in a modern ANSI-capable Windows terminal by typing text in the composer, pressing Shift+Enter and Ctrl+Enter, and confirming each inserts a newline instead of submitting. In a legacy Windows console path, startup should still succeed and restoration should not error even if modified Enter cannot be preserved.

## TODO

- [x] Update `crates/tui/app/src/app.rs` Windows imports so the Windows push/pop implementations can build keyboard enhancement commands and emit ANSI directly.
- [x] Implement the Windows `push_keyboard_enhancement_flags` path to return `Ok(false)` without side effects when ANSI is unavailable and to emit `DISAMBIGUATE_ESCAPE_CODES` directly when ANSI is available.
- [x] Implement the Windows `pop_keyboard_enhancement_flags` path to no-op when inactive and directly emit the keyboard enhancement pop sequence when active.
- [x] Preserve the non-Windows push/pop behavior and the existing `TerminalModeGuard` active-state contract.
- [x] Keep the existing input handler unchanged so plain Enter still submits and modified Enter still inserts a newline.
- [x] Keep the investigator-added repro test unchanged and make it pass through the terminal setup fix.
- [x] Add or update focused app tests only if a new helper needs direct coverage; avoid broad or workflow-runtime tests for this terminal setup bug.
- [x] Run the focused cargo tests, formatting check, and narrow clippy gate listed in How to verify.
