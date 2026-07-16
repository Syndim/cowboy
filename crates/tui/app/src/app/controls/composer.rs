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
    let content_width = input_content_width(composer_width);
    let input_rows = wrapped_input_row_count(state.input(), content_width).max(1);
    let suggestion_rows = if state.composer_accepts_submit() {
        slash_suggestion_line_count(state.input())
    } else {
        0
    };
    let wanted = (input_rows + suggestion_rows + 2).clamp(3, 12) as u16;
    let max_available = terminal_height.saturating_sub(3).max(3);
    let resolved_height = wanted.min(max_available);
    let visible_rows = resolved_height.saturating_sub(2) as usize;
    let input_budget = visible_rows.saturating_sub(suggestion_rows).max(1);
    state.publish_composer_layout(content_width, input_budget);
    resolved_height
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

#[cfg(test)]
struct WrappedInput {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_column: usize,
}

#[derive(Clone, Copy)]
struct CursorPoint {
    source: usize,
    column: usize,
}

struct VisualRow {
    text: String,
    source_start: usize,
    source_end: usize,
    display_width: usize,
    last_grapheme_width: usize,
    is_last_logical_segment: bool,
    boundaries: Vec<CursorPoint>,
    interior_cursor: Option<CursorPoint>,
}

impl VisualRow {
    fn max_cursor_column(&self) -> usize {
        if self.is_last_logical_segment {
            self.display_width
        } else {
            self.display_width.saturating_sub(self.last_grapheme_width)
        }
    }

    fn cursor_for_column(&self, column: usize) -> usize {
        let mut cursor = self.source_start;
        for point in &self.boundaries {
            if point.column > column {
                break;
            }

            cursor = point.source;
        }

        cursor.min(self.source_end)
    }
}

struct VisualLayout {
    rows: Vec<VisualRow>,
    cursor_line: usize,
    cursor_content_column: usize,
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
    state.publish_composer_layout(content_width, input_budget);

    let VisualLayout {
        rows,
        cursor_line,
        cursor_content_column,
    } = visual_layout(state.input(), state.input_cursor(), content_width);
    let viewport_start = state.composer_viewport_start(rows.len(), cursor_line);
    let lines = rows
        .into_iter()
        .skip(viewport_start)
        .take(input_budget)
        .map(|row| row.text)
        .collect();

    RenderedInput {
        lines,
        cursor_line: cursor_line.saturating_sub(viewport_start),
        cursor_column: PROMPT_WIDTH + cursor_content_column,
    }
}

fn input_content_width(composer_width: u16) -> usize {
    let inner_width = composer_width.saturating_sub(2) as usize;
    inner_width.saturating_sub(PROMPT_WIDTH).max(1)
}

#[derive(Clone, Copy)]
enum VerticalNavigation {
    Up,
    Down,
    PageUp,
    PageDown,
}

pub(in crate::app) fn move_input_up(state: &mut AppState, allow_history: bool) {
    navigate_input(state, VerticalNavigation::Up, allow_history);
}

pub(in crate::app) fn move_input_down(state: &mut AppState, allow_history: bool) {
    navigate_input(state, VerticalNavigation::Down, allow_history);
}

pub(in crate::app) fn move_input_page_up(state: &mut AppState, allow_history: bool) {
    navigate_input(state, VerticalNavigation::PageUp, allow_history);
}

pub(in crate::app) fn move_input_page_down(state: &mut AppState, allow_history: bool) {
    navigate_input(state, VerticalNavigation::PageDown, allow_history);
}

