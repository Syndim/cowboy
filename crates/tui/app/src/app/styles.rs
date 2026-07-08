use ratatui::style::{Color, Style};

pub(super) const COLOR_TRANSCRIPT_NORMAL: Color = Color::White;
pub(super) const COLOR_TRANSCRIPT_METADATA: Color = Color::Rgb(107, 114, 128);
pub(super) const COLOR_TRANSCRIPT_THOUGHT: Color = Color::Rgb(86, 95, 137);
pub(super) const COLOR_TRANSCRIPT_ACCENT: Color = Color::Rgb(122, 162, 247);
pub(super) const COLOR_TRANSCRIPT_TOOL_PENDING: Color = Color::Rgb(42, 195, 222);
pub(super) const COLOR_TRANSCRIPT_SUCCESS: Color = Color::Rgb(158, 206, 106);
pub(super) const COLOR_TRANSCRIPT_WARNING: Color = Color::Rgb(224, 175, 104);
pub(super) const COLOR_TRANSCRIPT_ERROR: Color = Color::Rgb(247, 118, 142);
pub(super) const COLOR_TRANSCRIPT_PLAN: Color = Color::Rgb(187, 154, 247);
pub(super) const COLOR_TRANSCRIPT_PROMPT: Color = Color::Rgb(154, 165, 206);
pub(super) const COLOR_TRANSCRIPT_BORDER: Color = Color::Rgb(59, 66, 97);
pub(super) const COLOR_TRANSCRIPT_CODE_FALLBACK: Color = Color::Rgb(192, 202, 245);

pub(super) fn style_accent() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_ACCENT)
}

pub(super) fn style_muted() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_METADATA)
}

pub(super) fn style_success() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_SUCCESS)
}

pub(super) fn style_warning() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_WARNING)
}

pub(super) fn style_error() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_ERROR)
}

pub(super) fn style_border() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_BORDER)
}

pub(super) fn style_border_accent() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_ACCENT)
}

pub(super) fn style_transcript_normal() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_NORMAL)
}

pub(super) fn style_transcript_metadata() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_METADATA)
}

pub(super) fn style_transcript_thought() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_THOUGHT)
}

pub(super) fn style_transcript_plan() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_PLAN)
}

pub(super) fn style_transcript_prompt() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_PROMPT)
}

pub(super) fn style_transcript_tool_pending() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_TOOL_PENDING)
}

pub(super) fn style_transcript_code_fallback() -> Style {
    Style::default().fg(COLOR_TRANSCRIPT_CODE_FALLBACK)
}

pub(super) fn style_for_run_state(state: &str) -> Style {
    match state {
        "completed" => style_success(),
        "waiting" => style_warning(),
        "failed" | "cancelled" => style_error(),
        "running" => style_accent(),
        _ => Style::default(),
    }
}

pub(super) fn style_for_tool_status(status: &str) -> Style {
    match status.to_ascii_lowercase().as_str() {
        "completed" | "success" | "succeeded" | "done" => style_success(),
        "failed" | "error" | "cancelled" | "canceled" => style_error(),
        "waiting" | "warning" => style_warning(),
        _ => style_transcript_tool_pending(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_palette_uses_concrete_dark_mode_colors() {
        assert_eq!(style_transcript_normal().fg, Some(Color::White));
        assert_eq!(
            style_transcript_metadata().fg,
            Some(Color::Rgb(107, 114, 128))
        );
        assert_eq!(style_transcript_thought().fg, Some(Color::Rgb(86, 95, 137)));
        assert_eq!(
            style_transcript_tool_pending().fg,
            Some(Color::Rgb(42, 195, 222))
        );
        assert_eq!(style_success().fg, Some(Color::Rgb(158, 206, 106)));
        assert_eq!(style_warning().fg, Some(Color::Rgb(224, 175, 104)));
        assert_eq!(style_error().fg, Some(Color::Rgb(247, 118, 142)));
        assert_eq!(style_transcript_plan().fg, Some(Color::Rgb(187, 154, 247)));
        assert_eq!(
            style_transcript_prompt().fg,
            Some(Color::Rgb(154, 165, 206))
        );
        assert_eq!(style_border().fg, Some(Color::Rgb(59, 66, 97)));
        assert_eq!(
            style_transcript_code_fallback().fg,
            Some(Color::Rgb(192, 202, 245))
        );
    }

    #[test]
    fn tool_status_styles_map_to_palette() {
        assert_eq!(
            style_for_tool_status("pending"),
            style_transcript_tool_pending()
        );
        assert_eq!(style_for_tool_status("completed"), style_success());
        assert_eq!(style_for_tool_status("warning"), style_warning());
        assert_eq!(style_for_tool_status("failed"), style_error());
    }
}
