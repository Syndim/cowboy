use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PROMPT: &str = "> ";
const CONTINUATION_PROMPT: &str = "  ";
const PROMPT_WIDTH: usize = 2;

use super::super::state::AppState;
use super::super::styles::{style_accent, style_border_accent, style_muted, style_warning};
use cowboy_command_parser::{slash_query, slash_suggestions};

const MAX_SLASH_SUGGESTIONS: usize = 6;

pub(in crate::app) fn height(state: &AppState, terminal_height: u16, composer_width: u16) -> u16 {
    let input_rows =
        wrapped_input_row_count(state.input(), input_content_width(composer_width)).max(1);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComposerVisualState {
    Initial,
    SubmitDisabled,
    WaitingForInput,
}

fn composer_visual_state(state: &AppState) -> ComposerVisualState {
    if state.pending_prompt().is_some() {
        ComposerVisualState::WaitingForInput
    } else if !state.composer_accepts_submit() {
        ComposerVisualState::SubmitDisabled
    } else {
        ComposerVisualState::Initial
    }
}

fn composer_style_for_state(visual_state: ComposerVisualState) -> Style {
    match visual_state {
        ComposerVisualState::Initial => style_border_accent(),
        ComposerVisualState::SubmitDisabled => style_muted(),
        ComposerVisualState::WaitingForInput => style_warning(),
    }
}

fn composer_style(state: &AppState) -> Style {
    composer_style_for_state(composer_visual_state(state))
}

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let content_width = input_content_width(area.width);
    let rendered = rendered_input(state, visible_height, content_width);
    let style = composer_style(state);
    let composer = Paragraph::new(lines_from_rendered(state, visible_height, &rendered)).block(
        Block::default()
            .title(Line::styled(title(state), style))
            .borders(Borders::ALL)
            .border_style(style),
    );
    frame.render_widget(composer, area);
    if state.composer_accepts_edits() {
        set_input_cursor(frame, area, visible_height, &rendered);
    }
}

pub(in crate::app) fn title(state: &AppState) -> String {
    if state.pending_prompt().is_some() {
        " Enter answers active prompt · Shift/Ctrl-Enter newline ".to_string()
    } else if !state.composer_accepts_submit() {
        " Run active · type draft, Enter waits · Esc cancels ".to_string()
    } else {
        " Enter submits · Shift/Ctrl-Enter newline · type / for commands ".to_string()
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

#[derive(Clone, Copy)]
struct SourceGrapheme<'a> {
    text: &'a str,
    source_start: usize,
    source_end: usize,
    width: usize,
}

#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    source_start: usize,
    width: usize,
}

impl<'a> Token<'a> {
    fn source_graphemes(self) -> impl Iterator<Item = SourceGrapheme<'a>> + 'a {
        self.text
            .graphemes(true)
            .scan(self.source_start, |source_index, grapheme| {
                let source_start = *source_index;
                *source_index += grapheme.chars().count();
                Some(SourceGrapheme {
                    text: grapheme,
                    source_start,
                    source_end: *source_index,
                    width: grapheme.width(),
                })
            })
    }

    fn drop_for_wrap(self, available_width: usize) -> Option<Self> {
        let mut dropped_bytes = 0;
        let mut dropped_chars = 0;
        let mut dropped_width = 0;
        let mut remaining_width = available_width;
        let mut graphemes = self.text.grapheme_indices(true).peekable();

        if remaining_width == 0 {
            if let Some((byte_index, grapheme)) = graphemes.next() {
                dropped_bytes = byte_index + grapheme.len();
                dropped_chars = grapheme.chars().count();
                dropped_width = grapheme.width();
            }
        } else {
            while let Some(&(byte_index, grapheme)) = graphemes.peek() {
                let width = grapheme.width();
                if width > remaining_width {
                    break;
                }

                graphemes.next();
                dropped_bytes = byte_index + grapheme.len();
                dropped_chars += grapheme.chars().count();
                dropped_width += width;
                remaining_width -= width;

                if remaining_width == 0 {
                    if let Some((byte_index, grapheme)) = graphemes.next() {
                        dropped_bytes = byte_index + grapheme.len();
                        dropped_chars += grapheme.chars().count();
                        dropped_width += grapheme.width();
                    }

                    break;
                }
            }
        }

        let text = &self.text[dropped_bytes..];
        (!text.is_empty()).then_some(Self {
            text,
            source_start: self.source_start + dropped_chars,
            width: self.width.saturating_sub(dropped_width),
        })
    }
}

