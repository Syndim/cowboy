use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

const PROMPT: &str = "> ";
const CONTINUATION_PROMPT: &str = "  ";
const PROMPT_WIDTH: usize = 2;

use super::super::commands::{
    MAX_SLASH_SUGGESTIONS, slash_query, slash_suggestion_line_count, slash_suggestions,
};
use super::super::state::AppState;
use super::super::styles::{style_accent, style_border_accent, style_muted};

// Ratatui wraps Paragraph text and can report the wrapped line count, but it does not
// resize surrounding Layout constraints automatically. The composer still computes
// its own height and cursor position because the input prompt, continuation indent,
// latest-lines clipping, and cursor placement are application-specific.
pub(in crate::app) fn height(state: &AppState, terminal_height: u16, composer_width: u16) -> u16 {
    let input_rows = Paragraph::new(state.input())
        .wrap(Wrap { trim: false })
        .line_count(input_content_width(composer_width) as u16)
        .max(1);
    let wanted = (input_rows + slash_suggestion_line_count(state.input()) + 2).clamp(3, 12) as u16;
    let max_available = terminal_height.saturating_sub(3).max(3);
    wanted.min(max_available)
}

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let content_width = input_content_width(area.width);
    let rendered = rendered_input(state, visible_height, content_width);
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
fn lines(state: &AppState, max_visible_lines: usize, composer_width: u16) -> Vec<Line<'static>> {
    if max_visible_lines == 0 {
        return Vec::new();
    }

    let rendered = rendered_input(
        state,
        max_visible_lines,
        input_content_width(composer_width),
    );
    lines_from_rendered(state, max_visible_lines, &rendered)
}

struct RenderedInput {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_column: usize,
}

struct VisualInputLine {
    line: String,
    cursor_column: usize,
}

fn rendered_input(
    state: &AppState,
    max_visible_lines: usize,
    content_width: usize,
) -> RenderedInput {
    let suggestion_line_count = slash_suggestion_line_count(state.input());
    let input_budget = max_visible_lines
        .saturating_sub(suggestion_line_count)
        .max(1);
    let visual_lines = wrapped_input_lines(state.input(), content_width);
    let cursor_column = visual_lines
        .last()
        .map(|line| line.cursor_column)
        .unwrap_or(PROMPT_WIDTH);
    let mut lines = visual_lines
        .into_iter()
        .map(|visual_line| visual_line.line)
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

fn input_content_width(composer_width: u16) -> usize {
    let inner_width = composer_width.saturating_sub(2) as usize;
    inner_width.saturating_sub(PROMPT_WIDTH).max(1)
}

fn wrapped_input_lines(input: &str, content_width: usize) -> Vec<VisualInputLine> {
    let mut visual_lines = Vec::new();
    if input.is_empty() {
        append_wrapped_input_line("", content_width, &mut visual_lines);
    } else {
        for raw_line in input.split('\n') {
            append_wrapped_input_line(raw_line, content_width, &mut visual_lines);
        }
    }
    visual_lines
}

fn append_wrapped_input_line(
    raw_line: &str,
    content_width: usize,
    visual_lines: &mut Vec<VisualInputLine>,
) {
    if raw_line.is_empty() {
        visual_lines.push(VisualInputLine {
            line: PROMPT.to_string(),
            cursor_column: PROMPT_WIDTH,
        });
        return;
    }

    let mut segment = String::new();
    let mut segment_width = 0;
    let mut first_segment = true;
    for ch in raw_line.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if ch_width > 0 && segment_width > 0 && segment_width + ch_width > content_width {
            push_visual_input_line(&mut segment, segment_width, first_segment, visual_lines);
            segment_width = 0;
            first_segment = false;
        }
        segment.push(ch);
        segment_width += ch_width;
    }
    push_visual_input_line(&mut segment, segment_width, first_segment, visual_lines);
}

fn push_visual_input_line(
    segment: &mut String,
    segment_width: usize,
    first_segment: bool,
    visual_lines: &mut Vec<VisualInputLine>,
) {
    let prefix = if first_segment {
        PROMPT
    } else {
        CONTINUATION_PROMPT
    };
    let mut line = String::with_capacity(prefix.len() + segment.len());
    line.push_str(prefix);
    line.push_str(segment);
    visual_lines.push(VisualInputLine {
        line,
        cursor_column: PROMPT_WIDTH + segment_width,
    });
    segment.clear();
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

        let rendered = lines(&state, 12, 80)
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

        let rendered = lines(&state, 12, 80)
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

        let rendered = lines(&state, 12, 80)
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

        let rendered = lines(&state, 12, 80)
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

        let rendered = lines(&state, 3, 80);

        assert_eq!(rendered.len(), 3);
        assert!(rendered[0].to_string().contains("earlier line"));
        assert_eq!(rendered[2].to_string(), "> five");
    }

    #[test]
    fn height_counts_soft_wrapped_input_rows() {
        let mut state = test_state();
        state.push_input("abcdefghijklmnop");

        assert_eq!(height(&state, 10, 16), 4);
    }

    #[test]
    fn lines_render_soft_wrapped_continuation_rows() {
        let mut state = test_state();
        state.push_input("abcdefghijklmnop");

        let rendered = lines(&state, 4, 16)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        assert_eq!(rendered[0], "> abcdefghijkl");
        assert_eq!(rendered[1], "  mnop");
    }
}
