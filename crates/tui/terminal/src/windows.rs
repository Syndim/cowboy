//! Windows keyboard strategy for Cowboy terminal mode.
//!
//! Crossterm's Windows event source reads native console `KeyEventRecord` values.
//! Those records include modifier bits, so Cowboy keeps the Windows terminal on
//! that event path instead of enabling CSI-u keyboard enhancement sequences that
//! the Windows event reader does not decode.

use std::io;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{KeyboardInputStrategy, KeyboardMode};

pub fn keyboard_strategy() -> KeyboardInputStrategy {
    KeyboardInputStrategy::WindowsConsoleKeyRecords
}

pub fn push_keyboard_enhancement_flags(_stdout: &mut io::Stdout) -> Result<KeyboardMode> {
    Ok(KeyboardMode::inactive(keyboard_strategy()))
}

pub fn pop_keyboard_enhancement_flags(_stdout: &mut io::Stdout, _mode: KeyboardMode) -> Result<()> {
    Ok(())
}

pub fn normalize_key_event(event: KeyEvent) -> KeyEvent {
    event
}

pub fn strategy_preserves_key(strategy: KeyboardInputStrategy, event: KeyEvent) -> bool {
    let normalized = normalize_key_event(event);
    match normalized.code {
        KeyCode::Enter => normalized
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL),
        KeyCode::Esc => strategy.delivers_escape_and_control_c(),
        KeyCode::Char('c') => normalized.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_keyboard_strategy_uses_native_console_key_records() {
        let strategy = keyboard_strategy();

        assert_eq!(strategy, KeyboardInputStrategy::WindowsConsoleKeyRecords);
        assert!(strategy.preserves_modified_enter());
        assert!(strategy.delivers_escape_and_control_c());
        assert!(!strategy.emits_csi_u());
    }

    #[test]
    fn windows_keyboard_strategy_preserves_bug_boundary_keys() {
        let strategy = keyboard_strategy();
        let cases = [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ];

        for event in cases {
            assert!(strategy_preserves_key(strategy, event), "{event:?}");
            assert_eq!(normalize_key_event(event), event);
        }
    }

    #[test]
    fn windows_keyboard_strategy_does_not_activate_keyboard_enhancement() {
        let mut stdout = io::stdout();
        let mode = push_keyboard_enhancement_flags(&mut stdout).unwrap();

        assert_eq!(
            mode.strategy(),
            KeyboardInputStrategy::WindowsConsoleKeyRecords
        );
        assert!(!mode.enhancement_active());
        pop_keyboard_enhancement_flags(&mut stdout, mode).unwrap();
    }
}
