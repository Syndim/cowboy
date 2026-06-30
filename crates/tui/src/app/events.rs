use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

pub(super) fn render_workflow_event(event: &WorkflowEvent) -> String {
    let stamp = event.timestamp.format("%H:%M:%S");
    let mut lines = Vec::new();
    match &event.kind {
        WorkflowEventKind::RunStarted {
            workflow_name,
            current_step,
        } => {
            lines.push(format!("{stamp}  Run started"));
            lines.push(format!("         run: {}", event.run_id));
            lines.push(format!("         workflow: {workflow_name}"));
            lines.push(format!("         step: {current_step}"));
        }
        WorkflowEventKind::StepStarted { step_id } => {
            lines.push(format!("{stamp}  Step started"));
            lines.push(format!("         step: {step_id}"));
        }
        WorkflowEventKind::StepProgress { step_id, message } => {
            lines.push(format!("{stamp}  Step progress"));
            lines.push(format!("         step: {step_id}"));
            push_multiline(&mut lines, "message", message, usize::MAX);
        }
        WorkflowEventKind::AgentSessionReady {
            step_id,
            role,
            session_id,
        } => {
            lines.push(format!("{stamp}  Agent session ready"));
            lines.push(format!("         step: {step_id}"));
            lines.push(format!("         role: {role}"));
            lines.push(format!("         session: {session_id}"));
        }
        WorkflowEventKind::AgentPrompt {
            step_id,
            role,
            session_id,
            prompt,
        } => {
            lines.push(format!("{stamp}  Prompt sent to agent"));
            lines.push(format!("         step: {step_id}"));
            lines.push(format!("         role: {role}"));
            lines.push(format!("         session: {session_id}"));
            push_multiline(&mut lines, "prompt", prompt, usize::MAX);
        }
        WorkflowEventKind::AgentResponse { step_id, content } => {
            lines.push(format!("{stamp}  Agent response"));
            lines.push(format!("         step: {step_id}"));
            push_multiline(&mut lines, "content", content, usize::MAX);
        }
        WorkflowEventKind::AgentThought { step_id, content } => {
            lines.push(format!("{stamp}  Agent thinking"));
            lines.push(format!("         step: {step_id}"));
            push_multiline(&mut lines, "thought", content, usize::MAX);
        }
        WorkflowEventKind::AgentToolCall {
            step_id,
            title,
            tool_kind,
            status,
            ..
        } => {
            lines.push(format!("{stamp}  Agent tool call"));
            lines.push(format!("         step: {step_id}"));
            lines.push(format!(
                "         tool: {}",
                display_tool_title(title, Some(tool_kind))
            ));
            lines.push(format!("         kind: {tool_kind}"));
            lines.push(format!("         status: {status}"));
        }
        WorkflowEventKind::AgentToolCallUpdate {
            step_id,
            title,
            status,
            content,
            ..
        } => {
            lines.push(format!("{stamp}  Agent tool update"));
            lines.push(format!("         step: {step_id}"));
            lines.push(format!(
                "         tool: {}",
                display_tool_title(title, None)
            ));
            lines.push(format!("         status: {status}"));
            if let Some(content) = content {
                let content = display_json_value(content);
                push_multiline(&mut lines, "content", &content, usize::MAX);
            }
        }
        WorkflowEventKind::AgentPlan { step_id, entries } => {
            lines.push(format!("{stamp}  Agent plan"));
            lines.push(format!("         step: {step_id}"));
            for (index, entry) in entries.iter().enumerate() {
                let entry = display_json_value(entry);
                push_multiline(&mut lines, &format!("plan {index}"), &entry, usize::MAX);
            }
        }
        WorkflowEventKind::StepCompleted {
            step_id,
            action,
            status,
            body,
        } => {
            lines.push(format!("{stamp}  Step completed"));
            lines.push(format!("         step: {step_id}"));
            lines.push(format!("         action: {action}"));
            lines.push(format!(
                "         status: {}",
                status.as_deref().unwrap_or("<none>")
            ));
            push_multiline(&mut lines, "body", body, 8);
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
            lines.push(format!("{stamp}  Waiting for input"));
            lines.push(format!("         step: {step}"));
            lines.push(format!("         prompt: {prompt_id}"));
            lines.push(format!("         message: {message}"));
            lines.push(format!("         choices: {choices}"));
            lines.push("         Type an answer below and press Enter.".to_string());
        }
        WorkflowEventKind::Suspended { step, reason } => {
            lines.push(format!("{stamp}  Run suspended"));
            lines.push(format!("         step: {step}"));
            lines.push(format!("         reason: {reason}"));
        }
        WorkflowEventKind::RunCompleted => {
            lines.push(format!("{stamp}  Run completed"));
            lines.push(format!("         run: {}", event.run_id));
        }
        WorkflowEventKind::RunFailed { reason } => {
            lines.push(format!("{stamp}  Run failed"));
            lines.push(format!("         reason: {reason}"));
            lines.push("".to_string());
            lines.push("         Next action".to_string());
            lines.push(
                "           Review the failure, then run /runs or start a new request.".to_string(),
            );
        }
        WorkflowEventKind::RunCancelled => {
            lines.push(format!("{stamp}  Run cancelled"));
            lines.push(format!("         run: {}", event.run_id));
        }
        WorkflowEventKind::RunStatusChanged { status } => {
            lines.push(format!("{stamp}  Run status changed"));
            lines.push(format!("         status: {status}"));
        }
    }
    lines.join("\n")
}

fn push_multiline(lines: &mut Vec<String>, label: &str, value: &str, max_lines: usize) {
    for (index, line) in value.lines().take(max_lines).enumerate() {
        let label = if index == 0 { label } else { "" };
        lines.push(format!("         {label:>7}: {line}"));
    }
    if value.lines().count() > max_lines {
        lines.push("                ...".to_string());
    }
    if value.is_empty() {
        lines.push(format!("         {label:>7}:"));
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
        joined.push_str(&part);
    }
    non_empty(joined)
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        .map(|kind| render_workflow_event(&WorkflowEvent::new("run-1", kind)))
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
        assert!(!rendered.contains("id: call_abc"), "{rendered}");
        assert!(rendered.contains("content: read complete"), "{rendered}");
        assert!(rendered.contains("second line"), "{rendered}");
        assert!(!rendered.contains("{\"content\""), "{rendered}");
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

        assert!(stdout.contains("content: command output"), "{stdout}");
        assert!(!stdout.contains("{\"stdout\""), "{stdout}");
        assert!(
            result_summary.contains("content: summary text"),
            "{result_summary}"
        );
        assert!(!result_summary.contains("{\"result\""), "{result_summary}");
        assert!(
            opaque.contains("content: <structured tool result>"),
            "{opaque}"
        );
        assert!(!opaque.contains("records"), "{opaque}");
        assert!(!opaque.contains("{"), "{opaque}");
    }
}
