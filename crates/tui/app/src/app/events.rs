use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};
use ratatui::text::{Line, Span};

use super::card::{Card, CardMetadata, CardSection, CardTone, DEFAULT_CARD_WIDTH};
use super::controls::chrome::status_icon;
use super::markup::render_markup;
use super::styles::{
    style_error, style_for_run_state, style_for_tool_status, style_transcript_metadata,
    style_transcript_normal, style_transcript_plan, style_transcript_prompt,
    style_transcript_thought, style_warning,
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
}

pub(super) fn render_workflow_event(event: &WorkflowEvent) -> RenderedWorkflowEvent {
    render_workflow_event_width(event, DEFAULT_CARD_WIDTH)
}

pub(super) fn render_workflow_event_width(
    event: &WorkflowEvent,
    width: usize,
) -> RenderedWorkflowEvent {
    let card = workflow_event_card(event);
    let lines = card.render(width);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    RenderedWorkflowEvent { lines, text }
}

fn workflow_event_card(event: &WorkflowEvent) -> Card {
    match &event.kind {
        WorkflowEventKind::RunStarted {
            workflow_name,
            current_step,
            ..
        } => Card::new(status_icon("running"), "Run started", CardTone::Accent).metadata([
            CardMetadata::step(current_step),
            CardMetadata::run(&event.run_id),
            CardMetadata::workflow(workflow_name),
        ]),
        WorkflowEventKind::StepStarted { step_id } => {
            Card::new(status_icon("running"), "Step started", CardTone::Accent).metadata([
                CardMetadata::step(step_id),
                CardMetadata::run(&event.run_id),
            ])
        }
        WorkflowEventKind::StepProgress { step_id, message } => {
            Card::new(status_icon("running"), "Step progress", CardTone::Accent)
                .metadata([
                    CardMetadata::step(step_id),
                    CardMetadata::run(&event.run_id),
                ])
                .section(CardSection::body(render_markup(
                    message,
                    style_transcript_normal(),
                )))
        }
        WorkflowEventKind::AgentSessionReady { step_id, .. } => Card::new(
            status_icon("running"),
            "Agent session ready",
            CardTone::Accent,
        )
        .metadata([
            CardMetadata::step(step_id),
            CardMetadata::run(&event.run_id),
        ]),
        WorkflowEventKind::AgentPrompt {
            step_id, prompt, ..
        } => Card::new(
            status_icon("running"),
            "Prompt sent to agent",
            CardTone::Prompt,
        )
        .metadata([
            CardMetadata::step(step_id),
            CardMetadata::run(&event.run_id),
        ])
        .section(CardSection::named(
            "Prompt",
            render_markup(prompt, style_transcript_prompt()),
        )),
        WorkflowEventKind::AgentResponse { step_id, content } => {
            Card::new(status_icon("running"), "Agent response", CardTone::Neutral)
                .metadata([
                    CardMetadata::step(step_id),
                    CardMetadata::run(&event.run_id),
                ])
                .section(CardSection::body(render_markup(
                    content,
                    style_transcript_normal(),
                )))
        }
        WorkflowEventKind::AgentThought { step_id, content } => {
            Card::new(status_icon("running"), "Agent thinking", CardTone::Thought)
                .metadata([
                    CardMetadata::step(step_id),
                    CardMetadata::run(&event.run_id),
                ])
                .section(CardSection::body(render_markup(
                    content,
                    style_transcript_thought(),
                )))
        }
        WorkflowEventKind::AgentToolCall {
            step_id,
            title,
            tool_kind,
            status,
            ..
        } => {
            let tool = display_tool_title(title, Some(tool_kind));
            Card::new(tool_status_icon(status), tool, CardTone::Tool)
                .tool_marker()
                .metadata([
                    CardMetadata::step(step_id),
                    CardMetadata::run(&event.run_id),
                ])
                .section(CardSection::body(vec![Line::from(vec![
                    Span::styled("Status: ", style_transcript_metadata()),
                    Span::styled(status.clone(), style_for_tool_status(status)),
                ])]))
        }
        WorkflowEventKind::AgentToolCallUpdate {
            step_id,
            title,
            status,
            content,
            ..
        } => {
            let tool = display_tool_title(title, None);
            let mut card = Card::new(tool_status_icon(status), tool, tool_tone(status))
                .tool_marker()
                .metadata([
                    CardMetadata::step(step_id),
                    CardMetadata::run(&event.run_id),
                ])
                .section(CardSection::body(vec![Line::from(vec![
                    Span::styled("Status: ", style_transcript_metadata()),
                    Span::styled(status.clone(), style_for_tool_status(status)),
                ])]));
            if let Some(content) = content {
                let content = display_tool_update_content(content);
                card = card.section(CardSection::named(
                    "Output",
                    render_markup(&content, style_transcript_normal()),
                ));
            }
            card
        }
        WorkflowEventKind::AgentPlan { step_id, entries } => {
            let lines = entries
                .iter()
                .flat_map(|entry| {
                    render_markup(&display_json_value(entry), style_transcript_plan())
                })
                .collect::<Vec<_>>();
            Card::new(status_icon("running"), "Agent plan", CardTone::Plan)
                .metadata([
                    CardMetadata::step(step_id),
                    CardMetadata::run(&event.run_id),
                ])
                .section(CardSection::body(lines))
        }
        WorkflowEventKind::StepCompleted {
            step_id,
            action,
            status,
            body,
        } => {
            let status_value = status.as_deref().unwrap_or("<none>");
            Card::new(
                status_icon("completed"),
                "Step completed",
                CardTone::Success,
            )
            .metadata([
                CardMetadata::step(step_id),
                CardMetadata::run(&event.run_id),
            ])
            .section(CardSection::body(vec![Line::from(vec![
                Span::styled("Action: ", style_transcript_metadata()),
                Span::styled(action.clone(), style_transcript_normal()),
                Span::styled(" · Status: ", style_transcript_metadata()),
                Span::styled(status_value.to_string(), style_for_run_state(status_value)),
            ])]))
            .section(
                CardSection::named("Body", render_markup(body, style_transcript_normal()))
                    .capped(8),
            )
        }
        WorkflowEventKind::WaitingForInput {
            step,
            message,
            choices,
            ..
        } => {
            let mut card = Card::new(
                status_icon("waiting"),
                "Waiting for input",
                CardTone::Warning,
            )
            .metadata([CardMetadata::step(step), CardMetadata::run(&event.run_id)])
            .section(CardSection::body(render_markup(
                message,
                style_transcript_normal(),
            )));
            if !choices.is_empty() {
                card = card.section(CardSection::named(
                    "Choices",
                    vec![Line::from(Span::styled(
                        display_choices(choices),
                        style_warning(),
                    ))],
                ));
            }
            card
        }
        WorkflowEventKind::RunCompleted => {
            Card::new(status_icon("completed"), "Run completed", CardTone::Success)
                .metadata([CardMetadata::run(&event.run_id)])
        }
        WorkflowEventKind::StepRetrying {
            step_id,
            attempt,
            max_attempts,
            reason,
        } => Card::new(status_icon("retrying"), "Step retrying", CardTone::Warning)
            .metadata([
                CardMetadata::step(step_id),
                CardMetadata::run(&event.run_id),
            ])
            .section(CardSection::body(vec![Line::from(Span::styled(
                format!("attempt {attempt}/{max_attempts}"),
                style_warning(),
            ))]))
            .section(CardSection::body(render_markup(
                reason,
                style_transcript_metadata(),
            ))),
        WorkflowEventKind::ManuallyResolved { step_id, status } => Card::new(
            status_icon("completed"),
            "Manually resolved",
            CardTone::Success,
        )
        .metadata([
            CardMetadata::step(step_id),
            CardMetadata::run(&event.run_id),
        ])
        .section(CardSection::body(vec![Line::from(vec![
            Span::styled("Status: ", style_transcript_metadata()),
            Span::styled(status.clone(), style_for_run_state(status)),
        ])])),
        WorkflowEventKind::RunFailed { reason } => {
            Card::new(status_icon("failed"), "Run failed", CardTone::Error)
                .metadata([CardMetadata::run(&event.run_id)])
                .section(CardSection::body(render_markup(reason, style_error())))
                .section(CardSection::named(
                    "Next action",
                    vec![Line::from(vec![
                        Span::styled(
                            "List resolvable statuses with ",
                            style_transcript_metadata(),
                        ),
                        Span::styled(
                            format!("/resolve {}", event.run_id),
                            style_transcript_prompt(),
                        ),
                        Span::styled(
                            ", then resolve with /resolve <run> <status>.",
                            style_transcript_metadata(),
                        ),
                    ])],
                ))
        }
        WorkflowEventKind::RunCancelled => {
            Card::new(status_icon("cancelled"), "Run cancelled", CardTone::Error)
                .metadata([CardMetadata::run(&event.run_id)])
        }
        WorkflowEventKind::RunStatusChanged { status } => Card::new(
            status_icon(status),
            "Run status changed",
            run_state_tone(status),
        )
        .metadata([CardMetadata::run(&event.run_id)])
        .section(CardSection::body(vec![Line::from(Span::styled(
            status.clone(),
            style_for_run_state(status),
        ))])),
    }
}

