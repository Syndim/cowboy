//! Terminal lifecycle support for the Cowboy TUI.
//!
//! This crate is a narrow product seam over `crossterm`. It owns raw-mode,
//! alternate-screen, mouse, bracketed-paste, cursor, and platform keyboard setup
//! policy. Rendering stays in `ratatui` users, and composer editing stays in
//! `tui-input` users.

use std::io;

use anyhow::Result;
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub mod unix;
pub mod windows;

#[cfg(not(windows))]
use unix as platform;
#[cfg(windows)]
use windows as platform;

/// Platform keyboard input strategy selected during terminal setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardInputStrategy {
    /// Use crossterm's Windows console `KeyEventRecord` parser. That parser
    /// preserves modifier state from the native console record, including
    /// Shift/Ctrl with Enter, without switching the terminal to CSI-u mode.
    WindowsConsoleKeyRecords,
    /// Ask the terminal to disambiguate Escape and modified keys with CSI-u
    /// sequences. This is currently used only by Unix terminals, where
    /// crossterm's Unix parser decodes those sequences.
    CsiUDisambiguateEscapeCodes,
}

impl KeyboardInputStrategy {
    pub fn preserves_modified_enter(self) -> bool {
        matches!(
            self,
            Self::WindowsConsoleKeyRecords | Self::CsiUDisambiguateEscapeCodes
        )
    }

    pub fn delivers_escape_and_control_c(self) -> bool {
        matches!(
            self,
            Self::WindowsConsoleKeyRecords | Self::CsiUDisambiguateEscapeCodes
        )
    }

    pub fn emits_csi_u(self) -> bool {
        matches!(self, Self::CsiUDisambiguateEscapeCodes)
    }
}

/// Active keyboard setup state used to restore terminal mode safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardMode {
    strategy: KeyboardInputStrategy,
    enhancement_active: bool,
}

impl KeyboardMode {
    pub fn active(strategy: KeyboardInputStrategy) -> Self {
        Self {
            strategy,
            enhancement_active: true,
        }
    }

    pub fn inactive(strategy: KeyboardInputStrategy) -> Self {
        Self {
            strategy,
            enhancement_active: false,
        }
    }

    pub fn strategy(self) -> KeyboardInputStrategy {
        self.strategy
    }

    pub fn enhancement_active(self) -> bool {
        self.enhancement_active
    }
}

/// Guard for the terminal modes Cowboy enables while the TUI is active.
pub struct TerminalModeGuard {
    raw_mode_active: bool,
    screen_active: bool,
    keyboard_mode: KeyboardMode,
}

impl TerminalModeGuard {
    pub fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut guard = Self {
            raw_mode_active: true,
            screen_active: false,
            keyboard_mode: KeyboardMode::inactive(platform::keyboard_strategy()),
        };

