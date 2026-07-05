use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::markup::render_markup;
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
            lines.push(header_line(
                &stamp,
                "Run started",
                style_accent(),
                vec![
                    ("run", event.run_id.as_str(), style_transcript_metadata()),
                    (
                        "workflow",
                        workflow_name.as_str(),
                        style_transcript_normal(),
                    ),
                    ("step", current_step.as_str(), style_accent()),
                ],
            ));
        }
        WorkflowEventKind::StepStarted { step_id } => {
            lines.push(header_line(
                &stamp,
                "Step started",
                style_accent(),
                vec![("step", step_id.as_str(), style_accent())],
            ));
        }
        WorkflowEventKind::StepProgress { step_id, message } => {
            lines.push(header_line(
                &stamp,
                "Step progress",
                style_accent(),
                vec![("step", step_id.as_str(), style_accent())],
            ));
            push_body(&mut lines, message, usize::MAX, style_transcript_normal());
        }
        WorkflowEventKind::AgentSessionReady {
            step_id,
            role,
            session_id,
        } => {
            lines.push(header_line(
                &stamp,
                "Agent session ready",
                style_accent(),
                vec![
                    ("step", step_id.as_str(), style_accent()),
                    ("role", role.as_str(), style_transcript_normal()),
                    ("session", session_id.as_str(), style_transcript_metadata()),
                ],
            ));
        }
        WorkflowEventKind::AgentPrompt {
            step_id,
            role,
            session_id,
            prompt,
        } => {
            lines.push(header_line(
                &stamp,
                "Prompt sent to agent",
                style_transcript_prompt(),
                vec![
                    ("step", step_id.as_str(), style_accent()),
                    ("role", role.as_str(), style_transcript_prompt()),
                    ("session", session_id.as_str(), style_transcript_metadata()),
                ],
            ));
            push_body(&mut lines, prompt, usize::MAX, style_transcript_prompt());
        }
        WorkflowEventKind::AgentResponse { step_id, content } => {
            lines.push(header_line(
                &stamp,
                "Agent response",
                style_transcript_normal(),
                vec![("step", step_id.as_str(), style_accent())],
            ));
            push_body(&mut lines, content, usize::MAX, style_transcript_normal());
        }
        WorkflowEventKind::AgentThought { step_id, content } => {
            lines.push(header_line(
                &stamp,
                "Agent thinking",
                style_transcript_thought(),
                vec![("step", step_id.as_str(), style_accent())],
            ));
            push_body(&mut lines, content, usize::MAX, style_transcript_thought());
        }
        WorkflowEventKind::AgentToolCall {
            step_id,
            title,
            tool_kind,
            status,
            ..
        } => {
            let tool = display_tool_title(title, Some(tool_kind));
            lines.push(header_line(
                &stamp,
                "Agent tool call",
                style_transcript_tool_pending(),
                vec![
                    ("step", step_id.as_str(), style_accent()),
                    ("tool", tool.as_str(), style_transcript_tool_pending()),
                    ("kind", tool_kind.as_str(), style_transcript_metadata()),
                    ("status", status.as_str(), style_for_tool_status(status)),
                ],
            ));
        }
        WorkflowEventKind::AgentToolCallUpdate {
            step_id,
            title,
            status,
            content,
            ..
        } => {
            let tool = display_tool_title(title, None);
            lines.push(header_line(
                &stamp,
                "Agent tool update",
                style_for_tool_status(status),
                vec![
                    ("step", step_id.as_str(), style_accent()),
                    ("tool", tool.as_str(), style_transcript_tool_pending()),
                    ("status", status.as_str(), style_for_tool_status(status)),
                ],
            ));
            if let Some(content) = content {
                let content = display_tool_update_content(content);
                push_body(&mut lines, &content, usize::MAX, style_transcript_normal());
            }
        }
        WorkflowEventKind::AgentPlan { step_id, entries } => {
            lines.push(header_line(
                &stamp,
                "Agent plan",
                style_transcript_plan(),
                vec![("step", step_id.as_str(), style_accent())],
            ));
            for (index, entry) in entries.iter().enumerate() {
                if index > 0 {
                    lines.push(Line::from(""));
                }
                let entry = display_json_value(entry);
                push_body(&mut lines, &entry, usize::MAX, style_transcript_plan());
            }
        }
        WorkflowEventKind::StepCompleted {
            step_id,
            action,
            status,
            body,
        } => {
            let status_value = status.as_deref().unwrap_or("<none>");
            let status_style = status
                .as_deref()
                .map(style_for_run_state)
                .unwrap_or_else(style_transcript_metadata);
            lines.push(header_line(
                &stamp,
                "Step completed",
                style_success(),
                vec![
                    ("step", step_id.as_str(), style_accent()),
                    ("action", action.as_str(), style_transcript_normal()),
                    ("status", status_value, status_style),
                ],
            ));
            push_body(&mut lines, body, 8, style_transcript_normal());
        }
        WorkflowEventKind::WaitingForInput {
            step,
            prompt_id,
            message,
            choices,
        } => {
            let choices = display_choices(choices);
            lines.push(header_line(
                &stamp,
                "Waiting for input",
                style_warning(),
                vec![
                    ("step", step.as_str(), style_accent()),
                    ("prompt", prompt_id.as_str(), style_transcript_metadata()),
                    ("choices", choices.as_str(), style_warning()),
                ],
            ));
            push_body(&mut lines, message, usize::MAX, style_transcript_normal());
        }
        WorkflowEventKind::RunCompleted => {
            lines.push(header_line(
                &stamp,
                "Run completed",
                style_success(),
                vec![("run", event.run_id.as_str(), style_transcript_metadata())],
            ));
        }
        WorkflowEventKind::RunFailed { reason } => {
            lines.push(header_line(
                &stamp,
                "Run failed",
                style_error(),
                vec![("reason", reason.as_str(), style_error())],
            ));
            lines.push(Line::from(""));
            lines.push(metadata_line("Next action"));
            lines.push(Line::from(vec![
                Span::styled("Review the failure, then run ", style_transcript_metadata()),
                Span::styled("/runs", style_transcript_prompt()),
                Span::styled(" or start a new request.", style_transcript_metadata()),
            ]));
        }
        WorkflowEventKind::RunCancelled => {
            lines.push(header_line(
                &stamp,
                "Run cancelled",
                style_error(),
                vec![("run", event.run_id.as_str(), style_transcript_metadata())],
            ));
        }
        WorkflowEventKind::RunStatusChanged { status } => {
            lines.push(header_line(
                &stamp,
                "Run status changed",
                style_for_run_state(status),
                vec![("status", status.as_str(), style_for_run_state(status))],
            ));
        }
    }
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    RenderedWorkflowEvent { lines, text }
}

