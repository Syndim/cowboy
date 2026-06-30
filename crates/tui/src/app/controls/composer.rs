use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::super::commands::{
    MAX_SLASH_SUGGESTIONS, slash_query, slash_suggestion_line_count, slash_suggestions,
};
use super::super::state::AppState;
use super::super::styles::{style_accent, style_border_accent, style_muted};

pub(in crate::app) fn height(state: &AppState, terminal_height: u16) -> u16 {
    let wanted = (state.input_line_count() + slash_suggestion_line_count(state.input()) + 2)
        .clamp(3, 12) as u16;
    let max_available = terminal_height.saturating_sub(3).max(3);
    wanted.min(max_available)
}

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let rendered = rendered_input(state, visible_height);
    let composer = Paragraph::new(lines_from_rendered(state, visible_height, &rendered)).block(
        Block::default()
            .title(title(state))
            .borders(Borders::ALL)
            .border_style(style_border_accent()),
    );
    frame.render_widget(composer, area);
    set_input_cursor(frame, area, visible_height, &rendered);
}

pub(in crate::app) fn title(state: &AppState) -> String {
    if state.pending_prompt().is_some() {
        " Enter answers active prompt ─ Shift/Ctrl-Enter newline ".to_string()
    } else {
        " Enter submits ─ Shift/Ctrl-Enter newline ─ type / for commands ".to_string()
    }
}

#[cfg(test)]
fn lines(state: &AppState, max_visible_lines: usize) -> Vec<Line<'static>> {
    if max_visible_lines == 0 {
        return Vec::new();
    }

    let rendered = rendered_input(state, max_visible_lines);
    lines_from_rendered(state, max_visible_lines, &rendered)
}


struct RenderedInput {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_column: usize,
}

fn rendered_input(state: &AppState, max_visible_lines: usize) -> RenderedInput {
    let suggestion_line_count = slash_suggestion_line_count(state.input());
    let input_budget = max_visible_lines
        .saturating_sub(suggestion_line_count)
        .max(1);
    let raw_lines = if state.input().is_empty() {
        vec![""]
    } else {
        state.input().split('\n').collect::<Vec<_>>()
    };
    let cursor_line_text = raw_lines.last().copied().unwrap_or_default();
    let cursor_column = Line::from(format!("> {cursor_line_text}")).width();
    let mut lines = raw_lines
        .into_iter()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>();
    let hidden = lines.len().saturating_sub(input_budget);
    if hidden > 0 {
        if input_budget == 1 {
            lines = lines.split_off(lines.len().saturating_sub(1));
        } else {
            let visible_after_marker = input_budget.saturating_sub(1);
            lines = lines.split_off(lines.len().saturating_sub(visible_after_marker));
            lines.insert(0, format!("> … {hidden} earlier line(s) hidden"));
        }
    }

    RenderedInput {
        cursor_line: lines.len().saturating_sub(1),
        cursor_column,
        lines,
    }
}

fn lines_from_rendered(
    state: &AppState,
    max_visible_lines: usize,
    rendered: &RenderedInput,
) -> Vec<Line<'static>> {
    if max_visible_lines == 0 {
        return Vec::new();
    }

    let mut lines = rendered
        .lines
        .iter()
        .take(max_visible_lines)
        .cloned()
        .map(Line::from)
        .collect::<Vec<_>>();

    append_slash_suggestions(state, &mut lines, max_visible_lines);
    lines.truncate(max_visible_lines);
    lines
}

fn set_input_cursor(
    frame: &mut Frame<'_>,
    area: Rect,
    max_visible_lines: usize,
    rendered: &RenderedInput,
) {
    let inner_width = area.width.saturating_sub(2);
    if inner_width == 0 || max_visible_lines == 0 {
        return;
    }

    let x = area.x
        + 1
        + rendered
            .cursor_column
            .min(inner_width.saturating_sub(1) as usize) as u16;
    let y = area.y
        + 1
        + rendered
            .cursor_line
            .min(max_visible_lines.saturating_sub(1)) as u16;
    frame.set_cursor_position(Position::new(x, y));
}

fn append_slash_suggestions(
    state: &AppState,
    lines: &mut Vec<Line<'static>>,
    max_visible_lines: usize,
) {
    if slash_query(state.input()).is_none() || lines.len() >= max_visible_lines {
        return;
    }

    let suggestions = slash_suggestions(state.input());
    if suggestions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching command. Try /help.",
            style_muted(),
        )));
        return;
    }

    lines.push(Line::from(Span::styled(
        "  slash command suggestions",
        style_accent(),
    )));
    for command in suggestions.iter().take(MAX_SLASH_SUGGESTIONS) {
        if lines.len() >= max_visible_lines {
            return;
        }
        lines.push(Line::from(format!(
            "  {:<28} {}",
            command.usage, command.description
        )));
    }

    let hidden = suggestions.len().saturating_sub(MAX_SLASH_SUGGESTIONS);
    if hidden > 0 && lines.len() < max_visible_lines {
        lines.push(Line::from(Span::styled(
            format!("  … {hidden} more command(s)"),
            style_muted(),
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::config::AppConfig;

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(AppConfig {
            state_dir: dir.path().to_path_buf(),
            workflow_store: dir.path().join("workflow.redb"),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
            ..AppConfig::default()
        })
    }

    #[test]
    fn slash_input_renders_command_suggestions() {
        let mut state = test_state();
        state.push_input("/");

        let rendered = lines(&state, 12)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("> /"));
        assert!(rendered.contains("slash command suggestions"));
        assert!(rendered.contains("/run <request>"));
        assert!(rendered.contains("/workflows"));
    }

    #[test]
    fn slash_suggestions_stop_after_command_arguments_begin() {
        let mut state = test_state();
        state.push_input("/run investigate failing tests");

        let rendered = lines(&state, 12)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("> /run investigate failing tests"));
        assert!(!rendered.contains("slash command suggestions"));
    }

    #[test]
    fn plain_text_hides_slash_suggestions() {
        let mut state = test_state();
        state.push_input("plain request");

        let rendered = lines(&state, 12)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("> plain request"));
        assert!(!rendered.contains("slash command suggestions"));
    }

    #[test]
    fn unknown_slash_prefix_points_to_help() {
        let mut state = test_state();
        state.push_input("/zz");

        let rendered = lines(&state, 12)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("No matching command. Try /help."));
    }

    #[test]
    fn composer_lines_are_clamped_to_visible_height() {
        let mut state = test_state();
        state.push_input("one\ntwo\nthree\nfour\nfive");

        let rendered = lines(&state, 3);

        assert_eq!(rendered.len(), 3);
        assert!(rendered[0].to_string().contains("earlier line"));
        assert_eq!(rendered[2].to_string(), "> five");
    }
}
