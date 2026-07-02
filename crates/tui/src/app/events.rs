use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::markup::{plain_labeled_lines, render_labeled_markup};
use super::styles::{
    style_accent, style_error, style_for_run_state, style_for_tool_status, style_success,
    style_transcript_metadata, style_transcript_normal, style_transcript_plan,
    style_transcript_prompt, style_transcript_thought, style_transcript_tool_pending,
    style_warning,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderedWorkflowEvent {
    lines: Vec<Line<'static>>,
    text: String,
}

impl RenderedWorkflowEvent {
    pub(super) fn lines(&self) -> &[Line<'static>] {
        &self.lines
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }
}

pub(super) fn render_workflow_event(event: &WorkflowEvent) -> RenderedWorkflowEvent {
    let stamp = event.timestamp.format("%H:%M:%S").to_string();
    let mut lines = Vec::new();
    match &event.kind {
        WorkflowEventKind::RunStarted {
            workflow_name,
            current_step,
        } => {
            lines.push(title_line(&stamp, "Run started", style_accent()));
            lines.push(field_line(
                "run",
                &event.run_id,
                style_transcript_metadata(),
            ));
            lines.push(field_line(
                "workflow",
                workflow_name,
                style_transcript_normal(),
            ));
            lines.push(field_line("step", current_step, style_accent()));
        }
        WorkflowEventKind::StepStarted { step_id } => {
            lines.push(title_line(&stamp, "Step started", style_accent()));
            lines.push(field_line("step", step_id, style_accent()));
        }
        WorkflowEventKind::StepProgress { step_id, message } => {
            lines.push(title_line(&stamp, "Step progress", style_accent()));
            lines.push(field_line("step", step_id, style_accent()));
            push_multiline(
                &mut lines,
                "message",
                message,
                usize::MAX,
                style_transcript_normal(),
            );
        }
        WorkflowEventKind::AgentSessionReady {
            step_id,
            role,
            session_id,
        } => {
            lines.push(title_line(&stamp, "Agent session ready", style_accent()));
            lines.push(field_line("step", step_id, style_accent()));
            lines.push(field_line("role", role, style_transcript_normal()));
            lines.push(field_line(
                "session",
                session_id,
                style_transcript_metadata(),
            ));
        }
        WorkflowEventKind::AgentPrompt {
            step_id,
            role,
            session_id,
            prompt,
        } => {
            lines.push(title_line(
                &stamp,
                "Prompt sent to agent",
                style_transcript_prompt(),
            ));
            lines.push(field_line("step", step_id, style_accent()));
            lines.push(field_line("role", role, style_transcript_prompt()));
            lines.push(field_line(
                "session",
                session_id,
                style_transcript_metadata(),
            ));
            push_multiline(
                &mut lines,
                "prompt",
                prompt,
                usize::MAX,
                style_transcript_prompt(),
            );
        }
        WorkflowEventKind::AgentResponse { step_id, content } => {
            lines.push(title_line(
                &stamp,
                "Agent response",
                style_transcript_normal(),
            ));
            lines.push(field_line("step", step_id, style_accent()));
            push_multiline(
                &mut lines,
                "content",
                content,
                usize::MAX,
                style_transcript_normal(),
            );
        }
        WorkflowEventKind::AgentThought { step_id, content } => {
            lines.push(title_line(
                &stamp,
                "Agent thinking",
                style_transcript_thought(),
            ));
            lines.push(field_line("step", step_id, style_accent()));
            push_multiline(
                &mut lines,
                "thought",
                content,
                usize::MAX,
                style_transcript_thought(),
            );
        }
        WorkflowEventKind::AgentToolCall {
            step_id,
            title,
            tool_kind,
            status,
            ..
        } => {
            lines.push(title_line(
                &stamp,
                "Agent tool call",
                style_transcript_tool_pending(),
            ));
            lines.push(field_line("step", step_id, style_accent()));
            lines.push(field_line(
                "tool",
                &display_tool_title(title, Some(tool_kind)),
                style_transcript_tool_pending(),
            ));
            lines.push(field_line("kind", tool_kind, style_transcript_metadata()));
            lines.push(field_line("status", status, style_for_tool_status(status)));
        }
        WorkflowEventKind::AgentToolCallUpdate {
            step_id,
            title,
            status,
            content,
            ..
        } => {
            lines.push(title_line(
                &stamp,
                "Agent tool update",
                style_for_tool_status(status),
            ));
            lines.push(field_line("step", step_id, style_accent()));
            lines.push(field_line(
                "tool",
                &display_tool_title(title, None),
                style_transcript_tool_pending(),
            ));
            lines.push(field_line("status", status, style_for_tool_status(status)));
            if let Some(content) = content {
                let content = display_json_value(content);
                push_multiline(
                    &mut lines,
                    "content",
                    &content,
                    usize::MAX,
                    style_transcript_normal(),
                );
            }
        }
        WorkflowEventKind::AgentPlan { step_id, entries } => {
            lines.push(title_line(&stamp, "Agent plan", style_transcript_plan()));
            lines.push(field_line("step", step_id, style_accent()));
            for (index, entry) in entries.iter().enumerate() {
                let entry = display_json_value(entry);
                push_multiline(
                    &mut lines,
                    &format!("plan {index}"),
                    &entry,
                    usize::MAX,
                    style_transcript_plan(),
                );
            }
        }
        WorkflowEventKind::StepCompleted {
            step_id,
            action,
            status,
            body,
        } => {
            lines.push(title_line(&stamp, "Step completed", style_success()));
            lines.push(field_line("step", step_id, style_accent()));
            lines.push(field_line("action", action, style_transcript_normal()));
            lines.push(field_line(
                "status",
                status.as_deref().unwrap_or("<none>"),
                status
                    .as_deref()
                    .map(style_for_run_state)
                    .unwrap_or_else(style_transcript_metadata),
            ));
            push_multiline(&mut lines, "body", body, 8, style_transcript_normal());
        }
        WorkflowEventKind::WaitingForInput {
            step,
            prompt_id,
            message,
            choices,
        } => {
            let choices = if choices.is_empty() {
                "<freeform>".to_string()
            } else {
                choices.join(", ")
            };
            lines.push(title_line(&stamp, "Waiting for input", style_warning()));
            lines.push(field_line("step", step, style_accent()));
            lines.push(field_line("prompt", prompt_id, style_transcript_metadata()));
            push_multiline(
                &mut lines,
                "message",
                message,
                usize::MAX,
                style_transcript_normal(),
            );
            lines.push(field_line("choices", &choices, style_warning()));
            lines.push(metadata_line(
                "         Type an answer below and press Enter.",
            ));
        }
        WorkflowEventKind::Suspended { step, reason } => {
            lines.push(title_line(&stamp, "Run suspended", style_warning()));
            lines.push(field_line("step", step, style_accent()));
            lines.push(field_line("reason", reason, style_warning()));
        }
        WorkflowEventKind::RunCompleted => {
            lines.push(title_line(&stamp, "Run completed", style_success()));
            lines.push(field_line(
                "run",
                &event.run_id,
                style_transcript_metadata(),
            ));
        }
        WorkflowEventKind::RunFailed { reason } => {
            lines.push(title_line(&stamp, "Run failed", style_error()));
            lines.push(field_line("reason", reason, style_error()));
            lines.push(Line::from(""));
            lines.push(metadata_line("         Next action"));
            lines.push(Line::from(vec![
                Span::styled(
                    "           Review the failure, then run ",
                    style_transcript_metadata(),
                ),
                Span::styled("/runs", style_transcript_prompt()),
                Span::styled(" or start a new request.", style_transcript_metadata()),
            ]));
        }
        WorkflowEventKind::RunCancelled => {
            lines.push(title_line(&stamp, "Run cancelled", style_error()));
            lines.push(field_line(
                "run",
                &event.run_id,
                style_transcript_metadata(),
            ));
        }
        WorkflowEventKind::RunStatusChanged { status } => {
            lines.push(title_line(
                &stamp,
                "Run status changed",
                style_for_run_state(status),
            ));
            lines.push(field_line("status", status, style_for_run_state(status)));
        }
    }
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    RenderedWorkflowEvent { lines, text }
}

fn title_line(stamp: &str, title: &str, title_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(stamp.to_string(), style_transcript_metadata()),
        Span::styled("  ", style_transcript_metadata()),
        Span::styled(title.to_string(), title_style),
    ])
}