fn header_line(
    stamp: &str,
    title: &str,
    title_style: Style,
    metadata: Vec<(&str, &str, Style)>,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(3 + metadata.len() * 2);
    spans.push(Span::styled(stamp.to_string(), style_transcript_metadata()));
    spans.push(Span::styled("  ", style_transcript_metadata()));
    spans.push(Span::styled(title.to_string(), title_style));
    for (label, value, value_style) in metadata {
        spans.push(Span::styled(
            format!("  {label}="),
            style_transcript_metadata(),
        ));
        spans.push(Span::styled(value.to_string(), value_style));
    }
    Line::from(spans)
}

fn metadata_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(text.into(), style_transcript_metadata()))
}

fn push_body(lines: &mut Vec<Line<'static>>, value: &str, max_lines: usize, style: Style) {
    let rendered = render_markup(value, style);
    let truncated = rendered.len() > max_lines;
    lines.extend(rendered.into_iter().take(max_lines));
    if truncated {
        lines.push(metadata_line("..."));
    }
}

fn display_choices(choices: &[String]) -> String {
    if choices.is_empty() {
        "<freeform>".to_string()
    } else {
        choices.join(", ")
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

fn display_tool_update_content(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                return display_tool_update_structured_content(&parsed);
            }

            display_json_value(value)
        }
        _ => display_tool_update_structured_content(value),
    }
}

fn display_tool_update_structured_content(value: &serde_json::Value) -> String {
    extract_json_text(value)
        .or_else(|| extract_tool_update_jobs(value))
        .unwrap_or_else(|| "<structured tool result>".to_string())
}

fn extract_tool_update_jobs(value: &serde_json::Value) -> Option<String> {
    let jobs = value.get("details")?.get("jobs")?.as_array()?;
    join_text(jobs.iter().filter_map(display_tool_update_job))
}

