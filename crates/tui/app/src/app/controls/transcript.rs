use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthChar;

use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

use super::super::state::{
    AppState, TranscriptEntry, TranscriptSelection, TranscriptSelectionPoint,
    render_pending_prompt_lines,
};
use super::super::styles::{
    style_accent, style_muted, style_transcript_normal, style_transcript_selection,
};

#[derive(Debug)]
struct TranscriptViewport {
    rows: Vec<Line<'static>>,
    effective_offset: usize,
    #[cfg(test)]
    older_overflow: bool,
    #[cfg(test)]
    newer_overflow: bool,
    #[cfg(test)]
    content_length: usize,
}

#[derive(Debug)]
struct BoundedVisualRows {
    rows: Vec<Line<'static>>,
    older_unmeasured: bool,
}

#[derive(Debug)]
struct EntryVisualRows {
    rows: Vec<Line<'static>>,
    older_unmeasured: bool,
}

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let (_, viewport) = content_viewport(state, area, state.scroll_offset());

    frame.render_widget(
        Paragraph::new(selected_rows(viewport.rows, state.transcript_selection())),
        area,
    );
}

pub(in crate::app) fn next_scroll_limit(state: &AppState, area: Rect) -> usize {
    content_viewport(state, area, state.next_scroll_offset())
        .1
        .effective_offset
}

pub(in crate::app) fn current_scroll_limit(state: &AppState, area: Rect) -> usize {
    content_viewport(state, area, state.scroll_offset())
        .1
        .effective_offset
}

pub(in crate::app) fn selection_point_at(
    state: &AppState,
    area: Rect,
    position: Position,
) -> Option<TranscriptSelectionPoint> {
    let (content_area, viewport) = content_viewport(state, area, state.scroll_offset());
    if content_area.width == 0 || content_area.height == 0 || !content_area.contains(position) {
        return None;
    }

    if viewport.rows.is_empty() {
        return None;
    }

    let row = usize::from(position.y.saturating_sub(content_area.y))
        .min(viewport.rows.len().saturating_sub(1));
    let column = usize::from(position.x.saturating_sub(content_area.x))
        .min(line_display_width(&viewport.rows[row]));
    Some(TranscriptSelectionPoint::new(row, column))
}

pub(in crate::app) fn selected_text(state: &AppState, area: Rect) -> String {
    let Some(selection) = state.transcript_selection() else {
        return String::new();
    };

    let (_, viewport) = content_viewport(state, area, state.scroll_offset());
    selected_text_from_rows(&viewport.rows, selection)
}

fn content_viewport(
    state: &AppState,
    area: Rect,
    requested_offset: usize,
) -> (Rect, TranscriptViewport) {
    let visible_height = area.height as usize;
    let viewport = viewport_at_offset(state, visible_height, area.width as usize, requested_offset);
    (area, viewport)
}

fn selected_rows(
    rows: Vec<Line<'static>>,
    selection: Option<&TranscriptSelection>,
) -> Vec<Line<'static>> {
    let Some(selection) = selection else {
        return rows;
    };

    rows.into_iter()
        .enumerate()
        .map(|(row_index, row)| line_with_selection(row, row_index, selection))
        .collect()
}

fn line_with_selection(
    line: Line<'static>,
    row_index: usize,
    selection: &TranscriptSelection,
) -> Line<'static> {
    let line_width = line_display_width(&line);
    let Some(range) = row_selection_range(selection, row_index, line_width) else {
        return line;
    };

    let Line {
        spans,
        style,
        alignment,
    } = line;
    let mut selected_spans = Vec::new();
    for span in spans {
        push_split_span(&mut selected_spans, span, range.clone());
    }

    let mut row = Line::from(selected_spans);
    row.style = style;
    row.alignment = alignment;
    row
}

