use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::state::AppState;
use super::super::styles::{style_for_run_state, truncate_to_width};

pub(in crate::app) fn render(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    frame.render_widget(Paragraph::new(line(state, area.width)), area);
}

pub(in crate::app) fn line(state: &AppState, width: u16) -> Line<'static> {
    let text = if let Some(prompt) = state.pending_prompt() {
        format!(
            "waiting for input ─ answer prompt:{} ─ /answer still supported",
            prompt.prompt_id()
        )
    } else if state.background_task_count() > 0 {
        format!(
            "{} ─ draft allowed ─ Enter waits for active run ─ Esc cancel ─ Ctrl-U/Ctrl-D scroll ─ End follow ─ Ctrl-C exit ─ tasks:{}",
            state.display_state(),
            state.background_task_count()
        )
    } else if state.event_log_is_empty() {
        "ready ─ Enter submits ─ Shift/Ctrl-Enter newline ─ type / for commands ─ Ctrl-C exit"
            .to_string()
    } else {
        format!(
            "{} ─ {} ─ Ctrl-U/Ctrl-D scroll ─ End follow ─ Ctrl-C exit ─ /help",
            state.display_state(),
            state.status()
        )
    };
    Line::from(Span::styled(
        truncate_to_width(text, width as usize),
        style_for_run_state(&state.display_state()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn active_run_status_says_draft_allowed_and_enter_waits() {
        let mut state = test_state();
        state.spawn_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });

        let rendered = line(&state, 160).to_string();

        assert!(rendered.contains("draft allowed"));
        assert!(rendered.contains("Enter waits for active run"));
        assert!(!rendered.contains("input disabled"));
        state.cancel_background_tasks();
    }
}
