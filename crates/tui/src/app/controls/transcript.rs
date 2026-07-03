use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

use super::super::state::{AppState, render_pending_prompt_lines};
use super::super::styles::{style_accent, style_border, style_muted, style_transcript_normal};

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let inner_width = usize::from(area.width.saturating_sub(2)).max(1);
    let transcript = Paragraph::new(lines(state, visible_height, inner_width)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(style_border()),
    );
    frame.render_widget(transcript, area);
}

pub(in crate::app) fn lines(
    state: &AppState,
    max_visible_lines: usize,
    wrap_width: usize,
) -> Vec<Line<'static>> {
    if max_visible_lines == 0 || wrap_width == 0 {
        return Vec::new();
    }

    let rows = visual_rows(all_lines(state), wrap_width);
    if rows.len() <= max_visible_lines {
        return rows;
    }

    let offset = state.scroll_offset().min(rows.len().saturating_sub(1));
    let end = rows.len().saturating_sub(offset).max(1);
    let start = end.saturating_sub(max_visible_lines);
    rows[start..end].to_vec()
}

fn visual_rows(logical_lines: Vec<Line<'static>>, wrap_width: usize) -> Vec<Line<'static>> {
    logical_lines
        .into_iter()
        .flat_map(|line| wrap_line(line, wrap_width))
        .collect()
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

fn all_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = if state.event_log_is_empty() {
        empty_lines()
    } else {
        let mut rendered = Vec::new();
        for entry in state.event_entries() {
            rendered.extend(entry.render_lines());
            rendered.push(Line::from(""));
        }
        rendered
    };

    if let Some(prompt) = state.pending_prompt() {
        let prompt_is_latest = state.event_entries().last().is_some_and(|entry| {
            entry.contains("Waiting for input")
                && entry.contains(&format!("prompt={}", prompt.prompt_id()))
        });
        if !prompt_is_latest {
            lines.extend(render_pending_prompt_lines(prompt));
        }
    }

    lines
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

    use super::*;
    use crate::app::state::AppState;
    use crate::app::styles::{style_transcript_metadata, style_transcript_thought};
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

        let rendered = lines(&state, 20, usize::MAX)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Waiting for input"));
        assert!(rendered.contains("step=approve"));
        assert!(rendered.contains("prompt=approval"));
        assert!(rendered.contains("choices=yes, no"));
        assert!(rendered.contains("\nApprove?"));
        assert!(!rendered.contains("prompt: approval"));
        assert!(!rendered.contains("message: Approve?"));
        assert!(!rendered.contains("╭─ Waiting for input ─╮"));
        assert!(!rendered.contains("╰──────────────────────╯"));
        assert!(!rendered.contains("│"));
        let first_event = rendered
            .lines()
            .find(|line| line.contains("Waiting for input"))
            .unwrap();
        assert_eq!(first_event.chars().nth(2), Some(':'));
        assert_eq!(first_event.chars().nth(5), Some(':'));
    }

    #[test]
    fn prompt_card_splits_multiline_message() {
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

        let rendered_lines = lines(&state, 100, usize::MAX)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(rendered_lines.iter().all(|line| !line.contains('\n')));
        let rendered = rendered_lines.join("\n");
        assert!(rendered.contains("Waiting for input"), "{rendered}");
        assert!(rendered.contains("step=confirm_plan"), "{rendered}");
        assert!(rendered.contains("prompt=approval"), "{rendered}");
        assert!(rendered.contains("choices=<freeform>"), "{rendered}");
        assert!(
            rendered.contains("\nReview plan\n- first item"),
            "{rendered}"
        );
        assert!(rendered.contains("\n- second item"), "{rendered}");
        assert!(!rendered.contains("message:"), "{rendered}");
        assert!(
            !rendered.contains("                - first item"),
            "{rendered}"
        );
    }

    #[test]
    fn events_render_in_chronological_order() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::RunStarted {
                workflow_name: "agent/00-feature".to_string(),
                current_step: "plan".to_string(),
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

        let rendered = lines(&state, 100, usize::MAX)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let run_started = rendered.find("Run started").unwrap();
        let step_started = rendered.find("Step started").unwrap();
        let session_ready = rendered.find("Agent session ready").unwrap();
        let prompt = rendered.find("Prompt sent to agent").unwrap();
        let thought = rendered.find("Agent thinking").unwrap();
        let tool_call = rendered.find("Agent tool call").unwrap();
        let tool_update = rendered.find("Agent tool update").unwrap();
        let response = rendered.find("Agent response").unwrap();

        assert!(run_started < step_started);
        assert!(step_started < session_ready);
        assert!(session_ready < prompt);
        assert!(prompt < thought);
        assert!(thought < tool_call);
        assert!(tool_call < tool_update);
        assert!(tool_update < response);
        assert!(rendered.contains("\nthinking"));
        assert!(rendered.contains("\ndone"));
        assert!(rendered.contains("\nready"));
        assert!(!rendered.contains("thought: thinking"));
        assert!(!rendered.contains("content: done"));
        assert!(!rendered.contains("content: ready"));
        assert!(!rendered.contains("id:"), "{rendered}");
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

        let rendered = lines(&state, 20, usize::MAX);
        let thought_line = rendered
            .iter()
            .find(|line| line.to_string() == "thinking")
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

        let rendered = lines(&state, 3, 6)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();

        assert!(
            rendered.iter().any(|line| line.contains("DONE")),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().all(|line| !line.contains("aaaaa")),
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
    fn scroll_offsets_apply_to_wrapped_visual_rows() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::AgentResponse {
                step_id: "review".to_string(),
                content: "0000111122223333444455556666777788889999TAIL".to_string(),
            },
        ));

        let following = lines(&state, 4, 4)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            following.iter().any(|line| line.contains("TAIL")),
            "{following:?}"
        );

        state.scroll_events_up();
        let scrolled = lines(&state, 4, 4)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            scrolled.iter().any(|line| line.contains("0000")),
            "{scrolled:?}"
        );
        assert!(
            scrolled.iter().all(|line| !line.contains("TAIL")),
            "{scrolled:?}"
        );

        state.scroll_events_down();
        let refollowing = lines(&state, 4, 4)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            refollowing.iter().any(|line| line.contains("TAIL")),
            "{refollowing:?}"
        );
    }
}