fn display_tool_update_job(job: &serde_json::Value) -> Option<String> {
    let object = job.as_object()?;
    let name = string_field(object, "label")
        .or_else(|| string_field(object, "type"))
        .unwrap_or("job");
    let summary = match string_field(object, "status") {
        Some(status) => format!("{name} ({status})"),
        None => name.to_string(),
    };

    non_empty(summary)
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    object
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
        assert!(rendered.contains("role=developer"));
        assert!(rendered.contains("session=session-1"));
        assert!(rendered.contains("Prompt sent to agent"));
        assert!(rendered.contains("Role: developer"));
        assert!(rendered.contains("Task: Do work"));
        assert!(rendered.contains("Agent thinking"));
        assert!(rendered.contains("checking approach"));
        assert!(rendered.contains("Agent response"));
        assert!(rendered.contains("partial answer"));
        assert!(rendered.contains("Agent tool call"));
        assert!(rendered.contains("tool=Reading app state"));
        assert!(!rendered.contains("id=call_abc"));
        assert!(rendered.contains("kind=read"));
        assert!(rendered.contains("status=pending"));
        assert!(!rendered.contains("prompt: Role: developer"));
        assert!(!rendered.contains("thought: checking approach"));
        assert!(!rendered.contains("content: partial answer"));
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
        assert!(rendered.contains("tool=Reading app state"));
        assert!(!rendered.contains("id=call_abc"));
        assert!(rendered.contains("status=completed"));
        assert!(rendered.contains("read complete"));
        assert!(!rendered.contains("content: read complete"));
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
        assert!(rendered.contains("tool=Reading app state"));
        assert!(!rendered.contains("id=call_abc"), "{}", rendered.text());
        assert!(rendered.contains("read complete"), "{}", rendered.text());
        assert!(rendered.contains("second line"), "{}", rendered.text());
        assert!(
            !rendered.contains("content: read complete"),
            "{}",
            rendered.text()
        );
        assert!(!rendered.contains("{"), "{}", rendered.text());
    }

    #[test]
    fn renders_json_encoded_tool_update_content_as_progress_summary() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Running background task".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!(
                    r#"{"content":[{"type":"text","text":""}],"details":{"jobs":[{"id":"job-123","type":"task","status":"running","label":"TuiLagRegressionTest","durationMs":123798}]}}"#
                )),
            },
        ));

        let text = rendered.text();
        assert!(text.contains("TuiLagRegressionTest"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(!text.contains("{"), "{text}");
        assert!(!text.contains("\"durationMs\""), "{text}");
        assert!(!text.contains("\"details\""), "{text}");
    }

    #[test]
    fn renders_direct_tool_update_job_details_as_progress_summary() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Waiting on tester".to_string(),
                status: "in_progress".to_string(),
                content: Some(serde_json::json!({
                    "details": {
                        "jobs": [{
                            "id": "job-123",
                            "type": "task",
                            "status": "running",
                            "label": "TuiLagRegressionTest",
                            "durationMs": 123798,
                        }]
                    }
                })),
            },
        ));

        let text = rendered.text();
        assert!(text.contains("TuiLagRegressionTest"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(!text.contains("job-123"), "{text}");
        assert!(!text.contains("durationMs"), "{text}");
        assert!(!text.contains("details"), "{text}");
        assert!(!text.contains("{"), "{text}");
    }

    #[test]
    fn renders_waiting_prompt_with_inline_metadata_and_unindented_markdown_body() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WaitingForInput {
                step: "confirm_plan".to_string(),
                prompt_id: "plan_confirmation_9".to_string(),
                message: "Review `plan`\n- first item\n- second item".to_string(),
                choices: Vec::new(),
            },
        ));

        let text = rendered.text();
        let lines = rendered.lines();
        let header = lines[0].to_string();
        assert!(header.contains("Waiting for input"), "{text}");
        assert!(header.contains("step=confirm_plan"), "{text}");
        assert!(header.contains("prompt=plan_confirmation_9"), "{text}");
        assert!(header.contains("choices=<freeform>"), "{text}");
        assert_eq!(lines[1].to_string(), "Review `plan`");
        assert_eq!(lines[2].to_string(), "- first item");
        assert_eq!(lines[3].to_string(), "- second item");
        assert!(!text.contains("message:"), "{text}");
        assert!(!text.contains("                "), "{text}");
        assert!(lines[1].spans.len() > 1, "{:?}", lines[1]);
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

        assert!(stdout.contains("command output"), "{}", stdout.text());
        assert!(
            !stdout.contains("content: command output"),
            "{}",
            stdout.text()
        );
        assert!(!stdout.contains("{"), "{}", stdout.text());
        assert!(
            result_summary.contains("summary text"),
            "{}",
            result_summary.text()
        );
        assert!(
            !result_summary.contains("content: summary text"),
            "{}",
            result_summary.text()
        );
        assert!(!result_summary.contains("{"), "{}", result_summary.text());
        assert!(
            opaque.contains("<structured tool result>"),
            "{}",
            opaque.text()
        );
        assert!(
            !opaque.contains("content: <structured tool result>"),
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
    fn step_completed_body_is_unindented_and_capped() {
        let rendered = render_workflow_event(&WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::StepCompleted {
                step_id: "review".to_string(),
                action: "status".to_string(),
                status: Some("approved".to_string()),
                body: (1..=10)
                    .map(|index| format!("body line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
        ));

        let text = rendered.text();
        assert!(rendered.lines()[0].to_string().contains("step=review"));
        assert_eq!(rendered.lines()[1].to_string(), "body line 1");
        assert_eq!(rendered.lines()[8].to_string(), "body line 8");
        assert_eq!(rendered.lines()[9].to_string(), "...");
        assert!(!text.contains("body:"), "{text}");
        assert!(!text.contains("         "), "{text}");
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
