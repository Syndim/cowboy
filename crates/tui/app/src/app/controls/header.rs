use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::state::AppState;
use super::super::styles::style_for_run_state;

const SEPARATOR: &str = " ─ ";

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderPartKind {
    Fixed,
    Step,
    Run,
    Workflow,
    Tasks,
}

struct HeaderPart {
    text: String,
    kind: HeaderPartKind,
}

impl HeaderPart {
    fn fixed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: HeaderPartKind::Fixed,
        }
    }

    fn optional(text: impl Into<String>, kind: HeaderPartKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

fn status_icon(status: &str) -> &'static str {
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

fn short_run_id(run_id: &str) -> &str {
    run_id
        .strip_prefix("run-")
        .and_then(|rest| rest.split('-').next())
        .filter(|segment| segment.len() >= 8)
        .unwrap_or(run_id)
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn joined_width(parts: &[HeaderPart]) -> usize {
    parts
        .iter()
        .map(|part| display_width(&part.text))
        .sum::<usize>()
        + display_width(SEPARATOR).saturating_mul(parts.len().saturating_sub(1))
}

fn remove_lowest_priority_part(parts: &mut Vec<HeaderPart>) -> bool {
    for kind in [
        HeaderPartKind::Tasks,
        HeaderPartKind::Workflow,
        HeaderPartKind::Run,
        HeaderPartKind::Step,
    ] {
        if let Some(index) = parts.iter().position(|part| part.kind == kind) {
            parts.remove(index);
            return true;
        }
    }

    false
}

fn join_parts(parts: &[HeaderPart]) -> String {
    parts
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join(SEPARATOR)
}

fn truncate_to_display_width(text: impl AsRef<str>, width: usize) -> String {
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

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    frame.render_widget(Paragraph::new(line(state, area.width)), area);
}

pub(in crate::app) fn line(state: &AppState, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        text(state, width as usize),
        style_for_run_state(&state.display_state()).add_modifier(Modifier::BOLD),
    ))
}

pub(in crate::app) fn text(state: &AppState, width: usize) -> String {
    let display_state = state.display_state();
    let mut parts = vec![
        HeaderPart::fixed("Cowboy"),
        HeaderPart::fixed(status_icon(&display_state)),
    ];

    if let Some(step) = state.current_step() {
        parts.push(HeaderPart::optional(
            format!("↳ {step}"),
            HeaderPartKind::Step,
        ));
    }

    if let Some(run_id) = state.active_run_id() {
        parts.push(HeaderPart::optional(
            format!("▶ {}", short_run_id(run_id)),
            HeaderPartKind::Run,
        ));
    }

    if let Some(workflow) = state.workflow_name() {
        parts.push(HeaderPart::optional(
            format!("⎇ {workflow}"),
            HeaderPartKind::Workflow,
        ));
    }

    if state.background_task_count() > 0 {
        parts.push(HeaderPart::optional(
            format!("◷ {}", state.background_task_count()),
            HeaderPartKind::Tasks,
        ));
    }

    while width > 0 && joined_width(&parts) > width && remove_lowest_priority_part(&mut parts) {}

    truncate_to_display_width(join_parts(&parts), width)
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

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

    fn apply_started_run(state: &mut AppState) {
        state.apply_workflow_event(WorkflowEvent::new(
            "run-170dc431-7a35-49a5-b4db-9f1219431a1d",
            WorkflowEventKind::RunStarted {
                workflow_name: "agent/00-feature".to_string(),
                current_step: "implement".to_string(),
            },
        ));
    }

    fn assert_no_verbose_header_text(header: &str) {
        for verbose in [
            "running",
            "waiting",
            "completed",
            "failed",
            "cancelled",
            "canceled",
            "step:",
            "run:",
            "workflow:",
            "tasks:",
            "no active run",
        ] {
            assert!(!header.contains(verbose), "{verbose} leaked in {header}");
        }
    }

    #[test]
    fn active_run_header_uses_compact_symbols_and_omits_verbose_labels() {
        let mut state = test_state();
        apply_started_run(&mut state);

        let header = text(&state, 120);
        assert_eq!(
            header,
            "Cowboy ─ ● ─ ↳ implement ─ ▶ 170dc431 ─ ⎇ agent/00-feature"
        );
        assert_no_verbose_header_text(&header);
    }

    #[test]
    fn idle_header_uses_icon_without_no_active_run_copy() {
        let state = test_state();

        let header = text(&state, 120);
        assert_eq!(header, "Cowboy ─ ○");
        assert_no_verbose_header_text(&header);
    }

    #[test]
    fn waiting_header_uses_status_icon_without_status_word() {
        let mut state = test_state();
        apply_started_run(&mut state);
        state.apply_workflow_event(WorkflowEvent::new(
            "run-170dc431-7a35-49a5-b4db-9f1219431a1d",
            WorkflowEventKind::WaitingForInput {
                step: "confirm".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
            },
        ));

        let header = text(&state, 120);

        assert!(header.contains("◔"), "{header}");
        assert!(!header.contains("waiting"), "{header}");
    }

    #[test]
    fn status_icon_maps_known_states() {
        assert_eq!(status_icon("idle"), "○");
        assert_eq!(status_icon("running"), "●");
        assert_eq!(status_icon("waiting"), "◔");
        assert_eq!(status_icon("retrying"), "↻");
        assert_eq!(status_icon("completed"), "✓");
        assert_eq!(status_icon("failed"), "✗");
        assert_eq!(status_icon("cancelled"), "■");
        assert_eq!(status_icon("canceled"), "■");
        assert_eq!(status_icon("paused"), "?");
    }

    #[test]
    fn header_drops_low_priority_fields_when_narrow() {
        let mut state = test_state();
        apply_started_run(&mut state);

        let header = text(&state, 40);

        assert!(header.contains("Cowboy"), "{header}");
        assert!(header.contains("●"), "{header}");
        assert!(header.contains("↳ implement"), "{header}");
        assert!(header.contains("▶ 170dc431"), "{header}");
        assert!(!header.contains("⎇"), "{header}");
        assert!(display_width(&header) <= 40, "{header}");
    }

    #[tokio::test]
    async fn background_task_symbol_renders_and_drops_first() {
        let mut state = test_state();
        apply_started_run(&mut state);
        state.spawn_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });

        let full = text(&state, 120);
        let narrow = text(&state, display_width(&full) - 1);
        assert_eq!(
            full,
            "Cowboy ─ ● ─ ↳ implement ─ ▶ 170dc431 ─ ⎇ agent/00-feature ─ ◷ 1"
        );
        assert!(!narrow.contains("◷"), "{narrow}");
        assert!(narrow.contains("⎇ agent/00-feature"), "{narrow}");
        assert!(display_width(&narrow) < display_width(&full), "{narrow}");
        state.cancel_background_tasks();
    }

    #[test]
    fn unicode_width_bounds_symbol_header() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-170dc431-7a35-49a5-b4db-9f1219431a1d",
            WorkflowEventKind::RunStarted {
                workflow_name: "重要工作流".to_string(),
                current_step: "implement".to_string(),
            },
        ));

        let header = text(&state, 52);

        assert!(display_width(&header) <= 52, "{header}");
    }
}
