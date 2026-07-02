use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::state::{AppState, PendingPrompt};
use super::super::styles::{
    style_accent, style_border, style_muted, style_transcript_metadata, style_transcript_normal,
    style_warning,
};

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let transcript = Paragraph::new(lines(state, visible_height))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(style_border()),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(transcript, area);
}

pub(in crate::app) fn lines(state: &AppState, max_visible_lines: usize) -> Vec<Line<'static>> {
    if max_visible_lines == 0 {
        return Vec::new();
    }

    let lines = all_lines(state);
    if lines.len() <= max_visible_lines {
        return lines;
    }

    let offset = state.scroll_offset().min(lines.len().saturating_sub(1));
    let end = lines.len().saturating_sub(offset).max(1);
    let start = end.saturating_sub(max_visible_lines);
    lines[start..end].to_vec()
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
                && entry.contains(&format!("prompt: {}", prompt.prompt_id()))
        });
        if !prompt_is_latest {
            lines.extend(prompt_card_lines(prompt));
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

fn prompt_card_lines(prompt: &PendingPrompt) -> Vec<Line<'static>> {
    let choices = if prompt.choices().is_empty() {
        "<freeform>".to_string()
    } else {
        prompt.choices().join(", ")
    };
    let mut lines = vec![
        Line::from(Span::styled("Waiting for input", style_warning())),
        metadata_line(format!("         step: {}", prompt.step())),
        metadata_line(format!("         prompt: {}", prompt.prompt_id())),
    ];
    lines.extend(prompt_message_lines(prompt.message()));
    lines.push(Line::from(vec![
        Span::styled("         choices: ", style_transcript_metadata()),
        Span::styled(choices, style_warning()),
    ]));
    lines.push(metadata_line(
        "         Type an answer below and press Enter.",
    ));
    lines.push(Line::from(""));
    lines
}

fn prompt_message_lines(message: &str) -> Vec<Line<'static>> {
    if message.is_empty() {
        return vec![metadata_line("         message:")];
    }

    message
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 {
                "         message: "
            } else {
                "                "
            };
            Line::from(vec![
                Span::styled(prefix, style_transcript_metadata()),
                Span::styled(line.to_string(), style_transcript_normal()),
            ])
        })
        .collect()
}

fn metadata_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(text.into(), style_transcript_metadata()))
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

    use super::*;
    use crate::app::state::AppState;
    use crate::app::styles::style_transcript_thought;
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

        let rendered = lines(&state, 20)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Waiting for input"));
        assert!(rendered.contains("prompt: approval"));
        assert!(rendered.contains("message: Approve?"));
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

        let rendered_lines = lines(&state, 100)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(rendered_lines.iter().all(|line| !line.contains('\n')));
        let rendered = rendered_lines.join("\n");
        assert!(rendered.contains("message: Review plan"), "{rendered}");
        assert!(
            rendered.contains("                - first item"),
            "{rendered}"
        );
        assert!(
            rendered.contains("                - second item"),
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

        let rendered = lines(&state, 100)
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
        assert!(rendered.contains("thought: thinking"));
        assert!(rendered.contains("content: done"));
        assert!(rendered.contains("content: ready"));
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

        let rendered = lines(&state, 20);
        let thought_line = rendered
            .iter()
            .find(|line| line.to_string().contains("thought: thinking"))
            .unwrap();

        assert!(thought_line.spans.iter().any(|span| {
            span.content.contains("thinking") && span.style == style_transcript_thought()
        }));
    }
}
