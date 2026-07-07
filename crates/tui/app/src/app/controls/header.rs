use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::state::AppState;
use super::super::styles::{style_for_run_state, truncate_to_width};

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
    let step = state.current_step().map(|step| format!("step:{step}"));
    let run = state.active_run_id().map(|run_id| {
        let short = run_id
            .strip_prefix("run-")
            .and_then(|rest| rest.split('-').next())
            .filter(|segment| segment.len() >= 8)
            .unwrap_or(run_id);
        format!("run:{short}")
    });
    let workflow = state
        .workflow_name()
        .map(|workflow| format!("workflow:{workflow}"));
    let tasks = (state.background_task_count() > 0)
        .then(|| format!("tasks:{}", state.background_task_count()));

    let mut parts = vec!["Cowboy".to_string(), display_state];
    if state.active_run_id().is_none() {
        parts.push("no active run".to_string());
    }
    for optional in [
        step.as_ref(),
        run.as_ref(),
        workflow.as_ref(),
        tasks.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        parts.push(optional.clone());
    }

    while width > 0 && parts.join(" ─ ").chars().count() > width && parts.len() > 2 {
        if let Some(index) = parts.iter().position(|part| part.starts_with("tasks:")) {
            parts.remove(index);
        } else if let Some(index) = parts.iter().position(|part| part.starts_with("workflow:")) {
            parts.remove(index);
        } else if let Some(index) = parts.iter().position(|part| part.starts_with("run:")) {
            parts.remove(index);
        } else if let Some(index) = parts.iter().position(|part| part.starts_with("step:")) {
            parts.remove(index);
        } else {
            break;
        }
    }

    truncate_to_width(parts.join(" ─ "), width)
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

    #[test]
    fn header_drops_low_priority_fields_when_narrow() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-170dc431-7a35-49a5-b4db-9f1219431a1d",
            WorkflowEventKind::RunStarted {
                workflow_name: "agent/00-feature".to_string(),
                current_step: "implement".to_string(),
            },
        ));
        state.set_status("idle");

        let header = text(&state, 32);

        assert!(header.contains("Cowboy"));
        assert!(header.contains("running"));
        assert!(!header.contains("workflow:"));
        assert!(header.chars().count() <= 32);
    }
}
