use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::state::AppState;
use super::super::styles::style_for_run_state;
use super::chrome::truncate_to_display_width;

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
    let title = match state.current_run_topic() {
        Some(topic) => format!("Cowboy · {topic}"),
        None => "Cowboy".to_string(),
    };

    truncate_to_display_width(title, width)
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
            workflow_store: dir.path().join("data.db"),
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

    fn apply_started_run(state: &mut AppState, request_topic: Option<&str>) {
        state.apply_workflow_event(WorkflowEvent::new(
            "run-170dc431-7a35-49a5-b4db-9f1219431a1d",
            WorkflowEventKind::RunStarted {
                workflow_name: "agent/00-feature".to_string(),
                current_step: "implement".to_string(),
                request_topic: request_topic.map(ToString::to_string),
            },
        ));
    }

    fn assert_no_metadata_text(header: &str) {
        for metadata in [
            "●",
            "○",
            "◔",
            "↳",
            "▶",
            "⎇",
            "◷",
            "step:",
            "run:",
            "workflow:",
            "tasks:",
        ] {
            assert!(!header.contains(metadata), "{metadata} leaked in {header}");
        }
    }

    #[test]
    fn active_run_header_renders_agent_topic_only() {
        let mut state = test_state();
        apply_started_run(&mut state, Some("Add health route"));

        let header = text(&state, 120);

        assert_eq!(header, "Cowboy · Add health route");
        assert_no_metadata_text(&header);
    }

    #[test]
    fn idle_header_renders_cowboy_only() {
        let state = test_state();

        let header = text(&state, 120);

        assert_eq!(header, "Cowboy");
        assert_no_metadata_text(&header);
    }

    #[test]
    fn run_without_topic_keeps_cowboy_title() {
        let mut state = test_state();
        apply_started_run(&mut state, None);

        let header = text(&state, 120);

        assert_eq!(header, "Cowboy");
        assert_no_metadata_text(&header);
    }

    #[test]
    fn header_truncates_long_topic() {
        let mut state = test_state();
        apply_started_run(
            &mut state,
            Some("Add a health check route with detailed diagnostics"),
        );

        let header = text(&state, 24);

        assert_eq!(header, "Cowboy · Add a health c…");
        assert_eq!(unicode_width::UnicodeWidthStr::width(header.as_str()), 24);
    }

    #[tokio::test]
    async fn background_task_metadata_does_not_render_in_header() {
        let mut state = test_state();
        apply_started_run(&mut state, Some("Background topic"));
        state.spawn_test_card_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });

        let header = text(&state, 120);

        assert_eq!(header, "Cowboy · Background topic");
        assert_no_metadata_text(&header);
        state.cancel_background_tasks();
    }

    #[test]
    fn unicode_width_bounds_topic_header() {
        let mut state = test_state();
        apply_started_run(&mut state, Some("重要任务整理"));

        let header = text(&state, 16);

        assert_eq!(header, "Cowboy · 重要任…");
        assert_eq!(unicode_width::UnicodeWidthStr::width(header.as_str()), 16);
    }
}
