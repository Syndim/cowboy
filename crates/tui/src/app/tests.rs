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

fn rendered_rows(state: &AppState, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    buffer
        .content
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect()
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
fn tui_input_cursor_style_uses_unix_block_cursor() {
    assert_eq!(tui_input_cursor_style(), SetCursorStyle::BlinkingBlock);
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
fn draw_places_cursor_at_moved_single_line_position() {
    let mut state = test_state();
    state.push_input("abc");
    state.set_input_cursor(1);
    let backend = ratatui::backend::TestBackend::new(40, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(4, 8));
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
fn draw_places_cursor_at_moved_wrapped_input_position() {
    let mut state = test_state();
    state.push_input("abcdefghijklmnop");
    state.set_input_cursor("abcdefghijkl".chars().count());
    let backend = ratatui::backend::TestBackend::new(16, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(14, 7));
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
        if text.contains("│thinking") {
            let x = text.find("thinking").unwrap();
            assert_eq!(row[x].fg, thought_fg);
            found = true;
        }
    }

    assert!(found, "thought text was not rendered");
}

#[test]
fn draw_narrow_short_terminal_keeps_tail_status_and_composer_borders() {
    let mut state = test_state();
    state.apply_workflow_event(WorkflowEvent::new(
        "run-1",
        WorkflowEventKind::AgentResponse {
            step_id: "review".to_string(),
            content: "first second third fourth fifth sixth seventh eighth TAILVISIBLE".to_string(),
        },
    ));
    state.apply_workflow_event(WorkflowEvent::new(
        "run-1",
        WorkflowEventKind::WaitingForInput {
            step: "confirm_result".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: Vec::new(),
        },
    ));

    let rows = rendered_rows(&state, 38, 14);
    let rendered = rows.join("\n");

    assert!(rendered.contains("TAILVISIBLE"), "{rendered}");
    assert!(rendered.contains("waiting for input"), "{rendered}");
    assert!(
        rows.iter()
            .any(|row| row.contains("┌ Enter answers active prompt")),
        "{rendered}"
    );
    assert!(
        rows.last().is_some_and(|row| row.starts_with('└')),
        "{rendered}"
    );
}

#[test]
fn draw_scheduler_draws_first_frame_then_stays_clean_until_dirty() {
    let mut scheduler = DrawScheduler::new();

    assert!(scheduler.should_draw());
    scheduler.mark_clean();
    assert!(!scheduler.should_draw());

    scheduler.mark_dirty();

    assert!(scheduler.should_draw());
}

#[test]
fn draw_scheduler_only_marks_dirty_for_changed_state() {
    let mut scheduler = DrawScheduler::new();
    scheduler.mark_clean();

    scheduler.mark_dirty_if(false);
    assert!(!scheduler.should_draw());

    scheduler.mark_dirty_if(true);
    assert!(scheduler.should_draw());
}
