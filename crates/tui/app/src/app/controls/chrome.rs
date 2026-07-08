use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::state::AppState;

pub(in crate::app) const METADATA_SEPARATOR: &str = " · ";

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataPartKind {
    Fixed,
    Step,
    Run,
    Workflow,
    Tasks,
}

struct MetadataPart {
    text: String,
    kind: MetadataPartKind,
}

impl MetadataPart {
    fn fixed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: MetadataPartKind::Fixed,
        }
    }

    fn optional(text: impl Into<String>, kind: MetadataPartKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

pub(in crate::app) fn status_icon(status: &str) -> &'static str {
    match status {
        "idle" => "○",
        "running" => "●",
        "waiting" => "◔",
        "retrying" => "↻",
        "completed" => "✓",
        "failed" => "✗",
        "cancelled" | "canceled" => "■",
        _ => "?",
    }
}

pub(in crate::app) fn short_run_id(run_id: &str) -> &str {
    run_id
        .strip_prefix("run-")
        .and_then(|rest| rest.split('-').next())
        .filter(|segment| segment.len() >= 8)
        .unwrap_or(run_id)
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn joined_width(parts: &[MetadataPart]) -> usize {
    parts
        .iter()
        .map(|part| display_width(&part.text))
        .sum::<usize>()
        + display_width(METADATA_SEPARATOR).saturating_mul(parts.len().saturating_sub(1))
}

fn remove_lowest_priority_part(parts: &mut Vec<MetadataPart>) -> bool {
    for kind in [
        MetadataPartKind::Tasks,
        MetadataPartKind::Workflow,
        MetadataPartKind::Run,
        MetadataPartKind::Step,
    ] {
        if let Some(index) = parts.iter().position(|part| part.kind == kind) {
            parts.remove(index);
            return true;
        }
    }

    false
}

fn join_parts(parts: &[MetadataPart]) -> String {
    parts
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join(METADATA_SEPARATOR)
}

pub(in crate::app) fn truncate_to_display_width(text: impl AsRef<str>, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let text = text.as_ref();
    if display_width(text) <= width {
        return text.to_string();
    }

    let target = width.saturating_sub(display_width("…"));
    let mut truncated = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > target {
            break;
        }

        truncated.push(ch);
        used += ch_width;
    }

    truncated.push('…');
    truncated
}

pub(super) fn status_metadata_text(state: &AppState, width: usize) -> String {
    let display_state = state.display_state();
    let mut parts = vec![MetadataPart::fixed(status_icon(&display_state))];

    if let Some(step) = state.current_step() {
        parts.push(MetadataPart::optional(
            format!("↳ {step}"),
            MetadataPartKind::Step,
        ));
    }

    if let Some(run_id) = state.active_run_id() {
        parts.push(MetadataPart::optional(
            format!("▶ {}", short_run_id(run_id)),
            MetadataPartKind::Run,
        ));
    }

    if let Some(workflow) = state.workflow_name() {
        parts.push(MetadataPart::optional(
            format!("⎇ {workflow}"),
            MetadataPartKind::Workflow,
        ));
    }

    if state.background_task_count() > 0 {
        parts.push(MetadataPart::optional(
            format!("◷ {}", state.background_task_count()),
            MetadataPartKind::Tasks,
        ));
    }

    while width > 0 && joined_width(&parts) > width && remove_lowest_priority_part(&mut parts) {}

    truncate_to_display_width(join_parts(&parts), width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
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
    fn status_icon_contract_covers_all_states() {
        assert_eq!(status_icon("idle"), "○");
        assert_eq!(status_icon("running"), "●");
        assert_eq!(status_icon("waiting"), "◔");
        assert_eq!(status_icon("retrying"), "↻");
        assert_eq!(status_icon("completed"), "✓");
        assert_eq!(status_icon("failed"), "✗");
        assert_eq!(status_icon("cancelled"), "■");
        assert_eq!(status_icon("unknown"), "?");
    }

    #[tokio::test]
    async fn metadata_uses_shared_icons_and_separator() {
        let mut state = test_state();
        state.apply_workflow_event(cowboy_workflow_engine::WorkflowEvent::new(
            "run-170dc431-abc",
            cowboy_workflow_engine::WorkflowEventKind::RunStarted {
                workflow_name: "bugfix".to_string(),
                current_step: "implement".to_string(),
                request_topic: None,
            },
        ));
        state.spawn_report_task("background".to_string(), async { Err("held".to_string()) });

        let metadata = status_metadata_text(&state, 80);

        assert!(metadata.contains("●"), "{metadata}");
        assert!(metadata.contains("↳ implement"), "{metadata}");
        assert!(metadata.contains("▶ 170dc431"), "{metadata}");
        assert!(metadata.contains("⎇ bugfix"), "{metadata}");
        assert!(metadata.contains("◷ 1"), "{metadata}");
        assert!(metadata.contains(METADATA_SEPARATOR), "{metadata}");
        assert!(!metadata.contains("step="), "{metadata}");
        assert!(!metadata.contains("run="), "{metadata}");
        assert!(!metadata.contains("workflow="), "{metadata}");
        assert!(!metadata.contains("tasks="), "{metadata}");
    }

    #[test]
    fn unicode_truncation_preserves_boundaries() {
        assert_eq!(truncate_to_display_width("实现实现", 5), "实现…");
        assert_eq!(truncate_to_display_width("abcdef", 4), "abc…");
        assert_eq!(truncate_to_display_width("abcdef", 6), "abcdef");
    }
}
