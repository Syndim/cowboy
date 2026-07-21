//! Reusable terminal animation primitives for Cowboy UI surfaces.
//!
//! This crate owns deterministic frame-cycle behavior only. TUI state,
//! redraw scheduling, styling, and terminal lifecycle stay in app crates.

/// Braille spinner frames used by the TUI running status indicator.
pub const RUNNING_STATUS_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Deterministic cyclic view over a static terminal frame sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCycle {
    frames: &'static [&'static str],
    index: usize,
}

impl FrameCycle {
    /// Creates a frame cycle over a nonempty static frame sequence.
    ///
    /// # Panics
    ///
    /// Panics if `frames` is empty.
    pub fn new(frames: &'static [&'static str]) -> Self {
        assert!(
            !frames.is_empty(),
            "frame cycle requires at least one frame"
        );

        Self { frames, index: 0 }
    }

    /// Creates the standard running-status spinner cycle.
    pub fn running_status() -> Self {
        Self::new(RUNNING_STATUS_FRAMES)
    }

    /// Returns the current frame without advancing the cycle.
    pub fn current(&self) -> &'static str {
        self.frames[self.index]
    }

    /// Advances one frame with wraparound and returns the new current frame.
    pub fn advance(&mut self) -> &'static str {
        self.index = (self.index + 1) % self.frames.len();
        self.current()
    }

    /// Resets the cycle to its first frame.
    pub fn reset(&mut self) {
        self.index = 0;
    }

    /// Returns the static frame sequence backing this cycle.
    pub fn frames(&self) -> &'static [&'static str] {
        self.frames
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use unicode_width::UnicodeWidthStr;

    use super::*;

    #[test]
    fn running_status_frames_are_nonempty_and_animated() {
        assert!(!RUNNING_STATUS_FRAMES.is_empty());

        let distinct = RUNNING_STATUS_FRAMES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert!(distinct.len() >= 2, "spinner must animate across frames");
    }

    #[test]
    fn frame_cycle_advances_deterministically_and_wraps() {
        let mut cycle = FrameCycle::running_status();
        let first = cycle.current();
        let second = cycle.advance();

        assert_eq!(first, RUNNING_STATUS_FRAMES[0]);
        assert_eq!(second, RUNNING_STATUS_FRAMES[1]);

        for _ in 1..RUNNING_STATUS_FRAMES.len() {
            cycle.advance();
        }

        assert_eq!(cycle.current(), first);
    }

    #[test]
    fn reset_returns_to_first_frame() {
        let mut cycle = FrameCycle::running_status();
        cycle.advance();
        cycle.advance();

        assert_ne!(cycle.current(), RUNNING_STATUS_FRAMES[0]);

        cycle.reset();

        assert_eq!(cycle.current(), RUNNING_STATUS_FRAMES[0]);
    }

    #[test]
    fn running_status_frames_have_display_width_one() {
        for frame in RUNNING_STATUS_FRAMES {
            assert_eq!(UnicodeWidthStr::width(*frame), 1, "frame {frame:?}");
        }
    }

    #[test]
    fn animation_crate_has_no_terminal_rendering_or_workflow_dependencies() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in [
            "ratatui",
            "crossterm",
            "cowboy-workflow",
            "cowboy-agent",
            "cowboy-tui-terminal",
        ] {
            assert!(
                !manifest.contains(forbidden),
                "forbidden dependency: {forbidden}"
            );
        }
    }

    #[test]
    fn workspace_manifests_register_animation_crate() {
        let workspace_manifest = include_str!("../../../../Cargo.toml");
        let app_manifest = include_str!("../../app/Cargo.toml");

        assert!(workspace_manifest.contains("crates/tui/animation"));
        assert!(
            workspace_manifest
                .contains("cowboy-tui-animation = { path = \"crates/tui/animation\" }")
        );
        assert!(app_manifest.contains("cowboy-tui-animation.workspace = true"));
    }
}
