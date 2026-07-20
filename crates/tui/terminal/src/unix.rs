//! Unix keyboard setup for Cowboy terminal mode.

use std::io;

use anyhow::Result;
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;

use crate::{KeyboardInputStrategy, KeyboardMode};

pub fn keyboard_strategy() -> KeyboardInputStrategy {
    KeyboardInputStrategy::CsiUDisambiguateEscapeCodes
}

pub fn push_keyboard_enhancement_flags(stdout: &mut io::Stdout) -> Result<KeyboardMode> {
    execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    Ok(KeyboardMode::active(keyboard_strategy()))
}

pub fn pop_keyboard_enhancement_flags(stdout: &mut io::Stdout, mode: KeyboardMode) -> Result<()> {
    if !mode.enhancement_active() {
        return Ok(());
    }

    execute!(stdout, PopKeyboardEnhancementFlags)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_keyboard_strategy_uses_csi_u_disambiguation() {
        let strategy = keyboard_strategy();

        assert_eq!(strategy, KeyboardInputStrategy::CsiUDisambiguateEscapeCodes);
        assert!(strategy.preserves_modified_enter());
        assert!(strategy.delivers_escape_and_control_c());
        assert!(strategy.emits_csi_u());
    }
}
