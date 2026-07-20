use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::state::AppState;
use super::super::styles::style_for_run_state;
use super::chrome::status_metadata_text;

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    frame.render_widget(Paragraph::new(line(state, area.width)), area);
}

pub(in crate::app) fn line(state: &AppState, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        status_metadata_text(state, width as usize),
        style_for_run_state(&state.display_state()),
    ))
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

    use super::*;
    use crate::config::AppConfig;

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(AppConfig {
            state_dir: dir.path().to_path_buf(),
            workflow_store: dir.path().join("workflow.redb"),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        })
    }

    fn apply_started_run(state: &mut AppState) {
        state.apply_workflow_event(WorkflowEvent::new(
            "run-170dc431-7a35-49a5-b4db-9f1219431a1d",
            WorkflowEventKind::RunStarted {
                workflow_name: "agent/00-feature".to_string(),
                current_step: "implement".to_string(),
                request_topic: Some("Add health route".to_string()),
            },
        ));
    }

    fn rendered_text(state: &AppState, width: u16) -> String {
        line(state, width).to_string()
    }

    #[test]
    fn idle_status_renders_only_state_icon() {
        let state = test_state();

        assert_eq!(rendered_text(&state, 160), "○");
    }

    #[test]
    fn active_run_status_renders_compact_metadata() {
        let mut state = test_state();
        apply_started_run(&mut state);

        let rendered = rendered_text(&state, 160);

        assert_eq!(
            rendered,
            "● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature"
        );
        assert!(!rendered.contains("draft allowed"));
        assert!(!rendered.contains("Enter waits for active run"));
    }

    #[test]
    fn waiting_status_renders_waiting_icon_and_prompt_step() {
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

        let rendered = rendered_text(&state, 160);

        assert_eq!(rendered, "◔ · ↳ confirm · ▶ 170dc431 · ⎇ agent/00-feature");
        assert!(!rendered.contains("answer prompt"));
    }

    #[tokio::test]
    async fn background_task_status_omits_task_count() {
        let mut state = test_state();
        apply_started_run(&mut state);
        state.spawn_test_card_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });

        let rendered = rendered_text(&state, 160);

        assert_eq!(
            rendered,
            "● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature"
        );
        assert!(!rendered.contains("◷"), "{rendered}");
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn workflow_status_omits_ambiguous_background_task_count() {
        let mut state = test_state();
        apply_started_run(&mut state);
        state.spawn_test_card_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });

        let rendered = rendered_text(&state, 160);

        assert_eq!(
            rendered,
            "● · ↳ implement · ▶ 170dc431 · ⎇ agent/00-feature"
        );
        assert!(!rendered.contains("◷"), "{rendered}");
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn narrow_status_drops_lower_priority_metadata_first() {
        let mut state = test_state();
        apply_started_run(&mut state);
        state.spawn_test_card_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });

        let rendered = rendered_text(&state, 42);

        assert_eq!(rendered, "● · ↳ implement · ▶ 170dc431");
        state.cancel_background_tasks();
    }
}