trait WrappedRowSink {
    fn begin_row(&mut self, logical_start: usize, segment_index: usize);

    fn push_source(&mut self, source: SourceGrapheme<'_>);

    fn end_row(&mut self, logical_end: Option<usize>);

    fn attach_logical_end(&mut self, logical_end: usize);
}

struct RowEmitter<'a, S> {
    sink: &'a mut S,
    content_width: usize,
    logical_start: usize,
    segment_index: usize,
    row_width: usize,
    row_open: bool,
}

impl<'a, S: WrappedRowSink> RowEmitter<'a, S> {
    fn new(sink: &'a mut S, content_width: usize, logical_start: usize) -> Self {
        Self {
            sink,
            content_width,
            logical_start,
            segment_index: 0,
            row_width: 0,
            row_open: false,
        }
    }

    fn place_word(&mut self, mut whitespace: Option<Token<'_>>, word: Token<'_>) {
        if word.width > self.content_width {
            if let Some(whitespace) = whitespace {
                self.push_breakable_whitespace(whitespace);
            }

            self.push_unbroken(word);
            return;
        }

        let whitespace_width = whitespace.map_or(0, |token| token.width);
        if self.row_width + whitespace_width + word.width <= self.content_width {
            if let Some(whitespace) = whitespace {
                self.push_token(whitespace);
            }

            self.push_unbroken(word);
            return;
        }

        if self.row_width > 0 {
            whitespace = whitespace.and_then(|token| token.drop_for_wrap(self.remaining_width()));
            self.finish_row(None);
        }

        let whitespace_width = whitespace.map_or(0, |token| token.width);
        if whitespace_width + word.width <= self.content_width {
            if let Some(whitespace) = whitespace {
                self.push_token(whitespace);
            }

            self.push_unbroken(word);
            return;
        }

        if let Some(whitespace) = whitespace {
            self.push_breakable_whitespace(whitespace);
            if self.row_open && self.row_width + word.width > self.content_width {
                self.finish_row(None);
            }
        }

        self.push_unbroken(word);
    }

    fn push_breakable_whitespace(&mut self, whitespace: Token<'_>) {
        for source in whitespace.source_graphemes() {
            if self.row_width > 0 && self.row_width >= self.content_width {
                self.finish_row(None);
                continue;
            }

            if source.width > 0
                && self.row_width > 0
                && self.row_width + source.width > self.content_width
            {
                self.finish_row(None);
            }

            self.push_source(source);
        }
    }

    fn push_unbroken(&mut self, token: Token<'_>) {
        for source in token.source_graphemes() {
            if source.width > 0
                && self.row_width > 0
                && self.row_width + source.width > self.content_width
            {
                self.finish_row(None);
            }

            self.push_source(source);
        }
    }

    fn push_token(&mut self, token: Token<'_>) {
        for source in token.source_graphemes() {
            self.push_source(source);
        }
    }

    fn push_source(&mut self, source: SourceGrapheme<'_>) {
        self.ensure_row();
        self.sink.push_source(source);
        self.row_width += source.width;
    }

    fn remaining_width(&self) -> usize {
        self.content_width.saturating_sub(self.row_width)
    }

    fn ensure_row(&mut self) {
        if self.row_open {
            return;
        }

        self.sink.begin_row(self.logical_start, self.segment_index);
        self.row_open = true;
    }

    fn finish_row(&mut self, logical_end: Option<usize>) {
        self.ensure_row();
        self.sink.end_row(logical_end);
        self.segment_index += 1;
        self.row_width = 0;
        self.row_open = false;
    }

