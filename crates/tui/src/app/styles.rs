use ratatui::style::{Color, Style};

pub(super) fn style_accent() -> Style {
    Style::default().fg(Color::Cyan)
}

pub(super) fn style_muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub(super) fn style_success() -> Style {
    Style::default().fg(Color::Green)
}

pub(super) fn style_warning() -> Style {
    Style::default().fg(Color::Yellow)
}

pub(super) fn style_error() -> Style {
    Style::default().fg(Color::Red)
}

pub(super) fn style_border() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub(super) fn style_border_accent() -> Style {
    Style::default().fg(Color::Cyan)
}

pub(super) fn style_for_run_state(state: &str) -> Style {
    match state {
        "completed" => style_success(),
        "waiting" | "suspended" => style_warning(),
        "failed" | "cancelled" => style_error(),
        "running" => style_accent(),
        _ => Style::default(),
    }
}

pub(super) fn truncate_to_width(text: impl AsRef<str>, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let text = text.as_ref();
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}