        let mut stdout = io::stdout();
        guard
            .activate_screen(|| enter_terminal_screen(&mut stdout).map_err(anyhow::Error::from))?;
        guard.activate_keyboard(|| platform::push_keyboard_enhancement_flags(&mut stdout))?;
        Ok(guard)
    }

    fn activate_screen(&mut self, activate: impl FnOnce() -> Result<()>) -> Result<()> {
        self.screen_active = true;
        activate()
    }

    fn activate_keyboard(&mut self, activate: impl FnOnce() -> Result<KeyboardMode>) -> Result<()> {
        self.keyboard_mode = activate()?;
        Ok(())
    }

    pub fn restore(&mut self) -> Result<()> {
        let keyboard_mode = self.keyboard_mode;
        self.restore_with(
            || disable_raw_mode().map_err(anyhow::Error::from),
            || {
                let mut stdout = io::stdout();
                platform::pop_keyboard_enhancement_flags(&mut stdout, keyboard_mode)
            },
            || {
                let mut stdout = io::stdout();
                restore_terminal_screen(&mut stdout).map_err(anyhow::Error::from)
            },
        )
    }

    fn restore_with(
        &mut self,
        disable_raw: impl FnOnce() -> Result<()>,
        pop_keyboard_enhancement: impl FnOnce() -> Result<()>,
        restore_screen: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let mut first_error = None;

        if self.raw_mode_active {
            match disable_raw() {
                Ok(()) => self.raw_mode_active = false,
                Err(err) => first_error = Some(err),
            }
        }

        if self.keyboard_mode.enhancement_active() {
            match pop_keyboard_enhancement() {
                Ok(()) => {
                    self.keyboard_mode = KeyboardMode::inactive(self.keyboard_mode.strategy())
                }
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        if self.screen_active {
            match restore_screen() {
                Ok(()) => self.screen_active = false,
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn is_restored(&self) -> bool {
        !self.raw_mode_active && !self.screen_active && !self.keyboard_mode.enhancement_active()
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        if self.is_restored() {
            return;
        }

        if let Err(err) = self.restore() {
            tracing::error!(error = ?err, "failed to restore terminal after TUI exit");
        }
    }
}

pub fn tui_input_cursor_style() -> SetCursorStyle {
    SetCursorStyle::SteadyBlock
}

fn enter_terminal_screen(stdout: &mut impl io::Write) -> io::Result<()> {
    let result = (|| {
        execute!(stdout, EnterAlternateScreen)?;
        execute!(stdout, EnableBracketedPaste)?;
        execute!(stdout, EnableMouseCapture)?;
        execute!(stdout, tui_input_cursor_style())?;
        Ok(())
    })();

    if result.is_err() {
        let _ = restore_terminal_screen(stdout);
    }

    result
}

fn restore_terminal_screen(stdout: &mut impl io::Write) -> io::Result<()> {
    execute!(
        stdout,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape,
        Show
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Write;
    use std::path::Path;

    struct FailAfterMouseCapture {
        bytes: Vec<u8>,
        failures_remaining: usize,
    }

    impl Write for FailAfterMouseCapture {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.failures_remaining > 0
                && self
                    .bytes
                    .windows(b"?1000h".len())
                    .any(|part| part == b"?1000h")
            {
                self.failures_remaining -= 1;
                return Err(io::Error::other("injected terminal setup failure"));
            }

            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn tui_input_cursor_style_uses_steady_block_cursor() {
        assert_eq!(tui_input_cursor_style(), SetCursorStyle::SteadyBlock);
    }

    #[test]
    fn terminal_screen_commands_pair_mouse_capture_and_bracketed_paste() {
        let mut bytes = Vec::new();

        enter_terminal_screen(&mut bytes).unwrap();
        restore_terminal_screen(&mut bytes).unwrap();

        let commands = String::from_utf8(bytes).unwrap();
        assert!(commands.contains("?1000h"), "{commands:?}");
        assert!(commands.contains("?1000l"), "{commands:?}");
        assert!(commands.contains("?2004h"), "{commands:?}");
        assert!(commands.contains("?2004l"), "{commands:?}");
        assert!(commands.contains("\u{1b}[0 q"), "{commands:?}");
        assert!(commands.contains("\u{1b}[?25h"), "{commands:?}");
    }

    #[test]
    fn terminal_screen_setup_failure_rolls_back_mouse_capture() {
        let mut writer = FailAfterMouseCapture {
            bytes: Vec::new(),
            failures_remaining: 1,
        };

        let error = enter_terminal_screen(&mut writer).unwrap_err();

        let commands = String::from_utf8(writer.bytes).unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(writer.failures_remaining, 0);
        assert!(commands.contains("?1000h"), "{commands:?}");
        assert!(commands.contains("?1000l"), "{commands:?}");
        assert!(commands.contains("?2004l"), "{commands:?}");
    }

    #[test]
    fn terminal_guard_retries_screen_cleanup_after_setup_rollback_fails() {
        let mut writer = FailAfterMouseCapture {
            bytes: Vec::new(),
            failures_remaining: 2,
        };
        let mut guard = TerminalModeGuard {
            raw_mode_active: false,
            screen_active: false,
            keyboard_mode: KeyboardMode::inactive(
                KeyboardInputStrategy::CsiUDisambiguateEscapeCodes,
            ),
        };

        let setup_result = guard
            .activate_screen(|| enter_terminal_screen(&mut writer).map_err(anyhow::Error::from));

        assert!(setup_result.is_err());
        assert!(guard.screen_active);
        assert_eq!(writer.failures_remaining, 0);

        guard
            .restore_with(
                || Ok(()),
                || Ok(()),
                || restore_terminal_screen(&mut writer).map_err(anyhow::Error::from),
            )
            .unwrap();

        let commands = String::from_utf8(writer.bytes).unwrap();
        assert!(guard.is_restored());
        assert!(commands.contains("?1000l"), "{commands:?}");
        assert!(commands.contains("?2004l"), "{commands:?}");
    }

    #[test]
    fn terminal_guard_retries_only_failed_restoration_components() {
        let raw_calls = Cell::new(0);
        let keyboard_calls = Cell::new(0);
        let screen_calls = Cell::new(0);
        let mut guard = TerminalModeGuard {
            raw_mode_active: true,
            screen_active: true,
            keyboard_mode: KeyboardMode::active(KeyboardInputStrategy::CsiUDisambiguateEscapeCodes),
        };

        let first_result = guard.restore_with(
            || {
                raw_calls.set(raw_calls.get() + 1);
                Ok(())
            },
            || {
                keyboard_calls.set(keyboard_calls.get() + 1);
                Ok(())
            },
            || {
                screen_calls.set(screen_calls.get() + 1);
                Err(anyhow::anyhow!("injected screen restoration failure"))
            },
        );

        assert!(first_result.is_err());
        assert!(!guard.is_restored());
        assert!(!guard.raw_mode_active);
        assert!(!guard.keyboard_mode.enhancement_active());
        assert!(guard.screen_active);

        guard
            .restore_with(
                || {
                    raw_calls.set(raw_calls.get() + 1);
                    Ok(())
                },
                || {
                    keyboard_calls.set(keyboard_calls.get() + 1);
                    Ok(())
                },
                || {
                    screen_calls.set(screen_calls.get() + 1);
                    Ok(())
                },
            )
            .unwrap();

        assert!(guard.is_restored());
        assert_eq!(raw_calls.get(), 1);
        assert_eq!(keyboard_calls.get(), 1);
        assert_eq!(screen_calls.get(), 2);
    }

    #[test]
    fn terminal_platform_modules_are_windows_and_unix() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rejected_name = ["non", "windows"].join("_");
        assert!(manifest_dir.join("src/windows.rs").exists());
        assert!(manifest_dir.join("src/unix.rs").exists());
        assert!(
            !manifest_dir
                .join("src")
                .join(format!("{rejected_name}.rs"))
                .exists()
        );

        let lib_source = include_str!("lib.rs");
        assert!(lib_source.contains("pub mod windows;"));
        assert!(lib_source.contains("pub mod unix;"));
        assert!(!lib_source.contains(&rejected_name));
    }

    #[test]
    fn terminal_crate_delegates_to_crossterm_without_rendering_or_composer_dependencies() {
        let manifest = include_str!("../Cargo.toml");
        let source = include_str!("lib.rs");

        assert!(manifest.contains("crossterm.workspace = true"));
        assert!(!manifest.contains("ratatui"));
        assert!(!manifest.contains("tui-input"));
        assert!(source.contains("enable_raw_mode"));
        assert!(source.contains("EnterAlternateScreen"));
        assert!(source.contains("EnableMouseCapture"));
        assert!(source.contains("EnableBracketedPaste"));
    }
}
