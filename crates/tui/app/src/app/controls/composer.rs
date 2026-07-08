use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

const PROMPT: &str = "> ";
const CONTINUATION_PROMPT: &str = "  ";
const PROMPT_WIDTH: usize = 2;

use super::super::state::AppState;
use super::super::styles::{style_accent, style_border_accent, style_muted};
use cowboy_command_parser::{slash_query, slash_suggestions};

const MAX_SLASH_SUGGESTIONS: usize = 6;

// Ratatui wraps Paragraph text and can report the wrapped line count, but it does not
// resize surrounding Layout constraints automatically. The composer still computes
// its own height and cursor position because the input prompt, continuation indent,
// latest-lines clipping, and cursor placement are application-specific.
pub(in crate::app) fn height(state: &AppState, terminal_height: u16, composer_width: u16) -> u16 {
    let input_rows = Paragraph::new(state.input())
        .wrap(Wrap { trim: false })
        .line_count(input_content_width(composer_width) as u16)
        .max(1);
    let suggestion_rows = if state.composer_accepts_submit() {
        slash_suggestion_line_count(state.input())
    } else {
        0
    };
    let wanted = (input_rows + suggestion_rows + 2).clamp(3, 12) as u16;
    let max_available = terminal_height.saturating_sub(3).max(3);
    wanted.min(max_available)
}

fn slash_suggestion_line_count(input: &str) -> usize {
    if slash_query(input).is_none() {
        return 0;
    }

    let suggestions = slash_suggestions(input);
    if suggestions.is_empty() {
        1
    } else {
        let hidden = suggestions.len().saturating_sub(MAX_SLASH_SUGGESTIONS);
        1 + suggestions.len().min(MAX_SLASH_SUGGESTIONS) + usize::from(hidden > 0)
    }
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
    if state.composer_accepts_edits() {
        set_input_cursor(frame, area, visible_height, &rendered);
    }
}

