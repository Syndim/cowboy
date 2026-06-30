use ratatui::Terminal;
use ratatui::layout::Position;

use super::state::AppState;
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

fn rendered_screen(state: &AppState, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .fold(String::new(), |mut rendered, cell| {
            rendered.push_str(cell.symbol());
            rendered
        })
}

#[test]
fn idle_draw_hides_debug_paths() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("visible-state-dir");
    let workflow_store = dir.path().join("visible-workflow.redb");
    let state = AppState::new(AppConfig {
        state_dir: state_dir.clone(),
        workflow_store: workflow_store.clone(),
        max_steps_per_run: 1,
        max_visits_per_step: 1,
        ..AppConfig::default()
    });

    let rendered = rendered_screen(&state, 100, 16);

    assert!(rendered.contains("No workflow transcript yet."));
    assert!(!rendered.contains("visible-state-dir"));
    assert!(!rendered.contains("visible-workflow.redb"));
    assert!(!rendered.contains(state_dir.to_string_lossy().as_ref()));
    assert!(!rendered.contains(workflow_store.to_string_lossy().as_ref()));
}

#[test]
fn slash_suggestions_are_safe_in_short_terminals() {
    let mut state = test_state();
    state.push_input("/");
    let backend = ratatui::backend::TestBackend::new(40, 5);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();
}

#[test]
fn draw_places_cursor_at_input_end() {
    let mut state = test_state();
    state.push_input("abc");
    let backend = ratatui::backend::TestBackend::new(40, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(6, 8));
}

#[test]
fn draw_handles_input_taller_than_input_box() {
    let mut state = test_state();
    state.push_input(
        &std::iter::repeat_n("pasted line", 100)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();
}