fn field_line(label: &str, value: &str, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("         {label:>7}: "),
            style_transcript_metadata(),
        ),
        Span::styled(value.to_string(), value_style),
    ])
}

fn metadata_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(text.into(), style_transcript_metadata()))
}

fn push_multiline(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &str,
    max_lines: usize,
    style: Style,
) {
    if value.lines().count() <= 1 && !value.contains('`') {
        lines.extend(plain_labeled_lines(label, value, style));
    } else {
        lines.extend(render_labeled_markup(label, value, style, max_lines));
    }
}

fn display_tool_title(title: &str, kind: Option<&str>) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    kind.filter(|kind| !kind.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "<unknown tool>".to_string())
}

fn display_json_value(value: &serde_json::Value) -> String {
    extract_json_text(value).unwrap_or_else(|| "<structured tool result>".to_string())
}

fn extract_json_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => non_empty(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Array(items) => join_text(items.iter().filter_map(extract_json_text)),
        serde_json::Value::Object(object) => [
            "text", "content", "message", "output", "stdout", "stderr", "result", "summary",
        ]
        .into_iter()
        .filter_map(|key| object.get(key))
        .find_map(extract_json_text),
        _ => None,
    }
}

fn join_text(parts: impl Iterator<Item = String>) -> Option<String> {
    let mut joined = String::new();
    for part in parts {
        if !joined.is_empty() && !joined.ends_with('\n') {
            joined.push('\n');
        }
        joined.push_str(&part);
    }
    non_empty(joined)
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::app::styles::{
        style_error, style_success, style_transcript_thought, style_transcript_tool_pending,
        style_warning,
    };

    #[test]
    fn renders_agent_prompt_response_thought_and_tool_events() {
        let rendered = [
            WorkflowEventKind::AgentSessionReady {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                session_id: "session-1".to_string(),
            },
            WorkflowEventKind::AgentPrompt {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                session_id: "session-1".to_string(),
                prompt: "Role: developer\nTask: Do work".to_string(),
            },
            WorkflowEventKind::AgentThought {
                step_id: "implement".to_string(),
                content: "checking approach".to_string(),
            },
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "partial answer".to_string(),
            },
            WorkflowEventKind::AgentToolCall {
                step_id: "implement".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Reading app state".to_string(),
                tool_kind: "read".to_string(),
                status: "pending".to_string(),
            },
        ]
        .into_iter()
        .map(|kind| {
            render_workflow_event(&WorkflowEvent::new("run-1", kind))
                .text()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("Agent session ready"));
        assert!(rendered.contains("role: developer"));
        assert!(rendered.contains("session: session-1"));
        assert!(rendered.contains("Prompt sent to agent"));
        assert!(rendered.contains("prompt: Role: developer"));
        assert!(rendered.contains("Agent thinking"));
        assert!(rendered.contains("thought: checking approach"));
        assert!(rendered.contains("Agent response"));
        assert!(rendered.contains("content: partial answer"));
        assert!(rendered.contains("Agent tool call"));
        assert!(rendered.contains("tool: Reading app state"));
        assert!(!rendered.contains("id: call_abc"));
        assert!(rendered.contains("kind: read"));
        assert!(rendered.contains("status: pending"));
    }

    #[test]
    fn renders_tool_update_content() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Reading app state".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"text":"read complete"})),
            },
        ));

        assert!(rendered.contains("Agent tool update"));
        assert!(rendered.contains("tool: Reading app state"));
        assert!(!rendered.contains("id: call_abc"));
        assert!(rendered.contains("status: completed"));
        assert!(rendered.contains("content: read complete"));
    }

    #[test]
    fn renders_tool_updates_without_ids_and_with_parsed_nested_content() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Reading app state".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({
                    "content": [
                        {"type": "text", "text": "read complete"},
                        {"type": "text", "text": "second line"}
                    ]
                })),
            },
        ));

        assert!(rendered.contains("Agent tool update"));
        assert!(rendered.contains("tool: Reading app state"));
        assert!(!rendered.contains("id: call_abc"), "{}", rendered.text());
        assert!(
            rendered.contains("content: read complete"),
            "{}",
            rendered.text()
        );
        assert!(rendered.contains("second line"), "{}", rendered.text());
        assert!(!rendered.contains("{\"content\""), "{}", rendered.text());
    }

    #[test]
    fn renders_waiting_prompt_message_as_separate_lines() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WaitingForInput {
                step: "confirm_plan".to_string(),
                prompt_id: "plan_confirmation_9".to_string(),
                message: "Review plan\n- first item\n- second item".to_string(),
                choices: Vec::new(),
            },
        ));

        assert!(
            rendered.contains("message: Review plan"),
            "{}",
            rendered.text()
        );
        assert!(
            rendered.contains("                : - first item"),
            "{}",
            rendered.text()
        );
        assert!(
            rendered.contains("                : - second item"),
            "{}",
            rendered.text()
        );
        assert!(
            !rendered.contains("message: Review plan\n-"),
            "{}",
            rendered.text()
        );
    }

    #[test]
    fn renders_common_tool_scalar_fields_without_raw_json() {
        let stdout = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_stdout".to_string(),
                title: "Running command".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"stdout":"command output"})),
            },
        ));
        let result_summary = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_result".to_string(),
                title: "Summarizing".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"result":{"summary":"summary text"}})),
            },
        ));
        let opaque = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_opaque".to_string(),
                title: "Structured result".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"records":[{"id":1}]})),
            },
        ));

        assert!(
            stdout.contains("content: command output"),
            "{}",
            stdout.text()
        );
        assert!(!stdout.contains("{\"stdout\""), "{}", stdout.text());
        assert!(
            result_summary.contains("content: summary text"),
            "{}",
            result_summary.text()
        );
        assert!(
            !result_summary.contains("{\"result\""),
            "{}",
            result_summary.text()
        );
        assert!(
            opaque.contains("content: <structured tool result>"),
            "{}",
            opaque.text()
        );
        assert!(!opaque.contains("records"), "{}", opaque.text());
        assert!(!opaque.contains("{"), "{}", opaque.text());
    }

    #[test]
    fn applies_key_event_styles() {
        let thought = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentThought {
                step_id: "plan".to_string(),
                content: "thinking".to_string(),
            },
        ));
        let response = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentResponse {
                step_id: "plan".to_string(),
                content: "answer".to_string(),
            },
        ));
        let waiting = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WaitingForInput {
                step: "plan".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: Vec::new(),
            },
        ));
        let failed = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunFailed {
                reason: "boom".to_string(),
            },
        ));
        let completed = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunCompleted,
        ));
        let tool_pending = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCall {
                step_id: "plan".to_string(),
                tool_call_id: "call".to_string(),
                title: "Run command".to_string(),
                tool_kind: "bash".to_string(),
                status: "pending".to_string(),
            },
        ));
        let tool_completed = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "plan".to_string(),
                tool_call_id: "call".to_string(),
                title: "Run command".to_string(),
                status: "completed".to_string(),
                content: None,
            },
        ));

        assert!(line_has_style(
            thought.lines(),
            "Agent thinking",
            style_transcript_thought()
        ));
        assert!(line_has_style(
            thought.lines(),
            "thinking",
            style_transcript_thought()
        ));
        assert!(line_has_style(
            response.lines(),
            "answer",
            style_transcript_normal()
        ));
        assert!(line_has_style(
            waiting.lines(),
            "Waiting for input",
            style_warning()
        ));
        assert!(line_has_style(failed.lines(), "Run failed", style_error()));
        assert!(line_has_style(
            completed.lines(),
            "Run completed",
            style_success()
        ));
        assert!(line_has_style(
            tool_pending.lines(),
            "pending",
            style_transcript_tool_pending()
        ));
        assert!(line_has_style(
            tool_completed.lines(),
            "completed",
            style_success()
        ));
    }

    #[test]
    fn response_fenced_rust_gets_syntect_styles() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "```rust\nfn main() { println!(\"hi\"); }\n```".to_string(),
            },
        ));
        let code_line = rendered
            .lines()
            .iter()
            .find(|line| line.to_string().contains("fn main"))
            .unwrap();
        let colors = code_line
            .spans
            .iter()
            .filter_map(|span| span.style.fg)
            .collect::<std::collections::HashSet<Color>>();

        assert!(colors.len() >= 2, "{code_line:?}");
    }

    fn line_has_style(lines: &[Line<'static>], text: &str, style: Style) -> bool {
        lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains(text) && span.style == style)
        })
    }
}