fn navigate_input(state: &mut AppState, navigation: VerticalNavigation, allow_history: bool) {
    let moving_up = matches!(
        navigation,
        VerticalNavigation::Up | VerticalNavigation::PageUp
    );
    if state.input().is_empty() {
        if moving_up && allow_history {
            state.history_previous();
        }

        return;
    }

    let (content_width, _) = state.composer_layout_metrics();
    let layout = visual_layout(state.input(), state.input_cursor(), content_width);
    let current_row = layout.cursor_line;
    let last_row = layout.rows.len().saturating_sub(1);

    if state.history_is_active()
        && allow_history
        && ((moving_up && current_row == 0) || (!moving_up && current_row == last_row))
    {
        if moving_up {
            state.history_previous();
        } else {
            state.history_next();
        }

        return;
    }

    if matches!(navigation, VerticalNavigation::Up) && current_row == 0 {
        state.set_input_cursor_boundary(layout.rows[0].source_start);
        return;
    }

    if matches!(navigation, VerticalNavigation::Down) && current_row == last_row {
        state.set_input_cursor_boundary(layout.rows[last_row].source_end);
        return;
    }

    let step = if matches!(
        navigation,
        VerticalNavigation::PageUp | VerticalNavigation::PageDown
    ) {
        state.composer_page_step()
    } else {
        1
    };

    let target_row = if moving_up {
        current_row.saturating_sub(step)
    } else {
        current_row.saturating_add(step).min(last_row)
    };

    if target_row == current_row {
        return;
    }

    let source = &layout.rows[current_row];
    let target = &layout.rows[target_row];
    let target_column = state.composer_vertical_target_column(
        layout.cursor_content_column,
        source.max_cursor_column(),
        target.max_cursor_column(),
    );
    state.set_input_cursor_vertical(target.cursor_for_column(target_column));
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

    fn drop_for_wrap(self, available_width: usize) -> (Option<Self>, Option<Self>) {
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

        let dropped_text = &self.text[..dropped_bytes];
        let dropped = (!dropped_text.is_empty()).then_some(Self {
            text: dropped_text,
            source_start: self.source_start,
            width: dropped_width,
        });
        let remaining_text = &self.text[dropped_bytes..];
        let remaining = (!remaining_text.is_empty()).then_some(Self {
            text: remaining_text,
            source_start: self.source_start + dropped_chars,
            width: self.width.saturating_sub(dropped_width),
        });
        (remaining, dropped)
    }
}

trait WrappedRowSink {
    fn begin_row(&mut self, logical_start: usize, segment_index: usize);

