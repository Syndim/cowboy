use cowboy_workflow_engine::{RunStatusDetail, RunSummaryLine};

pub fn render_run_summary_lines(run: &RunSummaryLine) -> Vec<String> {
    let mut lines = vec![run.run_id.clone()];
    if let Some(topic) = &run.topic {
        lines.push(format!("  topic: {topic}"));
    }

    lines.push(format!("  workflow: {}", run.workflow_name));
    lines.push(format!("  current_step: {}", run.current_step));
    lines.push(format!(
        "  head: {}",
        run.head_step.as_deref().unwrap_or("<none>")
    ));
    lines.extend(render_status_detail_lines("  ", &run.status_detail));
    lines
}

pub fn render_status_detail_lines(prefix: &str, status: &RunStatusDetail) -> Vec<String> {
    let mut lines = vec![format!("{prefix}status: {}", status.state.as_str())];
    if let Some(reason) = &status.reason {
        lines.push(format!("{prefix}status.reason: {reason}"));
    }

    if let Some(waiting_step) = &status.waiting_step {
        lines.push(format!("{prefix}status.waiting_step: {waiting_step}"));
    }

    if let Some(prompt_id) = &status.prompt_id {
        lines.push(format!("{prefix}status.prompt_id: {prompt_id}"));
    }

    if let Some(message) = &status.message {
        lines.push(format!("{prefix}status.message: {message}"));
    }

    if status.state.as_str() == "waiting_for_input" {
        let choices = if status.choices.is_empty() {
            "<free-form>".to_string()
        } else {
            status.choices.join(", ")
        };
        lines.push(format!("{prefix}status.choices: {choices}"));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowboy_workflow_core::{ResumeCallback, RunStatus};

    fn summary_with_status(status: RunStatus, topic: Option<&str>) -> RunSummaryLine {
        let status_detail = RunStatusDetail::from_status(&status);
        RunSummaryLine {
            run_id: "run-123".to_string(),
            workflow_name: "deploy".to_string(),
            topic: topic.map(ToString::to_string),
            status,
            status_detail,
            current_step: "ship".to_string(),
            head_step: Some("record-9".to_string()),
        }
    }

    fn assert_no_debug_status_payload(rendered: &str) {
        for fragment in ["WaitingForInput {", "Failed {", "resume_callback:"] {
            assert!(
                !rendered.contains(fragment),
                "rendered summary leaked Rust debug fragment {fragment:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn render_run_summary_lines_includes_topic_and_structured_completed_status() {
        let run = summary_with_status(RunStatus::Completed, Some("Ship deployment"));

        let lines = render_run_summary_lines(&run);

        assert_eq!(
            lines,
            vec![
                "run-123",
                "  topic: Ship deployment",
                "  workflow: deploy",
                "  current_step: ship",
                "  head: record-9",
                "  status: completed",
            ]
        );
        assert_no_debug_status_payload(&lines.join("\n"));
    }

    #[test]
    fn render_run_summary_lines_expands_waiting_status_without_resume_debug() {
        let run = summary_with_status(
            RunStatus::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "prompt-42".to_string(),
                message: "Approve release?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
                resume_callback: ResumeCallback::new(
                    "ask_user",
                    serde_json::json!({ "prompt_id": "prompt-42" }),
                )
                .unwrap(),
            },
            Some("Approve release"),
        );

        let lines = render_run_summary_lines(&run);

        assert_eq!(
            lines,
            vec![
                "run-123",
                "  topic: Approve release",
                "  workflow: deploy",
                "  current_step: ship",
                "  head: record-9",
                "  status: waiting_for_input",
                "  status.waiting_step: approve",
                "  status.prompt_id: prompt-42",
                "  status.message: Approve release?",
                "  status.choices: yes, no",
            ]
        );
        assert_no_debug_status_payload(&lines.join("\n"));
    }

    #[test]
    fn render_run_summary_lines_expands_failed_status_reason_without_enum_debug() {
        let run = summary_with_status(
            RunStatus::Failed {
                reason: "agent command exited 2".to_string(),
            },
            Some("Diagnose failure"),
        );

        let lines = render_run_summary_lines(&run);

        assert_eq!(
            lines,
            vec![
                "run-123",
                "  topic: Diagnose failure",
                "  workflow: deploy",
                "  current_step: ship",
                "  head: record-9",
                "  status: failed",
                "  status.reason: agent command exited 2",
            ]
        );
        assert_no_debug_status_payload(&lines.join("\n"));
    }
}