fn tool_status_icon(status: &str) -> &'static str {
    match status.to_ascii_lowercase().as_str() {
        "completed" | "success" | "succeeded" | "done" => status_icon("completed"),
        "failed" | "error" => status_icon("failed"),
        "cancelled" | "canceled" => status_icon("cancelled"),
        "waiting" | "warning" => status_icon("waiting"),
        _ => status_icon("running"),
    }
}

fn tool_tone(status: &str) -> CardTone {
    match status.to_ascii_lowercase().as_str() {
        "completed" | "success" | "succeeded" | "done" => CardTone::Success,
        "failed" | "error" | "cancelled" | "canceled" => CardTone::Error,
        "waiting" | "warning" => CardTone::Warning,
        _ => CardTone::Tool,
    }
}

fn run_state_tone(status: &str) -> CardTone {
    match status {
        "completed" => CardTone::Success,
        "waiting" => CardTone::Warning,
        "failed" | "cancelled" => CardTone::Error,
        "running" => CardTone::Accent,
        _ => CardTone::Neutral,
    }
}

fn display_choices(choices: &[String]) -> String {
    if choices.is_empty() {
        "<freeform>".to_string()
    } else {
        choices.join(" · ")
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
    use ratatui::style::{Color, Style};

    use super::*;
    use crate::app::styles::{
        style_error, style_success, style_transcript_thought, style_transcript_tool_pending,
        style_warning,
    };

    fn event(kind: WorkflowEventKind) -> WorkflowEvent {
        WorkflowEvent::new("run-170dc431-abc", kind)
    }

    fn rendered_text(kind: WorkflowEventKind) -> String {
        render_workflow_event(&event(kind)).text().to_string()
    }

    #[test]
    fn renders_lifecycle_cards_with_icon_metadata() {
        let rendered = [
            WorkflowEventKind::RunStarted {
                workflow_name: "bugfix".to_string(),
                current_step: "plan".to_string(),
                request_topic: None,
            },
            WorkflowEventKind::StepStarted {
                step_id: "implement".to_string(),
            },
            WorkflowEventKind::StepProgress {
                step_id: "implement".to_string(),
                message: "Running formatter".to_string(),
            },
            WorkflowEventKind::AgentSessionReady {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                session_id: "session-1".to_string(),
            },
            WorkflowEventKind::StepRetrying {
                step_id: "implement".to_string(),
                attempt: 2,
                max_attempts: 3,
                reason: "missing frontmatter".to_string(),
            },
            WorkflowEventKind::ManuallyResolved {
                step_id: "review".to_string(),
                status: "approved".to_string(),
            },
            WorkflowEventKind::RunCompleted,
            WorkflowEventKind::RunFailed {
                reason: "boom".to_string(),
            },
            WorkflowEventKind::RunCancelled,
            WorkflowEventKind::RunStatusChanged {
                status: "waiting".to_string(),
            },
        ]
        .into_iter()
        .map(rendered_text)
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("● Run started · ↳ plan · ▶ 170dc431 · ⎇ bugfix"));
        assert!(rendered.contains("● Step started · ↳ implement · ▶ 170dc431"));
        assert!(rendered.contains("Running formatter"));
        assert!(rendered.contains("● Agent session ready · ↳ implement · ▶ 170dc431"));
        assert!(!rendered.contains("Role: developer"), "{rendered}");
        assert!(!rendered.contains("Session: session-1"), "{rendered}");
        assert!(rendered.contains("↻ Step retrying · ↳ implement · ▶ 170dc431"));
        assert!(rendered.contains("attempt 2/3"));
        assert!(rendered.contains("✓ Manually resolved · ↳ review · ▶ 170dc431"));
        assert!(rendered.contains("✓ Run completed · ▶ 170dc431"));
        assert!(rendered.contains("✗ Run failed · ▶ 170dc431"));
        assert!(rendered.contains("■ Run cancelled · ▶ 170dc431"));
        assert!(rendered.contains("◔ Run status changed · ▶ 170dc431"));
        assert!(rendered.contains("╭"));
        assert!(rendered.contains("╰"));
        assert!(!rendered.contains("step="), "{rendered}");
        assert!(!rendered.contains("run="), "{rendered}");
        assert!(!rendered.contains("workflow="), "{rendered}");
        assert!(!rendered.contains("tasks="), "{rendered}");
    }

    #[test]
    fn renders_agent_prompt_thought_response_and_plan_cards() {
        let rendered = [
            WorkflowEventKind::AgentPrompt {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                session_id: "session-1".to_string(),
                prompt: "Task: Do work".to_string(),
            },
            WorkflowEventKind::AgentThought {
                step_id: "implement".to_string(),
                content: "checking approach".to_string(),
            },
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "partial answer".to_string(),
            },
            WorkflowEventKind::AgentPlan {
                step_id: "implement".to_string(),
                entries: vec![serde_json::json!("- Add card renderer")],
            },
        ]
        .into_iter()
        .map(rendered_text)
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("● Prompt sent to agent · ↳ implement · ▶ 170dc431"));
        assert!(rendered.contains("├─── Prompt "), "{rendered}");
        assert!(!rendered.contains("Role: developer"), "{rendered}");
        assert!(!rendered.contains("session-1"), "{rendered}");
        assert!(rendered.contains("Task: Do work"));
        assert!(rendered.contains("● Agent thinking · ↳ implement · ▶ 170dc431"));
        assert!(rendered.contains("checking approach"));
        assert!(rendered.contains("● Agent response · ↳ implement · ▶ 170dc431"));
        assert!(rendered.contains("partial answer"));
        assert!(rendered.contains("● Agent plan · ↳ implement · ▶ 170dc431"));
        assert!(rendered.contains("- Add card renderer"));
        assert!(!rendered.contains("role=developer"), "{rendered}");
        assert!(!rendered.contains("prompt: Task: Do work"), "{rendered}");
        assert!(
            !rendered.contains("thought: checking approach"),
            "{rendered}"
        );
    }

    #[test]
    fn renders_tool_cards_with_output_sections_and_suppressed_json() {
        let pending = render_workflow_event(&event(WorkflowEventKind::AgentToolCall {
            step_id: "implement".to_string(),
            tool_call_id: "call_abc".to_string(),
            title: "Reading app state".to_string(),
            tool_kind: "read".to_string(),
            status: "pending".to_string(),
        }));
        let completed = render_workflow_event(&event(WorkflowEventKind::AgentToolCallUpdate {
            step_id: "implement".to_string(),
            tool_call_id: "call_abc".to_string(),
            title: "Reading app state".to_string(),
            status: "completed".to_string(),
            content: Some(serde_json::json!({"text":"read complete"})),
        }));
        let pending_text = pending.text();
        let completed_text = completed.text();

        assert!(pending_text.contains("● • Reading app state · ↳ implement · ▶ 170dc431"));
        assert!(completed_text.contains("✓ • Reading app state · ↳ implement · ▶ 170dc431"));
        assert!(completed_text.contains("├─── Output "));
        assert!(completed_text.contains("read complete"));
        assert!(!completed_text.contains("call_abc"), "{completed_text}");
        assert!(!completed_text.contains("{\"text\""), "{completed_text}");
    }

    #[test]
    fn renders_tool_update_summaries_without_raw_json() {
        let encoded = render_workflow_event(&event(WorkflowEventKind::AgentToolCallUpdate {
            step_id: "implement".to_string(),
            tool_call_id: "call_abc".to_string(),
            title: "Running background task".to_string(),
            status: "completed".to_string(),
            content: Some(serde_json::json!(
                r#"{"content":[{"type":"text","text":""}],"details":{"jobs":[{"id":"job-123","type":"task","status":"running","label":"TuiLagRegressionTest","durationMs":123798}]}}"#
            )),
        }));
        let direct = render_workflow_event(&event(WorkflowEventKind::AgentToolCallUpdate {
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
        }));
        let scalar = render_workflow_event(&event(WorkflowEventKind::AgentToolCallUpdate {
            step_id: "implement".to_string(),
            tool_call_id: "call_stdout".to_string(),
            title: "Running command".to_string(),
            status: "completed".to_string(),
            content: Some(serde_json::json!({"stdout":"command output"})),
        }));
        let opaque = render_workflow_event(&event(WorkflowEventKind::AgentToolCallUpdate {
            step_id: "implement".to_string(),
            tool_call_id: "call_opaque".to_string(),
            title: "Structured result".to_string(),
            status: "completed".to_string(),
            content: Some(serde_json::json!({"records":[{"id":1}]})),
        }));
        let text = [encoded.text(), direct.text(), scalar.text(), opaque.text()].join("\n");

        assert!(text.contains("TuiLagRegressionTest"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("command output"), "{text}");
        assert!(text.contains("<structured tool result>"), "{text}");
        assert!(!text.contains("job-123"), "{text}");
        assert!(!text.contains("durationMs"), "{text}");
        assert!(!text.contains("records"), "{text}");
        assert!(!text.contains("{"), "{text}");
    }

    #[test]
    fn renders_waiting_and_completed_cards_with_sections() {
        let waiting = render_workflow_event(&event(WorkflowEventKind::WaitingForInput {
            step: "confirm_plan".to_string(),
            prompt_id: "plan_confirmation_9".to_string(),
            message: "Review `plan`\n- first item\n- second item".to_string(),
            choices: vec!["approve".to_string(), "reject".to_string()],
        }));
        let completed = render_workflow_event(&event(WorkflowEventKind::StepCompleted {
            step_id: "review".to_string(),
            action: "status".to_string(),
            status: Some("approved".to_string()),
            body: (1..=10)
                .map(|index| format!("body line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }));
        let waiting_text = waiting.text();
        let completed_text = completed.text();

        assert!(waiting_text.contains("◔ Waiting for input · ↳ confirm_plan · ▶ 170dc431"));
        assert!(waiting_text.contains("Review `plan`"));
        assert!(waiting_text.contains("├─── Choices "));
        assert!(waiting_text.contains("approve · reject"));
        assert!(!waiting_text.contains("prompt="), "{waiting_text}");
        assert!(waiting.lines()[1].to_string().starts_with('╭'));
        assert!(completed_text.contains("✓ Step completed · ↳ review · ▶ 170dc431"));
        assert!(completed_text.contains("├─── Body "));
        assert!(completed_text.contains("body line 1"));
        assert!(completed_text.contains("body line 8"));
        assert!(completed_text.contains("… 2 more rows"));
        assert!(!completed_text.contains("body:"), "{completed_text}");
    }

    #[test]
    fn applies_key_event_styles_inside_cards() {
        let thought = render_workflow_event(&event(WorkflowEventKind::AgentThought {
            step_id: "plan".to_string(),
            content: "thinking".to_string(),
        }));
        let waiting = render_workflow_event(&event(WorkflowEventKind::WaitingForInput {
            step: "plan".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: Vec::new(),
        }));
        let failed = render_workflow_event(&event(WorkflowEventKind::RunFailed {
            reason: "boom".to_string(),
        }));
        let completed = render_workflow_event(&event(WorkflowEventKind::RunCompleted));
        let tool_pending = render_workflow_event(&event(WorkflowEventKind::AgentToolCall {
            step_id: "plan".to_string(),
            tool_call_id: "call".to_string(),
            title: "Run command".to_string(),
            tool_kind: "bash".to_string(),
            status: "pending".to_string(),
        }));
        let tool_completed =
            render_workflow_event(&event(WorkflowEventKind::AgentToolCallUpdate {
                step_id: "plan".to_string(),
                tool_call_id: "call".to_string(),
                title: "Run command".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"text":"completed"})),
            }));

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
    fn response_fenced_rust_gets_syntect_styles_inside_card() {
        let rendered = render_workflow_event(&event(WorkflowEventKind::AgentResponse {
            step_id: "implement".to_string(),
            content: "```rust\nfn main() { println!(\"hi\"); }\n```".to_string(),
        }));
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