    fn push_source(&mut self, source: SourceGrapheme<'_>);
    fn push_skipped_source(&mut self, source: SourceGrapheme<'_>);

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
            if let Some(token) = whitespace {
                let (remaining, dropped) = token.drop_for_wrap(self.remaining_width());
                if let Some(dropped) = dropped {
                    for source in dropped.source_graphemes() {
                        self.sink.push_skipped_source(source);
                    }
                }

                whitespace = remaining;
            }

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

struct VisualLayoutSink {
    rows: Vec<VisualRow>,
    current: Option<VisualRow>,
    cursor: usize,
}

impl VisualLayoutSink {
    fn new(cursor: usize) -> Self {
        Self {
            rows: Vec::new(),
            current: None,
            cursor,
        }
    }

    fn finish(mut self) -> VisualLayout {
        let cursor = self.cursor;
        let mut exact = None;
        let mut next = None;
        let mut previous = None;
        let mut containing_row = None;

        for (row_index, row) in self.rows.iter().enumerate() {
            if cursor >= row.source_start && cursor <= row.source_end {
                containing_row = Some(row_index);
            }

            for point in &row.boundaries {
                if point.source == cursor {
                    exact = Some((row_index, point.column));
                } else if point.source < cursor {
                    previous = Some((row_index, point.column));
                } else if next.is_none() {
                    next = Some((row_index, point.column));
                }
            }

            if let Some(point) = row.interior_cursor {
                exact = Some((row_index, point.column));
            }
        }

        let fallback = containing_row.map(|row_index| {
            let row = &self.rows[row_index];
            let point = row
                .boundaries
                .iter()
                .rev()
                .find(|point| point.source <= cursor)
                .copied()
                .unwrap_or(CursorPoint {
                    source: row.source_start,
                    column: 0,
                });
            (row_index, point.column)
        });

        let (cursor_line, cursor_content_column) =
            exact.or(fallback).or(next).or(previous).unwrap_or((0, 0));

        VisualLayout {
            rows: std::mem::take(&mut self.rows),
            cursor_line,
            cursor_content_column,
        }
    }

    fn push_boundary(row: &mut VisualRow, point: CursorPoint) {
        row.boundaries.push(point);
    }
}

impl WrappedRowSink for VisualLayoutSink {
    fn begin_row(&mut self, logical_start: usize, segment_index: usize) {
        let mut row = VisualRow {
            text: if segment_index == 0 {
                PROMPT.to_string()
            } else {
                CONTINUATION_PROMPT.to_string()
            },
            source_start: logical_start,
            source_end: logical_start,
            display_width: 0,
            last_grapheme_width: 0,
            is_last_logical_segment: false,
            boundaries: Vec::new(),
            interior_cursor: None,
        };
        if segment_index == 0 {
            Self::push_boundary(
                &mut row,
                CursorPoint {
                    source: logical_start,
                    column: 0,
                },
            );
        }

        self.current = Some(row);
    }

    fn push_source(&mut self, source: SourceGrapheme<'_>) {
        let row = self.current.as_mut().expect("row begins before source");
        if row.boundaries.is_empty() {
            row.source_start = source.source_start;
            Self::push_boundary(
                row,
                CursorPoint {
                    source: source.source_start,
                    column: row.display_width,
                },
            );
        }

        row.text.push_str(source.text);
        if self.cursor > source.source_start && self.cursor < source.source_end {
            let scalar_offset = self.cursor - source.source_start;
            let prefix_end = source
                .text
                .char_indices()
                .nth(scalar_offset)
                .map(|(byte_index, _)| byte_index)
                .expect("cursor inside grapheme has a prefix boundary");
            row.interior_cursor = Some(CursorPoint {
                source: self.cursor,
                column: row.display_width + source.text[..prefix_end].width(),
            });
        }

        row.display_width += source.width;
        row.last_grapheme_width = source.width;
        row.source_end = source.source_end;
        Self::push_boundary(
            row,
            CursorPoint {
                source: source.source_end,
                column: row.display_width,
            },
        );
    }

    fn push_skipped_source(&mut self, source: SourceGrapheme<'_>) {
        let row = self
            .current
            .as_mut()
            .expect("row begins before skipped source");
        if self.cursor > source.source_start && self.cursor < source.source_end {
            let scalar_offset = self.cursor - source.source_start;
            let prefix_end = source
                .text
                .char_indices()
                .nth(scalar_offset)
                .map(|(byte_index, _)| byte_index)
                .expect("cursor inside skipped grapheme has a prefix boundary");
            row.interior_cursor = Some(CursorPoint {
                source: self.cursor,
                column: row.display_width + source.text[..prefix_end].width(),
            });
        }

        row.display_width += source.width;
        row.last_grapheme_width = source.width;
        row.source_end = source.source_end;
        Self::push_boundary(
            row,
            CursorPoint {
                source: source.source_end,
                column: row.display_width,
            },
        );
    }

    fn end_row(&mut self, logical_end: Option<usize>) {
        let mut row = self.current.take().expect("row ends after begin");
        if let Some(logical_end) = logical_end {
            row.source_end = logical_end;
            row.is_last_logical_segment = true;
            let display_width = row.display_width;
            Self::push_boundary(
                &mut row,
                CursorPoint {
                    source: logical_end,
                    column: display_width,
                },
            );
        }

        self.rows.push(row);
    }

    fn attach_logical_end(&mut self, logical_end: usize) {
        let row = self.rows.last_mut().expect("logical line has a row");
        row.source_end = logical_end;
        row.is_last_logical_segment = true;
        let display_width = row.display_width;
        Self::push_boundary(
            row,
            CursorPoint {
                source: logical_end,
                column: display_width,
            },
        );
    }
}

#[derive(Default)]
struct RowCountSink {
    count: usize,
}

impl WrappedRowSink for RowCountSink {
    fn begin_row(&mut self, _logical_start: usize, _segment_index: usize) {}

    fn push_source(&mut self, _source: SourceGrapheme<'_>) {}

    fn push_skipped_source(&mut self, _source: SourceGrapheme<'_>) {}

    fn end_row(&mut self, _logical_end: Option<usize>) {
        self.count += 1;
    }

    fn attach_logical_end(&mut self, _logical_end: usize) {}
}

fn visual_layout(input: &str, cursor: usize, content_width: usize) -> VisualLayout {
    let mut sink = VisualLayoutSink::new(cursor);
    wrap_input(input, content_width, &mut sink);
    sink.finish()
}

#[cfg(test)]
fn wrapped_input_lines(input: &str, cursor: usize, content_width: usize) -> WrappedInput {
    let layout = visual_layout(input, cursor, content_width);
    WrappedInput {
        lines: layout.rows.into_iter().map(|row| row.text).collect(),
        cursor_line: layout.cursor_line,
        cursor_column: PROMPT_WIDTH + layout.cursor_content_column,
    }
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
    use crate::app::input::{KeyHandling, handle_key_press};
    use crate::app::state::AppState;
    use crate::config::AppConfig;
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn nonempty_composer_navigation_matches_omp_up_and_page_up_behavior() {
        let mut up_state = test_state();
        up_state.push_input("from history");
        assert_eq!(
            up_state.take_submitted_input(),
            Some("from history".to_string())
        );
        let draft = "alpha\nbravo\ncharl";
        up_state.push_input(draft);

        let up_handling = handle_key_press(
            &mut up_state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );
        let up_observation = (
            up_handling,
            up_state.input().to_string(),
            up_state.input_cursor(),
        );

        const COMPOSER_WIDTH: u16 = 20;
        const TERMINAL_HEIGHT: u16 = 9;
        const VISIBLE_CONTENT_ROWS: usize = 4;
        let mut page_state = test_state();
        page_state.push_card("Transcript", (0..20).map(|index| format!("line {index}")));
        page_state.scroll_events_up();
        let transcript_before = (page_state.scroll_offset(), page_state.is_following_events());
        let page_draft = "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9";
        page_state.push_input(page_draft);
        let composer_height = height(&page_state, TERMINAL_HEIGHT, COMPOSER_WIDTH);
        let rows_before_page_up = lines(&page_state, VISIBLE_CONTENT_ROWS, COMPOSER_WIDTH)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        let first_page_up_handling = handle_key_press(
            &mut page_state,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        );
        let cursor_after_first_page_up = page_state.input_cursor();
        let rows_after_first_page_up = lines(&page_state, VISIBLE_CONTENT_ROWS, COMPOSER_WIDTH)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        let second_page_up_handling = handle_key_press(
            &mut page_state,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        );
        let cursor_after_second_page_up = page_state.input_cursor();
        let rows_after_second_page_up = lines(&page_state, VISIBLE_CONTENT_ROWS, COMPOSER_WIDTH)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        let transcript_after = (page_state.scroll_offset(), page_state.is_following_events());

        assert_eq!(
            (
                up_observation,
                composer_height,
                rows_before_page_up,
                first_page_up_handling,
                cursor_after_first_page_up,
                rows_after_first_page_up,
                second_page_up_handling,
                cursor_after_second_page_up,
                rows_after_second_page_up,
                page_state.input().to_string(),
                transcript_before,
                transcript_after,
            ),
            (
                (
                    KeyHandling::Continue,
                    draft.to_string(),
                    "alpha\nbravo".chars().count(),
                ),
                6,
                vec![
                    "> l6".to_string(),
                    "> l7".to_string(),
                    "> l8".to_string(),
                    "> l9".to_string(),
                ],
                KeyHandling::Continue,
                "l0\nl1\nl2\nl3\nl4\nl5\nl6".chars().count(),
                vec![
                    "> l6".to_string(),
                    "> l7".to_string(),
                    "> l8".to_string(),
                    "> l9".to_string(),
                ],
                KeyHandling::Continue,
                "l0\nl1\nl2\nl3".chars().count(),
                vec![
                    "> l3".to_string(),
                    "> l4".to_string(),
                    "> l5".to_string(),
                    "> l6".to_string(),
                ],
                page_draft.to_string(),
                (10, false),
                (10, false),
            )
        );
    }

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
        state.spawn_test_card_report_task("pending".to_string(), async {
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
        assert_eq!(
            rendered
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["> three", "> four", "> five"]
        );
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
        assert_eq!(after_mark.cursor_line, 1);
        assert_eq!(after_mark.cursor_column, PROMPT_WIDTH);
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

        assert_eq!(rendered.cursor_line, 1);
        assert_eq!(rendered.cursor_column, PROMPT_WIDTH);
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

    #[test]
    fn page_down_uses_visible_rows_minus_one_at_multiple_heights() {
        for (visible_rows, expected_cursor) in [(4, 9), (3, 6)] {
            let mut state = test_state();
            state.push_input("l0\nl1\nl2\nl3\nl4\nl5");
            state.set_input_cursor(0);
            let _ = lines(&state, visible_rows, 20);

            move_input_page_down(&mut state, true);

            assert_eq!(state.input_cursor(), expected_cursor, "{visible_rows} rows");
        }
    }

    #[test]
    fn vertical_navigation_preserves_then_resets_preferred_display_column() {
        let mut state = test_state();
        state.push_input("1234567890\nx\n1234567890");
        let _ = lines(&state, 3, 80);

        move_input_up(&mut state, true);
        assert_eq!(state.input_cursor(), "1234567890\nx".chars().count());

        state.move_input_cursor_left();
        move_input_up(&mut state, true);

        assert_eq!(state.input_cursor(), 0);
    }

    #[test]
    fn vertical_navigation_snaps_unicode_targets_to_grapheme_boundaries() {
        for (input, cursor, expected) in [
            ("ab\n😀😀", 1, 3),
            ("中x\na", 1, 4),
            ("a\u{0301}b\nz", 1, 5),
        ] {
            let mut state = test_state();
            state.push_input(input);
            state.set_input_cursor(cursor);
            let _ = lines(&state, 4, 80);

            move_input_down(&mut state, true);

            assert_eq!(state.input_cursor(), expected, "input {input:?}");
        }
    }

    #[test]
    fn skipped_wrap_whitespace_stays_on_preceding_row_for_cursor_and_navigation() {
        let input = "hello   bananas";
        for cursor in [6, 7] {
            let wrapped = wrapped_input_lines(input, cursor, 12);
            assert_eq!(wrapped.cursor_line, 0, "cursor {cursor}");
            assert_eq!(
                wrapped.cursor_column,
                PROMPT_WIDTH + cursor,
                "cursor {cursor}"
            );
        }

        let boundary = wrapped_input_lines(input, 8, 12);
        assert_eq!(boundary.cursor_line, 1);
        assert_eq!(boundary.cursor_column, PROMPT_WIDTH);

        let mut state = test_state();
        state.push_input(input);
        state.set_input_cursor(6);
        let _ = rendered_input(&state, 2, 12);

        move_input_down(&mut state, true);
        assert_eq!(state.input_cursor(), 14);
        move_input_up(&mut state, true);
        assert_eq!(state.input_cursor(), 6);

        state.set_input_cursor(8);
        move_input_up(&mut state, true);
        assert_eq!(state.input_cursor(), 0);
        move_input_down(&mut state, true);
        assert_eq!(state.input_cursor(), 8);

        let mut four_column = test_state();
        four_column.push_input("aaaa bbbb");
        let _ = rendered_input(&four_column, 2, 4);
        move_input_up(&mut four_column, true);
        assert_eq!(four_column.input_cursor(), 4);
        move_input_down(&mut four_column, true);
        assert_eq!(four_column.input_cursor(), "aaaa bbbb".chars().count());
    }

    #[test]
    fn viewport_follows_cursor_both_directions_and_clamps_after_shrink() {
        let mut state = test_state();
        state.push_input("l0\nl1\nl2\nl3\nl4\nl5");

        let tail = lines(&state, 3, 20)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(tail, ["> l3", "> l4", "> l5"]);

        move_input_page_up(&mut state, true);
        move_input_page_up(&mut state, true);
        let earlier = lines(&state, 3, 20)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(earlier, ["> l1", "> l2", "> l3"]);

        move_input_page_down(&mut state, true);
        let middle = lines(&state, 3, 20)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(middle, ["> l1", "> l2", "> l3"]);

        move_input_page_down(&mut state, true);
        let tail_again = lines(&state, 3, 20)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(tail_again, ["> l3", "> l4", "> l5"]);

        state.replace_input_from_completion("a\nb".to_string());
        let shrunk = lines(&state, 3, 20)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(shrunk, ["> a", "> b"]);
    }

    #[test]
    fn immutable_render_seam_drives_page_step_and_width_reset_behavior() {
        let mut page_state = test_state();
        page_state.push_input("l0\nl1\nl2\nl3\nl4\nl5");
        assert_eq!(height(&page_state, 9, 20), 6);

        move_input_page_up(&mut page_state, true);
        assert_eq!(page_state.input_cursor(), "l0\nl1\nl2".chars().count());

        let mut width_state = test_state();
        width_state.push_input("1234567890\nx\n1234567890");
        let _ = rendered_input(&width_state, 2, input_content_width(80));
        move_input_up(&mut width_state, true);
        assert_eq!(width_state.input_cursor(), "1234567890\nx".chars().count());

        let narrow_width = input_content_width(8);
        let _ = rendered_input(&width_state, 2, narrow_width);
        move_input_up(&mut width_state, true);
        let rendered = rendered_input(&width_state, 2, narrow_width);

        assert_eq!(width_state.input_cursor(), 9);
        assert!(rendered.cursor_line < rendered.lines.len());
    }

    #[test]
    fn nonvertical_actions_reset_the_next_vertical_destination() {
        fn preferred_state() -> AppState {
            let mut state = test_state();
            state.push_input("1234567890\nx\n1234567890");
            state.set_input_cursor(19);
            let _ = lines(&state, 3, 80);
            move_input_up(&mut state, true);
            assert_eq!(state.input_cursor(), "1234567890\nx".chars().count());
            state
        }

        type CursorAction = fn(&mut AppState);
        let cases: [(CursorAction, usize); 8] = [
            (
                |state| {
                    state.push_input("q");
                    move_input_up(state, true);
                },
                2,
            ),
            (
                |state| {
                    state.pop_input_char();
                    move_input_up(state, true);
                },
                0,
            ),
            (
                |state| {
                    state.delete_input_char();
                    move_input_up(state, true);
                },
                1,
            ),
            (
                |state| {
                    state.move_input_cursor_left();
                    move_input_up(state, true);
                },
                0,
            ),
            (
                |state| {
                    state.move_input_cursor_prev_word();
                    move_input_up(state, true);
                },
                0,
            ),
            (
                |state| {
                    state.move_input_cursor_next_word();
                    move_input_up(state, true);
                    move_input_up(state, true);
                },
                0,
            ),
            (
                |state| {
                    state.set_input_cursor(17);
                    move_input_up(state, true);
                    move_input_up(state, true);
                },
                4,
            ),
            (
                |state| {
                    state.replace_input_from_completion("1234567890\nx\n1234567890".to_string());
                    move_input_up(state, true);
                    move_input_up(state, true);
                },
                10,
            ),
        ];

        for (action, expected_cursor) in cases {
            let mut state = preferred_state();
            action(&mut state);
            assert_eq!(state.input_cursor(), expected_cursor);
        }
    }

    #[test]
    fn right_and_line_boundary_reset_the_next_vertical_destination() {
        fn preferred_at_last_line_end() -> AppState {
            let mut state = test_state();
            state.push_input("1234567890\nx");
            state.set_input_cursor(6);
            let _ = lines(&state, 2, 80);
            move_input_down(&mut state, true);
            assert_eq!(state.input_cursor(), state.input().chars().count());
            state
        }

        let mut right = preferred_at_last_line_end();
        right.move_input_cursor_right();
        move_input_up(&mut right, true);
        assert_eq!(right.input_cursor(), 1);

        let mut boundary = preferred_at_last_line_end();
        move_input_down(&mut boundary, true);
        move_input_up(&mut boundary, true);
        assert_eq!(boundary.input_cursor(), 1);
    }

    #[test]
    fn history_replacement_resets_the_next_vertical_destination() {
        let mut state = test_state();
        let history_entry = "\u{200b}\n1234567890";
        state.push_input(history_entry);
        assert_eq!(
            state.take_submitted_input(),
            Some(history_entry.to_string())
        );

        state.push_input("1234567890\nx");
        state.set_input_cursor(6);
        let _ = lines(&state, 2, 80);
        move_input_down(&mut state, true);
        state.history_previous();
        assert_eq!(state.input_cursor(), 0);

        move_input_down(&mut state, true);
        assert_eq!(state.input_cursor(), 2);
    }

    #[test]
    fn submission_reset_keeps_recalled_vertical_navigation_at_the_new_column() {
        let mut state = test_state();
        let input = "1234567890\nx";
        state.push_input(input);
        state.set_input_cursor(6);
        let _ = lines(&state, 2, 80);
        move_input_down(&mut state, true);
        assert_eq!(state.take_submitted_input(), Some(input.to_string()));

        move_input_up(&mut state, true);
        assert_eq!(state.input_cursor(), 0);
        move_input_down(&mut state, true);
        assert_eq!(state.input_cursor(), "1234567890\n".chars().count());
    }
}