fn push_split_span(
    output: &mut Vec<Span<'static>>,
    span: Span<'static>,
    range: std::ops::Range<usize>,
) {
    let mut segment = String::new();
    let mut segment_style = span.style;
    let mut segment_selected = None;
    let mut column = 0usize;

    for ch in span.content.chars() {
        let width = ch.width().unwrap_or(0);
        let selected = char_intersects_range(column, width, &range);
        if segment_selected.is_some_and(|previous| previous != selected) {
            push_selection_segment(output, &mut segment, segment_style);
        }

        if segment_selected != Some(selected) {
            segment_style = if selected {
                style_transcript_selection(span.style)
            } else {
                span.style
            };
            segment_selected = Some(selected);
        }

        segment.push(ch);
        column = column.saturating_add(width);
    }

    push_selection_segment(output, &mut segment, segment_style);
}

fn push_selection_segment(output: &mut Vec<Span<'static>>, segment: &mut String, style: Style) {
    if segment.is_empty() {
        return;
    }

    output.push(Span::styled(std::mem::take(segment), style));
}

fn selected_text_from_rows(rows: &[Line<'static>], selection: &TranscriptSelection) -> String {
    let (start, end) = normalize_selection(selection);
    if start == end || rows.is_empty() {
        return String::new();
    }

    let mut selected_rows = Vec::new();
    for row_index in start.row..=end.row {
        let Some(row) = rows.get(row_index) else {
            continue;
        };

        let row_width = line_display_width(row);
        let Some(range) = row_selection_range_between(start, end, row_index, row_width) else {
            selected_rows.push(String::new());
            continue;
        };

        selected_rows.push(line_selected_text(row, range));
    }

    selected_rows.join("\n")
}

fn line_selected_text(line: &Line<'static>, range: std::ops::Range<usize>) -> String {
    let mut text = String::new();
    let mut column = 0usize;
    for span in &line.spans {
        for ch in span.content.chars() {
            let width = ch.width().unwrap_or(0);
            if char_intersects_range(column, width, &range) {
                text.push(ch);
            }

            column = column.saturating_add(width);
        }
    }

    text
}

fn row_selection_range(
    selection: &TranscriptSelection,
    row_index: usize,
    row_width: usize,
) -> Option<std::ops::Range<usize>> {
    let (start, end) = normalize_selection(selection);
    row_selection_range_between(start, end, row_index, row_width)
}

fn row_selection_range_between(
    start: TranscriptSelectionPoint,
    end: TranscriptSelectionPoint,
    row_index: usize,
    row_width: usize,
) -> Option<std::ops::Range<usize>> {
    if start == end || row_index < start.row || row_index > end.row {
        return None;
    }

    let start_column = if row_index == start.row {
        start.column
    } else {
        0
    }
    .min(row_width);
    let end_column = if row_index == end.row {
        end.column
    } else {
        row_width
    }
    .min(row_width);
    (start_column < end_column).then_some(start_column..end_column)
}

fn normalize_selection(
    selection: &TranscriptSelection,
) -> (TranscriptSelectionPoint, TranscriptSelectionPoint) {
    let anchor = selection.anchor;
    let focus = selection.focus;
    if (focus.row, focus.column) < (anchor.row, anchor.column) {
        (focus, anchor)
    } else {
        (anchor, focus)
    }
}

fn line_display_width(line: &Line<'static>) -> usize {
    line.spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(|ch| ch.width().unwrap_or(0))
        .sum()
}

fn char_intersects_range(column: usize, width: usize, range: &std::ops::Range<usize>) -> bool {
    if width == 0 {
        return range.contains(&column);
    }

    column < range.end && column.saturating_add(width) > range.start
}

#[cfg(test)]
pub(in crate::app) fn lines(
    state: &AppState,
    max_visible_lines: usize,
    wrap_width: usize,
) -> Vec<Line<'static>> {
    viewport(state, max_visible_lines, wrap_width).rows
}