    fn finish_logical_line(&mut self, logical_end: usize) {
        if self.row_open || self.segment_index == 0 {
            self.finish_row(Some(logical_end));
        } else {
            self.sink.attach_logical_end(logical_end);
        }
    }
}

struct RenderedRowSink {
    lines: Vec<String>,
    current_line: String,
    current_column: usize,
    cursor: usize,
    direct_cursor: Option<(usize, usize)>,
    previous_cursor: Option<(usize, usize)>,
    next_cursor: Option<(usize, usize)>,
}

impl RenderedRowSink {
    fn new(cursor: usize) -> Self {
        Self {
            lines: Vec::new(),
            current_line: String::new(),
            current_column: 0,
            cursor,
            direct_cursor: None,
            previous_cursor: None,
            next_cursor: None,
        }
    }

    fn observe_boundary(&mut self, source_index: usize) {
        self.observe_boundary_at(source_index, (self.lines.len(), self.current_column));
    }

    fn observe_boundary_at(&mut self, source_index: usize, position: (usize, usize)) {
        if source_index == self.cursor {
            self.direct_cursor.get_or_insert(position);
        } else if source_index < self.cursor {
            self.previous_cursor = Some(position);
        } else if self.next_cursor.is_none() {
            self.next_cursor = Some(position);
        }
    }

    fn finish(self) -> WrappedInput {
        let (cursor_line, cursor_content_column) = self
            .direct_cursor
            .or(self.next_cursor)
            .or(self.previous_cursor)
            .unwrap_or((self.lines.len().saturating_sub(1), 0));

        WrappedInput {
            lines: self.lines,
            cursor_line,
            cursor_column: PROMPT_WIDTH + cursor_content_column,
        }
    }
}

impl WrappedRowSink for RenderedRowSink {
    fn begin_row(&mut self, logical_start: usize, segment_index: usize) {
        self.current_line.clear();
        self.current_line.push_str(if segment_index == 0 {
            PROMPT
        } else {
            CONTINUATION_PROMPT
        });
        self.current_column = 0;

        if segment_index == 0 {
            self.observe_boundary(logical_start);
        }
    }

    fn push_source(&mut self, source: SourceGrapheme<'_>) {
        let start_column = self.current_column;
        self.current_line.push_str(source.text);
        self.observe_boundary(source.source_start);

        if self.cursor > source.source_start && self.cursor < source.source_end {
            let scalar_offset = self.cursor - source.source_start;
            let prefix_end = source
                .text
                .char_indices()
                .nth(scalar_offset)
                .map(|(byte_index, _)| byte_index)
                .expect("cursor inside grapheme has a prefix boundary");
            self.current_column = start_column + source.text[..prefix_end].width();
            self.observe_boundary(self.cursor);
        }

        self.current_column = start_column + source.width;
        self.observe_boundary(source.source_end);
    }

    fn end_row(&mut self, logical_end: Option<usize>) {
        if let Some(logical_end) = logical_end {
            self.observe_boundary(logical_end);
        }

        self.lines.push(std::mem::take(&mut self.current_line));
    }

    fn attach_logical_end(&mut self, logical_end: usize) {
        let position = (self.lines.len().saturating_sub(1), self.current_column);
        self.observe_boundary_at(logical_end, position);
    }
}

#[derive(Default)]
struct RowCountSink {
    count: usize,
}

impl WrappedRowSink for RowCountSink {
    fn begin_row(&mut self, _logical_start: usize, _segment_index: usize) {}

