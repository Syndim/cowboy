use cowboy_workflow_engine::WorkflowEventKind;
use ratatui::Terminal;
use ratatui::layout::Position;

use super::state::AppState;
use super::*;
use crate::app::styles::style_transcript_thought;
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
fn draw_wraps_long_input_into_visible_continuation_row() {
    let mut state = test_state();
    state.push_input("abcdefghijklmnop");

    let rendered = rendered_screen(&state, 16, 10);

    assert!(rendered.contains("> abcdefghijkl"));
    assert!(rendered.contains("  mnop"));
}

#[test]
fn draw_places_cursor_at_wrapped_input_end() {
    let mut state = test_state();
    state.push_input("abcdefghijklmnop");
    let backend = ratatui::backend::TestBackend::new(16, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(7, 8));
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

#[test]
fn draw_preserves_transcript_styles() {
    let mut state = test_state();
    state.apply_workflow_event(WorkflowEvent::new(
        "run-1",
        WorkflowEventKind::AgentThought {
            step_id: "plan".to_string(),
            content: "thinking".to_string(),
        },
    ));
    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();

    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    let thought_fg = style_transcript_thought().fg.unwrap();
    let mut found = false;
    for y in 0..buffer.area.height as usize {
        let start = y * width;
        let row = &buffer.content[start..start + width];
        let text = row.iter().map(|cell| cell.symbol()).collect::<String>();
        if text.contains("thought: thinking") {
            let x = text.find("thinking").unwrap();
            assert_eq!(row[x].fg, thought_fg);
            found = true;
        }
    }

    assert!(found, "thought text was not rendered");
}