#[cfg(test)]
fn viewport(state: &AppState, max_visible_lines: usize, wrap_width: usize) -> TranscriptViewport {
    viewport_at_offset(state, max_visible_lines, wrap_width, state.scroll_offset())
}

fn viewport_at_offset(
    state: &AppState,
    max_visible_lines: usize,
    wrap_width: usize,
    requested_offset: usize,
) -> TranscriptViewport {
    if max_visible_lines == 0 || wrap_width == 0 {
        return TranscriptViewport {
            rows: Vec::new(),
            effective_offset: 0,
            #[cfg(test)]
            older_overflow: false,
            #[cfg(test)]
            newer_overflow: false,
            #[cfg(test)]
            content_length: 0,
        };
    }

    let bounded = if state.event_log_is_empty() {
        BoundedVisualRows {
            rows: visual_rows(empty_lines(), wrap_width),
            older_unmeasured: false,
        }
    } else {
        bounded_tail_visual_rows(state, max_visible_lines, wrap_width, requested_offset)
    };

    select_viewport(bounded, max_visible_lines, requested_offset)
}

fn visual_rows(logical_lines: Vec<Line<'static>>, wrap_width: usize) -> Vec<Line<'static>> {
    logical_lines
        .into_iter()
        .flat_map(|line| wrap_line(line, wrap_width))
        .collect()
}

fn select_viewport(
    bounded: BoundedVisualRows,
    max_visible_lines: usize,
    requested_offset: usize,
) -> TranscriptViewport {
    let BoundedVisualRows {
        rows,
        older_unmeasured,
    } = bounded;
    let max_offset = if older_unmeasured {
        requested_offset
    } else {
        rows.len().saturating_sub(max_visible_lines)
    };
    let effective_offset = requested_offset.min(max_offset);
    let end = rows.len().saturating_sub(effective_offset);
    let start = end.saturating_sub(max_visible_lines);
    #[cfg(test)]
    let older_overflow = older_unmeasured || start > 0;
    #[cfg(test)]
    let newer_overflow = end < rows.len();
    #[cfg(test)]
    let unmeasured_sentinel = usize::from(older_unmeasured);
    #[cfg(test)]
    let content_length = rows.len().saturating_add(unmeasured_sentinel);

    TranscriptViewport {
        rows: rows[start..end].to_vec(),
        effective_offset,
        #[cfg(test)]
        older_overflow,
        #[cfg(test)]
        newer_overflow,
        #[cfg(test)]
        content_length,
    }
}

fn bounded_tail_visual_rows(
    state: &AppState,
    max_visible_lines: usize,
    wrap_width: usize,
    requested_offset: usize,
) -> BoundedVisualRows {
    let target_rows = max_visible_lines.saturating_add(requested_offset);
    let mut chunks = Vec::new();
    let mut row_count = 0usize;
    let mut older_unmeasured = false;

    let mut prompt_id_to_skip = None;
    if let Some(prompt) = state.pending_prompt()
        && !pending_prompt_is_latest(state, prompt.prompt_id())
    {
        let rows = render_pending_prompt_lines(prompt, wrap_width);
        row_count = row_count.saturating_add(rows.len());
        chunks.push(rows);
        prompt_id_to_skip = Some(prompt.prompt_id());
    }

    for entry in state.event_entries().iter().rev() {
        if prompt_id_to_skip.is_some_and(|prompt_id| entry_is_waiting_prompt(entry, prompt_id)) {
            continue;
        }
        if row_count >= target_rows {
            older_unmeasured = true;
            break;
        }

        let mut entry_rows =
            entry_tail_visual_rows(entry, target_rows.saturating_sub(row_count), wrap_width);
        older_unmeasured |= entry_rows.older_unmeasured;
        entry_rows.rows.push(Line::from(""));
        row_count = row_count.saturating_add(entry_rows.rows.len());
        chunks.push(entry_rows.rows);

        if older_unmeasured {
            break;
        }
    }

    chunks.reverse();
    BoundedVisualRows {
        rows: chunks.into_iter().flatten().collect(),
        older_unmeasured,
    }
}

fn pending_prompt_is_latest(state: &AppState, prompt_id: &str) -> bool {
    state
        .event_entries()
        .last()
        .is_some_and(|entry| match entry {
            TranscriptEntry::Workflow(event) => matches!(
                &event.kind,
                WorkflowEventKind::WaitingForInput { prompt_id: id, .. } if id == prompt_id
            ),
            _ => false,
        })
}

fn entry_is_waiting_prompt(entry: &TranscriptEntry, prompt_id: &str) -> bool {
    matches!(
        entry,
        TranscriptEntry::Workflow(WorkflowEvent {
            kind: WorkflowEventKind::WaitingForInput { prompt_id: id, .. },
            ..
        }) if id == prompt_id
    )
}

fn entry_tail_visual_rows(
    entry: &TranscriptEntry,
    rows_needed: usize,
    wrap_width: usize,
) -> EntryVisualRows {
    match entry {
        TranscriptEntry::Workflow(event) => {
            stream_event_tail_visual_rows(event, rows_needed, wrap_width).unwrap_or_else(|| {
                EntryVisualRows {
                    rows: entry.render_lines_for_width(wrap_width),
                    older_unmeasured: false,
                }
            })
        }
        TranscriptEntry::Card { .. } => EntryVisualRows {
            rows: entry.render_lines_for_width(wrap_width),
            older_unmeasured: false,
        },
    }
}

fn stream_event_tail_visual_rows(
    event: &WorkflowEvent,
    rows_needed: usize,
    wrap_width: usize,
) -> Option<EntryVisualRows> {
    let retained_body_rows = rows_needed.saturating_add(2).max(1);
    let retained_body = match &event.kind {
        WorkflowEventKind::AgentResponse { content, .. }
        | WorkflowEventKind::AgentThought { content, .. } => {
            tail_content_for_visual_rows(content, retained_body_rows, wrap_width)
        }
        _ => return None,
    };

    if retained_body.len() == stream_content(event)?.len() {
        return None;
    }

    let mut event = event.clone();
    match &mut event.kind {
        WorkflowEventKind::AgentResponse { content, .. }
        | WorkflowEventKind::AgentThought { content, .. } => *content = retained_body,
        _ => return None,
    }

    Some(EntryVisualRows {
        rows: TranscriptEntry::Workflow(event).render_lines_for_width(wrap_width),
        older_unmeasured: true,
    })
}

fn stream_content(event: &WorkflowEvent) -> Option<&str> {
    match &event.kind {
        WorkflowEventKind::AgentResponse { content, .. }
        | WorkflowEventKind::AgentThought { content, .. } => Some(content),
        _ => None,
    }
}

fn tail_content_for_visual_rows(content: &str, max_rows: usize, wrap_width: usize) -> String {
    let max_width = max_rows.saturating_mul(wrap_width).max(wrap_width);
    let mut width = 0usize;
    let mut rows = 1usize;
    let mut start = content.len();

    for (index, ch) in content.char_indices().rev() {
        if ch == '\n' {
            if rows >= max_rows {
                break;
            }

            rows = rows.saturating_add(1);
            width = 0;
            start = index + ch.len_utf8();
            continue;
        }

        let ch_width = ch.width().unwrap_or(0);
        if ch_width > 0 && width > 0 && width.saturating_add(ch_width) > max_width {
            break;
        }

        width = width.saturating_add(ch_width);
        start = index;
    }

    content[start..].to_string()
}

fn wrap_line(line: Line<'static>, wrap_width: usize) -> Vec<Line<'static>> {
    let Line {
        spans,
        style,
        alignment,
    } = line;
    let mut rows = Vec::new();
    let mut row_spans = Vec::new();
    let mut row_width: usize = 0;

    for span in spans {
        let span_style = span.style;
        let mut segment = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if ch_width > 0 && row_width > 0 && row_width.saturating_add(ch_width) > wrap_width {
                push_span(&mut row_spans, &mut segment, span_style);
                push_visual_row(&mut rows, &mut row_spans, style, alignment);
                row_width = 0;
            }
            segment.push(ch);
            row_width = row_width.saturating_add(ch_width);
        }
        push_span(&mut row_spans, &mut segment, span_style);
    }

    push_visual_row(&mut rows, &mut row_spans, style, alignment);
    rows
}

fn push_span(spans: &mut Vec<Span<'static>>, segment: &mut String, style: ratatui::style::Style) {
    if segment.is_empty() {
        return;
    }
    spans.push(Span::styled(std::mem::take(segment), style));
}

fn push_visual_row(
    rows: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    style: ratatui::style::Style,
    alignment: Option<ratatui::layout::Alignment>,
) {
    let mut row = Line::from(std::mem::take(spans));
    row.style = style;
    row.alignment = alignment;
    rows.push(row);
}

fn empty_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled("No workflow transcript yet.", style_muted())),
        Line::from(""),
        Line::from(Span::styled(
            "Type a request below to start the default workflow, or use /help.",
            style_transcript_normal(),
        )),
        Line::from(""),
        Line::from(Span::styled("Examples", style_accent())),
        Line::from(Span::styled(
            "  > add a /healthz route",
            style_transcript_normal(),
        )),
        Line::from(Span::styled(
            "  > /run investigate failing tests",
            style_transcript_normal(),
        )),
        Line::from(Span::styled("  > /workflows", style_transcript_normal())),
    ]
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::app::state::{AppState, TranscriptSelection, TranscriptSelectionPoint};
    use crate::app::styles::{style_transcript_metadata, style_transcript_thought};
    use crate::config::AppConfig;

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

    fn selection(anchor: (usize, usize), focus: (usize, usize)) -> TranscriptSelection {
        TranscriptSelection {
            anchor: TranscriptSelectionPoint::new(anchor.0, anchor.1),
            focus: TranscriptSelectionPoint::new(focus.0, focus.1),
            active: false,
            selected_text: String::new(),
        }
    }

    #[test]
    fn selection_point_hit_testing_includes_rightmost_column() {
        let mut state = test_state();
        state.push_card("Transcript", (0..20).map(|index| format!("row {index}")));
        let area = Rect::new(0, 0, 24, 6);

        assert!(selection_point_at(&state, area, Position::new(22, 1)).is_some());
        assert!(selection_point_at(&state, area, Position::new(23, 1)).is_some());
    }

    #[test]
    fn overflowing_transcript_keeps_last_column_selectable_without_scrollbar_chrome() {
        let mut state = test_state();
        state.push_card("Transcript", (0..20).map(|index| format!("selectable row {index}")));
        let area = Rect::new(0, 0, 24, 6);
        let rows = rendered_rows(&state, area.height, area.width);
        let rightmost_column = area.right().saturating_sub(1);

        assert!(
            selection_point_at(&state, area, Position::new(rightmost_column, 1)).is_some(),
            "overflowing transcripts should keep the last visible column selectable instead of reserving it for scrollbar chrome: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| !row.ends_with('█')),
            "overflowing transcripts should not render a scrollbar thumb in the final column: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|row| !row.ends_with("││") && !row.ends_with("╯│")),
            "overflowing transcripts should not render an extra scrollbar track after normal transcript borders: {rows:?}"
        );
    }

    #[test]
    fn selected_text_extracts_single_row_and_highlights_range() {
        let rows = vec![Line::from("abcdef")];
        let selection = selection((0, 1), (0, 4));

        assert_eq!(selected_text_from_rows(&rows, &selection), "bcd");

        let highlighted = selected_rows(rows, Some(&selection));
        let spans = &highlighted[0].spans;
        assert_eq!(spans[0].content, "a");
        assert_eq!(spans[1].content, "bcd");
        assert!(spans[1].style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(spans[2].content, "ef");
    }

    #[test]
    fn selected_text_extracts_across_wrapped_visual_rows() {
        let rows = visual_rows(vec![Line::from("abcdefghij")], 4);
        let selection = selection((0, 2), (2, 1));

        assert_eq!(
            rows.iter().map(Line::to_string).collect::<Vec<_>>(),
            vec!["abcd", "efgh", "ij"]
        );
        assert_eq!(selected_text_from_rows(&rows, &selection), "cd\nefgh\ni");
    }

    #[test]
    fn selected_text_extracts_across_multiple_rows() {
        let rows = vec![
            Line::from("first"),
            Line::from("second"),
            Line::from("third"),
        ];
        let selection = selection((0, 2), (2, 3));

        assert_eq!(
            selected_text_from_rows(&rows, &selection),
            "rst\nsecond\nthi"
        );
    }

    #[test]
    fn selected_text_uses_display_width_for_wide_unicode() {
        let rows = vec![Line::from("a界b")];
        let selection = selection((0, 1), (0, 2));

        assert_eq!(selected_text_from_rows(&rows, &selection), "界");
    }

    fn rendered_text(state: &AppState, height: usize, width: usize) -> String {
        lines(state, height, width)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rendered_rows(state: &AppState, height: u16, width: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, area, state);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    #[test]
    fn workflow_events_render_as_focused_prompt_card() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
            },
        ));

        let rendered = rendered_text(&state, 20, 80);

        assert!(
            rendered.contains("◔ Waiting for input · ↳ approve · ▶ run-2"),
            "{rendered}"
        );
        assert!(rendered.contains("├─── Choices "), "{rendered}");
        assert!(rendered.contains("yes · no"), "{rendered}");
        assert!(rendered.contains("Approve?"), "{rendered}");
        assert!(!rendered.contains("step="), "{rendered}");
        assert!(!rendered.contains("prompt="), "{rendered}");
        assert!(!rendered.contains("choices="), "{rendered}");
        assert!(!rendered.contains("┌"), "{rendered}");
        assert!(!rendered.contains("└"), "{rendered}");
    }

    #[test]
    fn prompt_card_splits_multiline_message_and_deduplicates_pending_prompt() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::WaitingForInput {
                step: "confirm_plan".to_string(),
                prompt_id: "approval".to_string(),
                message: "Review plan\n- first item\n- second item".to_string(),
                choices: Vec::new(),
            },
        ));
        state.push_card("Notice", ["keep prompt visible".to_string()]);

        let rendered_lines = lines(&state, 100, 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(rendered_lines.iter().all(|line| !line.contains('\n')));
        let rendered = rendered_lines.join("\n");
        assert_eq!(
            rendered.matches("Waiting for input").count(),
            1,
            "{rendered}"
        );
        assert!(rendered.contains("Review plan"), "{rendered}");
        assert!(rendered.contains("- first item"), "{rendered}");
        assert!(rendered.contains("- second item"), "{rendered}");
        assert!(rendered.contains("◔ Notice"), "{rendered}");
        assert!(!rendered.contains("message:"), "{rendered}");
    }

    #[test]
    fn events_render_in_chronological_order_with_tool_coalescing() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::RunStarted {
                workflow_name: "agent/00-feature".to_string(),
                current_step: "plan".to_string(),
                request_topic: None,
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::StepStarted {
                step_id: "plan".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentSessionReady {
                step_id: "plan".to_string(),
                role: "planner".to_string(),
                session_id: "session-1".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentPrompt {
                step_id: "plan".to_string(),
                role: "planner".to_string(),
                session_id: "session-1".to_string(),
                prompt: "Plan the work".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentThought {
                step_id: "plan".to_string(),
                content: "thinking".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentToolCall {
                step_id: "plan".to_string(),
                tool_call_id: "call_1".to_string(),
                title: "Reading app state".to_string(),
                tool_kind: "read".to_string(),
                status: "pending".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "plan".to_string(),
                tool_call_id: "call_1".to_string(),
                title: "Reading app state".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"text":"done"})),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentResponse {
                step_id: "plan".to_string(),
                content: "ready".to_string(),
            },
        ));

        let rendered = rendered_text(&state, 100, 80);
        let run_started = rendered.find("Run started").unwrap();
        let step_started = rendered.find("Step started").unwrap();
        let session_ready = rendered.find("Agent session ready").unwrap();
        let prompt = rendered.find("Prompt sent to agent").unwrap();
        let thought = rendered.find("Agent thinking").unwrap();
        let tool = rendered.find("• Reading app state").unwrap();
        let response = rendered.find("Agent response").unwrap();

        assert!(run_started < step_started);
        assert!(step_started < session_ready);
        assert!(session_ready < prompt);
        assert!(prompt < thought);
        assert!(thought < tool);
        assert!(tool < response);
        assert!(rendered.contains("done"));
        assert!(rendered.contains("ready"));
        assert!(!rendered.contains("call_1"), "{rendered}");
        assert!(!rendered.contains("{\"text\""), "{rendered}");
    }

    #[test]
    fn styled_event_spans_survive_transcript_lines() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentThought {
                step_id: "plan".to_string(),
                content: "thinking".to_string(),
            },
        ));

        let rendered = lines(&state, 20, 80);
        let thought_line = rendered
            .iter()
            .find(|line| line.to_string().contains("thinking"))
            .unwrap();

        assert!(thought_line.spans.iter().any(|span| {
            span.content.contains("thinking") && span.style == style_transcript_thought()
        }));
    }

    #[test]
    fn narrow_width_latest_tail_uses_wrapped_visual_rows() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentResponse {
                step_id: "review".to_string(),
                content: "aaaaa bbbbb DONE".to_string(),
            },
        ));

        let rendered = lines(&state, 6, 12)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        assert!(
            rendered.iter().any(|line| line.contains("DONE")),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("╰")),
            "{rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= 12)
        );
    }

    #[test]
    fn long_history_follow_latest_renders_tail_without_early_entries() {
        let mut state = test_state();

        for index in 0..1_000 {
            state.push_card("Notice", [format!("early filler {index}")]);
        }

        state.push_card("Notice", ["TAIL_MARKER".to_string()]);

        let rendered = lines(&state, 6, 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        assert!(
            rendered.iter().any(|line| line.contains("TAIL_MARKER")),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().all(|line| !line.contains("early filler 0")),
            "{rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .all(|line| !line.contains("early filler 500")),
            "{rendered:?}"
        );
    }

    #[test]
    fn wrapped_visual_rows_preserve_span_styles() {
        let wrapped = visual_rows(
            vec![Line::from(vec![
                Span::styled("thought", style_transcript_thought()),
                Span::styled("meta", style_transcript_metadata()),
            ])],
            7,
        );

        assert_eq!(wrapped[0].to_string(), "thought");
        assert_eq!(wrapped[0].spans[0].style, style_transcript_thought());
        assert_eq!(wrapped[1].to_string(), "meta");
        assert_eq!(wrapped[1].spans[0].style, style_transcript_metadata());
    }

    #[test]
    fn scroll_offsets_apply_to_card_visual_rows() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentResponse {
                step_id: "review".to_string(),
                content: "0000111122223333444455556666777788889999TAIL".to_string(),
            },
        ));

        let following = lines(&state, 5, 16)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            following.iter().any(|line| line.contains("TA"))
                && following.iter().any(|line| line.contains("IL")),
            "{following:?}"
        );

        state.scroll_events_up();
        let scrolled = lines(&state, 5, 16)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            scrolled.iter().any(|line| line.contains("Agent respons")),
            "{scrolled:?}"
        );
        assert!(
            !(scrolled.iter().any(|line| line.contains("TA"))
                && scrolled.iter().any(|line| line.contains("IL"))),
            "{scrolled:?}"
        );

        state.scroll_events_down();
        let refollowing = lines(&state, 5, 16)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            refollowing.iter().any(|line| line.contains("TA"))
                && refollowing.iter().any(|line| line.contains("IL")),
            "{refollowing:?}"
        );
    }

    #[test]
    fn overflowing_content_uses_full_width_without_scrollbar_chrome() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentResponse {
                step_id: "review".to_string(),
                content: format!("{} END_MARKER", "wrapped transcript text ".repeat(30)),
            },
        ));

        let rows = rendered_rows(&state, 8, 24);

        assert!(
            rows.iter().all(|row| !row.ends_with('█')),
            "overflowing content should not draw a scrollbar thumb: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|row| !row.ends_with("││") && !row.ends_with("╯│")),
            "overflowing content should not draw scrollbar chrome after normal transcript borders: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("END_MARKER")),
            "overflowing content should keep tail content visible across the full transcript width: {rows:?}"
        );
    }

    #[test]
    fn scroll_offset_moves_up_and_returns_to_bottom_without_scrollbar_chrome() {
        let mut state = test_state();
        for index in 0..20 {
            state.push_card("Notice", [format!("transcript row {index}")]);
        }

        let following = rendered_rows(&state, 10, 40);

        assert!(state.scroll_events_up());
        let scrolled = rendered_rows(&state, 10, 40);

        assert_ne!(scrolled, following);
        assert!(
            scrolled.iter().all(|row| !row.ends_with('█')),
            "{scrolled:?}"
        );
        assert!(state.scroll_events_down());
        assert_eq!(rendered_rows(&state, 10, 40), following);
    }

    #[test]
    fn one_long_stream_reports_unmeasured_older_overflow() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentThought {
                step_id: "review".to_string(),
                content: "0123456789".repeat(2_000),
            },
        ));

        let following = viewport(&state, 6, 20);
        assert!(following.older_overflow);
        assert!(!following.newer_overflow);
        assert_eq!(following.effective_offset, 0);
        assert!(following.content_length > following.rows.len());

        assert!(state.scroll_events_up());
        let scrolled = viewport(&state, 6, 20);
        assert_eq!(scrolled.effective_offset, 10);
        assert!(scrolled.older_overflow);
        assert!(scrolled.newer_overflow);
    }

    #[test]
    fn earlier_entries_report_bounded_older_overflow() {
        let mut state = test_state();
        for index in 0..100 {
            state.push_card("Notice", [format!("entry {index}")]);
        }

        let viewport = viewport(&state, 6, 40);

        assert!(viewport.older_overflow);
        assert!(!viewport.newer_overflow);
        assert!(viewport.content_length > viewport.rows.len());
    }

    #[test]
    fn short_content_has_no_overflow() {
        let mut state = test_state();
        state.push_card("Notice", ["short".to_string()]);

        let viewport = viewport(&state, 20, 40);
        let rows = rendered_rows(&state, 20, 40);

        assert!(!viewport.older_overflow);
        assert!(!viewport.newer_overflow);
        assert!(rows.iter().all(|row| !row.ends_with('█')), "{rows:?}");
    }

    #[test]
    fn zero_height_and_one_column_transcript_areas_are_safe() {
        assert!(viewport(&test_state(), 0, 20).rows.is_empty());
        assert!(viewport(&test_state(), 10, 0).rows.is_empty());

        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentResponse {
                step_id: "review".to_string(),
                content: "long content ".repeat(100),
            },
        ));

        let rows = rendered_rows(&state, 5, 1);
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|row| row.chars().count() == 1));
    }
}