    fn push_source(&mut self, _source: SourceGrapheme<'_>) {}

    fn attach_logical_end(&mut self, _logical_end: usize) {}

    fn end_row(&mut self, _logical_end: Option<usize>) {
        self.count += 1;
    }
}

fn wrapped_input_lines(input: &str, cursor: usize, content_width: usize) -> WrappedInput {
    let mut sink = RenderedRowSink::new(cursor);
    wrap_input(input, content_width, &mut sink);
    sink.finish()
}

fn wrapped_input_row_count(input: &str, content_width: usize) -> usize {
    let mut sink = RowCountSink::default();
    wrap_input(input, content_width, &mut sink);
    sink.count
}

fn wrap_input(input: &str, content_width: usize, sink: &mut impl WrappedRowSink) {
    let mut logical_start = 0;
    let mut logical_lines = input.split('\n').peekable();

    while let Some(logical_line) = logical_lines.next() {
        let logical_end = logical_start + logical_line.chars().count();
        wrap_logical_line(
            logical_line,
            logical_start,
            logical_end,
            content_width,
            sink,
        );
        logical_start = logical_end + usize::from(logical_lines.peek().is_some());
    }
}

fn wrap_logical_line(
    logical_line: &str,
    logical_start: usize,
    logical_end: usize,
    content_width: usize,
    sink: &mut impl WrappedRowSink,
) {
    let mut emitter = RowEmitter::new(sink, content_width, logical_start);
    let mut pending_whitespace = None;

    for_each_token(logical_line, logical_start, |token, is_whitespace| {
        if is_whitespace {
            pending_whitespace = Some(token);
        } else {
            emitter.place_word(pending_whitespace.take(), token);
        }
    });

    if let Some(whitespace) = pending_whitespace {
        emitter.push_breakable_whitespace(whitespace);
    }

    emitter.finish_logical_line(logical_end);
}

fn for_each_token<'a>(
    logical_line: &'a str,
    source_start: usize,
    mut visit: impl FnMut(Token<'a>, bool),
) {
    let mut graphemes = logical_line.grapheme_indices(true).scan(
        source_start,
        |source_index, (byte_index, grapheme)| {
            let current_source_index = *source_index;
            *source_index += grapheme.chars().count();
            Some((current_source_index, byte_index, grapheme))
        },
    );
    let Some((_, _, first)) = graphemes.next() else {
        return;
    };

    let mut token_start_byte = 0;
    let mut token_source_start = source_start;
    let mut token_width = first.width();
    let mut token_is_whitespace = is_breakable_whitespace(first);

    for (source_index, byte_index, grapheme) in graphemes {
        let is_whitespace = is_breakable_whitespace(grapheme);
        if is_whitespace != token_is_whitespace {
            visit(
                Token {
                    text: &logical_line[token_start_byte..byte_index],
                    source_start: token_source_start,
                    width: token_width,
                },
                token_is_whitespace,
            );
            token_start_byte = byte_index;
            token_source_start = source_index;
            token_width = 0;
            token_is_whitespace = is_whitespace;
        }

        token_width += grapheme.width();
    }

    visit(
        Token {
            text: &logical_line[token_start_byte..],
            source_start: token_source_start,
            width: token_width,
        },
        token_is_whitespace,
    );
}

fn is_breakable_whitespace(grapheme: &str) -> bool {
    grapheme == "\u{200b}" || (grapheme != "\u{00a0}" && grapheme.chars().all(char::is_whitespace))
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
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(AppConfig {
            state_dir: dir.path().to_path_buf(),
            workflow_store: dir.path().join("workflow.redb"),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
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

    fn apply_waiting_prompt(state: &mut AppState) {
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WaitingForInput {
                step: "confirm_result".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: Vec::new(),
            },
        ));
        assert!(state.pending_prompt().is_some());
    }

    #[test]
    fn idle_composer_style_uses_border_accent() {
        let state = test_state();

        assert!(state.composer_accepts_submit());
        assert_eq!(composer_style(&state).fg, style_border_accent().fg);
    }

    #[tokio::test]
    async fn active_background_run_without_prompt_uses_muted_style() {
        let mut state = test_state();
        lock_composer_with_pending_task(&mut state);

        assert!(state.pending_prompt().is_none());
        assert_eq!(composer_style(&state).fg, style_muted().fg);

        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn pending_prompt_uses_warning_style_and_wins_over_active_background_task() {
        let mut state = test_state();
        lock_composer_with_pending_task(&mut state);
        apply_waiting_prompt(&mut state);

        assert_eq!(state.background_task_count(), 1);
        assert_eq!(composer_style(&state).fg, style_warning().fg);

        state.cancel_background_tasks();
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
        assert!(rendered.contains("/resume <run-id>"));
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

    #[test]
    fn idle_composer_title_uses_middle_dot_and_preserves_key_hyphen() {
        let state = test_state();

        let title = title(&state);

        assert_eq!(
            title,
            " Enter submits · Shift/Ctrl-Enter newline · type / for commands "
        );
        assert!(title.contains("Shift/Ctrl-Enter"));
        assert!(!title.contains(" ─ "));
    }

    #[test]
    fn pending_prompt_composer_title_uses_middle_dot_and_preserves_key_hyphen() {
        let mut state = test_state();
        apply_waiting_prompt(&mut state);

        let title = title(&state);

        assert_eq!(
            title,
            " Enter answers active prompt · Shift/Ctrl-Enter newline "
        );
        assert!(title.contains("Shift/Ctrl-Enter"));
        assert!(!title.contains(" ─ "));
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
            " Run active · type draft, Enter waits · Esc cancels "
        );
        assert_eq!(height(&state, 10, 80), 3);
        assert!(rendered.contains("> /"));
        assert!(!rendered.contains("Input disabled"));
        assert!(!rendered.contains("input disabled"));
        assert!(!rendered.contains("slash command suggestions"));
        assert!(!rendered.contains("/resume <run-id>"));
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
    fn wrapped_input_preserves_wide_whitespace() {
        let input = "aaaa\u{3000}b";
        let after_whitespace = wrapped_input_lines(input, 5, 5);
        let at_end = wrapped_input_lines(input, input.chars().count(), 5);

        assert_eq!(after_whitespace.lines, ["> aaaa", "  \u{3000}b"]);
        assert_eq!(after_whitespace.cursor_line, 1);
        assert_eq!(after_whitespace.cursor_column, PROMPT_WIDTH + 2);
        assert_eq!(at_end.cursor_line, 1);
        assert_eq!(at_end.cursor_column, PROMPT_WIDTH + 3);

        let trailing = wrapped_input_lines("aaaa\u{3000}", 5, 5);
        assert_eq!(trailing.lines, ["> aaaa", "  \u{3000}"]);
        assert_eq!(trailing.cursor_line, 1);
        assert_eq!(trailing.cursor_column, PROMPT_WIDTH + 2);
    }

    #[test]
    fn wrapped_input_only_consumes_zwsp_before_visible_space() {
        let input = "abc\u{200b} d";
        let before_zwsp = wrapped_input_lines(input, 3, 3);
        let after_zwsp = wrapped_input_lines(input, 4, 3);

        assert_eq!(before_zwsp.lines, ["> abc", "   d"]);
        assert_eq!(before_zwsp.cursor_line, 0);
        assert_eq!(before_zwsp.cursor_column, PROMPT_WIDTH + 3);
        assert_eq!(after_zwsp.cursor_line, 1);
        assert_eq!(after_zwsp.cursor_column, PROMPT_WIDTH);
    }

    #[test]
    fn wrapped_input_keeps_word_after_partial_leading_whitespace_row() {
        let input = "               4";
        let wrapped = wrapped_input_lines(input, input.chars().count(), 10);

        assert_eq!(wrapped.lines, [">           ", "      4"]);
        assert_eq!(wrapped.cursor_line, 1);
        assert_eq!(wrapped.cursor_column, PROMPT_WIDTH + 5);
    }

    #[test]
    fn wrapped_input_keeps_combining_mark_with_whitespace_base() {
        let input = "ab \u{0301}c";
        let before_mark = wrapped_input_lines(input, 3, 3);
        let after_mark = wrapped_input_lines(input, 4, 3);

        assert_eq!(before_mark.lines, ["> ab \u{0301}", "  c"]);
        assert_eq!(before_mark.cursor_line, 0);
        assert_eq!(before_mark.cursor_column, PROMPT_WIDTH + 3);
        assert_eq!(after_mark.cursor_line, 0);
        assert_eq!(after_mark.cursor_column, PROMPT_WIDTH + 3);
    }

    #[test]
    fn wrapped_input_uses_remaining_width_before_splitting_overlong_word() {
        let wrapped = wrapped_input_lines("   abcdef", "   abcdef".chars().count(), 5);

        assert_eq!(wrapped.lines, [">    ab", "  cdef"]);
    }

    #[test]
    fn wrapped_input_does_not_break_at_nbsp() {
        let input = "a\u{00a0}bb";

        let after_nbsp = wrapped_input_lines(input, 2, 3);
        let at_end = wrapped_input_lines(input, input.chars().count(), 3);

        assert_eq!(after_nbsp.lines, ["> a\u{00a0}b", "  b"]);
        assert_eq!(after_nbsp.cursor_line, 0);
        assert_eq!(after_nbsp.cursor_column, PROMPT_WIDTH + 2);
        assert_eq!(at_end.cursor_line, 1);
        assert_eq!(at_end.cursor_column, PROMPT_WIDTH + 1);
    }

    #[test]
    fn wrapped_input_breaks_at_zwsp() {
        let input = "ab\u{200b}cd";

        let before_zwsp = wrapped_input_lines(input, 2, 3);
        let after_zwsp = wrapped_input_lines(input, 3, 3);

        assert_eq!(before_zwsp.lines, ["> ab", "  cd"]);
        assert_eq!(before_zwsp.cursor_line, 0);
        assert_eq!(before_zwsp.cursor_column, PROMPT_WIDTH + 2);
        assert_eq!(after_zwsp.cursor_line, 1);
        assert_eq!(after_zwsp.cursor_column, PROMPT_WIDTH);
    }

    #[test]
    fn wrapped_input_keeps_fitting_word_after_multiple_spaces() {
        let wrapped = wrapped_input_lines("a        bbb", "a        bbb".chars().count(), 5);

        assert_eq!(wrapped.lines, ["> a", "     ", "  bbb"]);
    }

    #[test]
    fn wrapped_input_keeps_fitting_word_after_leading_whitespace() {
        let wrapped = wrapped_input_lines("   bbb", "   bbb".chars().count(), 5);

        assert_eq!(wrapped.lines, [">    ", "  bbb"]);
    }

    #[test]
    fn wrapped_input_keeps_overwide_character_on_one_fallback_row() {
        let wrapped = wrapped_input_lines("中", 1, 1);

        assert_eq!(wrapped.lines, ["> 中"]);
        assert_eq!(wrapped.cursor_line, 0);
        assert_eq!(wrapped.cursor_column, PROMPT_WIDTH + 2);
    }

    #[test]
    fn wrapped_input_moves_fitting_word_intact() {
        let wrapped = wrapped_input_lines("hello bananas", "hello bananas".chars().count(), 12);

        assert_eq!(wrapped.lines, ["> hello", "  bananas"]);
    }

    #[test]
    fn wrapped_cursor_follows_word_moved_to_continuation_row() {
        for (cursor, expected_column) in [(6, 2), (9, 5), (13, 9)] {
            let wrapped = wrapped_input_lines("hello bananas", cursor, 12);

            assert_eq!(wrapped.cursor_line, 1, "cursor {cursor}");
            assert_eq!(wrapped.cursor_column, expected_column, "cursor {cursor}");
        }
    }

    #[test]
    fn emoji_presentation_cursor_uses_grapheme_prefix_width() {
        let cases = [
            (
                "ab ☺️",
                ["> ab", "  ☺️"],
                &[(0, 2), (0, 3), (0, 4), (1, 2), (1, 3), (1, 4)][..],
            ),
            (
                "ab 1️⃣",
                ["> ab", "  1️⃣"],
                &[(0, 2), (0, 3), (0, 4), (1, 2), (1, 3), (1, 4), (1, 4)][..],
            ),
        ];

        for (input, expected_lines, expected_positions) in cases {
            for (cursor, &(expected_line, expected_column)) in expected_positions.iter().enumerate()
            {
                let wrapped = wrapped_input_lines(input, cursor, 4);

                assert_eq!(
                    wrapped.lines, expected_lines,
                    "input {input:?}, cursor {cursor}"
                );
                assert_eq!(
                    (wrapped.cursor_line, wrapped.cursor_column),
                    (expected_line, expected_column),
                    "input {input:?}, cursor {cursor}"
                );
            }
        }
    }

    #[test]
    fn long_combining_grapheme_cursor_maps_start_interior_and_end() {
        let combining_marks = 50_000;
        let input = format!("a{}", "\u{0301}".repeat(combining_marks));
        let expected_line = format!("> {input}");

        for (cursor, expected_column) in [
            (0, PROMPT_WIDTH),
            (1 + combining_marks / 2, PROMPT_WIDTH + 1),
            (1 + combining_marks, PROMPT_WIDTH + 1),
        ] {
            let wrapped = wrapped_input_lines(&input, cursor, 4);

            assert_eq!(wrapped.lines, [expected_line.as_str()]);
            assert_eq!(wrapped.cursor_line, 0, "cursor {cursor}");
            assert_eq!(wrapped.cursor_column, expected_column, "cursor {cursor}");
        }
    }

    #[test]
    fn moved_unicode_word_cursor_uses_display_width() {
        let input = "ab 中\u{0301}文";
        let cursor = "ab 中\u{0301}".chars().count();

        let wrapped = wrapped_input_lines(input, cursor, 6);

        assert_eq!(wrapped.lines, ["> ab", "  中\u{0301}文"]);
        assert_eq!(wrapped.cursor_line, 1);
        assert_eq!(wrapped.cursor_column, PROMPT_WIDTH + 2);
    }

    #[test]
    fn dropped_trailing_space_keeps_explicit_newline_cursor_boundary() {
        let input = "abcde \nx";

        let before_newline = wrapped_input_lines(input, 6, 5);
        let after_newline = wrapped_input_lines(input, 7, 5);

        assert_eq!(before_newline.lines, ["> abcde", "> x"]);
        assert_eq!(before_newline.cursor_line, 0);
        assert_eq!(before_newline.cursor_column, PROMPT_WIDTH + 5);
        assert_eq!(after_newline.lines, ["> abcde", "> x"]);
        assert_eq!(after_newline.cursor_line, 1);
        assert_eq!(after_newline.cursor_column, PROMPT_WIDTH);
        assert_eq!(
            wrapped_input_row_count(input, 5),
            before_newline.lines.len()
        );

        let mut state = test_state();
        state.push_input(input);
        state.set_input_cursor(6);
        let clipped = rendered_input(&state, 1, 5);

        assert_eq!(clipped.lines, ["> abcde"]);
        assert_eq!(clipped.cursor_line, 0);
        assert_eq!(clipped.cursor_column, PROMPT_WIDTH + 5);
    }

    #[test]
    fn row_count_matches_rendered_segments_at_wrap_boundaries() {
        for (input, content_width) in [
            ("abcde \nx", 5),
            ("aaaa\u{3000}b", 5),
            ("aaaa\u{3000}", 5),
            ("abc\u{200b} d", 3),
            ("               4", 10),
            ("ab \u{0301}c", 3),
            ("   abcdef", 5),
            ("a\u{00a0}bb", 3),
            ("ab\u{200b}cd", 3),
            ("a        bbb", 5),
            ("   bbb", 5),
            ("中", 1),
            ("one\ntwo", 5),
        ] {
            let wrapped = wrapped_input_lines(input, input.chars().count(), content_width);

            assert_eq!(
                wrapped_input_row_count(input, content_width),
                wrapped.lines.len(),
                "input {input:?} at width {content_width}"
            );
        }
    }

    #[test]
    fn height_and_rendered_rows_share_word_wrap_layout() {
        let mut state = test_state();
        state.push_input("hello bananas");

        let rendered = rendered_input(&state, usize::MAX, input_content_width(16));

        assert_eq!(rendered.lines, ["> hello", "  bananas"]);
        assert_eq!(height(&state, 20, 16) as usize - 2, rendered.lines.len());
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
