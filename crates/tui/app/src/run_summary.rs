use chrono::{DateTime, FixedOffset, Local};
use cowboy_workflow_engine::{RunStatusDetail, RunSummaryLine};

fn format_started_at(started_at: DateTime<FixedOffset>) -> String {
    started_at.format("%Y-%m-%d %H:%M:%S %:z").to_string()
}

pub fn render_run_summary_lines(run: &RunSummaryLine) -> Vec<String> {
    let mut lines = vec![run.run_id.clone()];
    let started_at = run
        .started_at
        .map(|started_at| format_started_at(started_at.with_timezone(&Local).fixed_offset()))
        .unwrap_or_else(|| "<unknown>".to_string());
    lines.push(format!("  started_at: {started_at}"));
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
            started_at: None,
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
                "  started_at: <unknown>",
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
                "  started_at: <unknown>",
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
                "  started_at: <unknown>",
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

    #[test]
    fn format_started_at_preserves_non_utc_wall_clock_and_offset() {
        let offset = FixedOffset::east_opt(8 * 60 * 60).unwrap();
        let started_at = DateTime::parse_from_rfc3339("2026-07-28T13:40:44+08:00").unwrap();

        assert_eq!(
            format_started_at(started_at.with_timezone(&offset)),
            "2026-07-28 13:40:44 +08:00"
        );
        assert_ne!(
            format_started_at(started_at.with_timezone(&offset)),
            "2026-07-28 05:40:44 +00:00"
        );
    }

    #[test]
    fn render_run_summary_lines_converts_known_timestamp_to_local_time() {
        let mut run = summary_with_status(RunStatus::Completed, None);
        run.started_at = Some(
            DateTime::parse_from_rfc3339("2026-07-28T05:40:44Z")
                .unwrap()
                .to_utc(),
        );
        let expected = format!(
            "  started_at: {}",
            format_started_at(run.started_at.unwrap().with_timezone(&Local).fixed_offset())
        );

        let lines = render_run_summary_lines(&run);

        assert_eq!(lines[1], expected);
    }
}