pub(in crate::app) fn title(state: &AppState) -> String {
    if state.pending_prompt().is_some() {
        " Enter answers active prompt ─ Shift/Ctrl-Enter newline ".to_string()
    } else if !state.composer_accepts_submit() {
        " Run active ─ type draft, Enter waits ─ Esc cancels ".to_string()
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

struct WrappedInput {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_column: usize,
}

fn rendered_input(
    state: &AppState,
    max_visible_lines: usize,
    content_width: usize,
) -> RenderedInput {
    let suggestion_line_count = if state.composer_accepts_submit() {
        slash_suggestion_line_count(state.input())
    } else {
        0
    };
    let input_budget = max_visible_lines
        .saturating_sub(suggestion_line_count)
        .max(1);
    let wrapped = wrapped_input_lines(state.input(), state.input_cursor(), content_width);
    let mut lines = wrapped.lines;
    let mut cursor_line = wrapped.cursor_line;
    let cursor_column = wrapped.cursor_column;
    let hidden = lines.len().saturating_sub(input_budget);
    if hidden > 0 {
        if input_budget == 1 {
            let start = cursor_line.min(lines.len().saturating_sub(1));
            lines = vec![lines[start].clone()];
            cursor_line = 0;
        } else {
            let visible_after_marker = input_budget.saturating_sub(1);
            let tail_start = lines.len().saturating_sub(visible_after_marker);
            let start = if cursor_line >= tail_start {
                tail_start
            } else {
                cursor_line.saturating_sub(visible_after_marker.saturating_sub(1))
            };
            if start == 0 {
                lines.truncate(input_budget);
            } else {
                let end = (start + visible_after_marker).min(lines.len());
                lines = lines[start..end].to_vec();
                lines.insert(0, format!("> … {start} earlier line(s) hidden"));
                cursor_line = 1 + cursor_line.saturating_sub(start);
            }
        }
    }

    RenderedInput {
        lines,
        cursor_line,
        cursor_column,
    }
}

fn input_content_width(composer_width: u16) -> usize {
    let inner_width = composer_width.saturating_sub(2) as usize;
    inner_width.saturating_sub(PROMPT_WIDTH).max(1)
}

fn wrapped_input_lines(input: &str, cursor: usize, content_width: usize) -> WrappedInput {
    let mut lines = Vec::new();
    let mut segment = String::new();
    let mut segment_width = 0;
    let mut first_segment = true;
    let mut cursor_line = None;
    let mut cursor_column = PROMPT_WIDTH;
    let mut codepoint_index = 0;

    for ch in input.chars() {
        capture_cursor(
            codepoint_index,
            cursor,
            lines.len(),
            segment_width,
            &mut cursor_line,
            &mut cursor_column,
        );
        if ch == '\n' {
            push_visual_input_line(&mut segment, first_segment, &mut lines);
            segment_width = 0;
            first_segment = true;
        } else {
            let ch_width = ch.width().unwrap_or(0);
            if ch_width > 0 && segment_width > 0 && segment_width + ch_width > content_width {
                push_visual_input_line(&mut segment, first_segment, &mut lines);
                segment_width = 0;
                first_segment = false;
            }
            segment.push(ch);
            segment_width += ch_width;
        }
        codepoint_index += 1;
    }

    capture_cursor(
        codepoint_index,
        cursor,
        lines.len(),
        segment_width,
        &mut cursor_line,
        &mut cursor_column,
    );
    push_visual_input_line(&mut segment, first_segment, &mut lines);

    WrappedInput {
        cursor_line: cursor_line.unwrap_or_else(|| lines.len().saturating_sub(1)),
        cursor_column,
        lines,
    }
}

fn capture_cursor(
    codepoint_index: usize,
    cursor: usize,
    line_index: usize,
    segment_width: usize,
    cursor_line: &mut Option<usize>,
    cursor_column: &mut usize,
) {
    if cursor_line.is_some() || codepoint_index != cursor {
        return;
    }
    *cursor_line = Some(line_index);
    *cursor_column = PROMPT_WIDTH + segment_width;
}

fn push_visual_input_line(
    segment: &mut String,
    first_segment: bool,
    visual_lines: &mut Vec<String>,
) {
    let prefix = if first_segment {
        PROMPT
    } else {
        CONTINUATION_PROMPT
    };
    let mut line = String::with_capacity(prefix.len() + segment.len());
    line.push_str(prefix);
    line.push_str(segment);
    visual_lines.push(line);
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
    if !state.composer_accepts_submit()
        || slash_query(state.input()).is_none()
        || lines.len() >= max_visible_lines
    {
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

    fn lock_composer_with_pending_task(state: &mut AppState) {
        state.spawn_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });
        assert!(state.composer_accepts_edits());
        assert!(!state.composer_accepts_submit());
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
        assert!(rendered.contains("/resume [run-id]"));
        assert!(rendered.contains("more command(s)"));
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

    #[tokio::test]
    async fn active_run_composer_uses_draft_title_and_hides_submit_affordances() {
        let mut state = test_state();
        state.push_input("/");
        let enabled = lines(&state, 12, 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(enabled.contains("slash command suggestions"));

        lock_composer_with_pending_task(&mut state);

        let rendered = lines(&state, 12, 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            title(&state),
            " Run active ─ type draft, Enter waits ─ Esc cancels "
        );
        assert_eq!(height(&state, 10, 80), 3);
        assert!(rendered.contains("> /"));
        assert!(!rendered.contains("Input disabled"));
        assert!(!rendered.contains("input disabled"));
        assert!(!rendered.contains("slash command suggestions"));
        assert!(!rendered.contains("/resume [run-id]"));
        state.cancel_background_tasks();
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

    #[test]
    fn rendered_cursor_uses_moved_wrapped_input_position() {
        let mut state = test_state();
        state.push_input("abcdefghijklmnop");
        state.set_input_cursor("abcdefghijkl".chars().count());

        let rendered = rendered_input(&state, 4, input_content_width(16));

        assert_eq!(rendered.cursor_line, 0);
        assert_eq!(rendered.cursor_column, PROMPT_WIDTH + 12);
    }

    #[test]
    fn rendered_cursor_uses_unicode_display_width() {
        let mut state = test_state();
        state.push_input("a中b");
        state.set_input_cursor("a中".chars().count());

        let rendered = rendered_input(&state, 4, input_content_width(80));

        assert_eq!(rendered.cursor_line, 0);
        assert_eq!(rendered.cursor_column, PROMPT_WIDTH + 3);
    }

    #[test]
    fn clipped_input_keeps_moved_cursor_visible() {
        let mut state = test_state();
        state.push_input("one\ntwo\nthree\nfour\nfive");
        state.set_input_cursor("one\n".chars().count());

        let rendered = rendered_input(&state, 3, input_content_width(80));

        assert_eq!(rendered.lines.len(), 3);
        assert_eq!(rendered.lines[1], "> two");
        assert_eq!(rendered.cursor_line, 1);
    }
}
