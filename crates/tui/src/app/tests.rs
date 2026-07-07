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
fn windows_terminal_mode_does_not_execute_unsupported_keyboard_enhancement_on_windows() {
    let source = include_str!("../app.rs");
    let lines: Vec<_> = source.lines().collect();
    let unsupported_commands = ["PushKeyboardEnhancementFlags", "PopKeyboardEnhancementFlags"];
    let mut unguarded_commands = Vec::new();

    for command in unsupported_commands {
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let executes_command = trimmed.starts_with(command)
                && (trimmed.contains(',') || trimmed.contains('('));

            if !executes_command {
                continue;
            }

            let guard_start = index.saturating_sub(4);
            let guard_context = lines[guard_start..=index].join("\n");
            let guarded_for_non_windows = guard_context.contains("not(windows)")
                || guard_context.contains("cfg(unix)")
                || guard_context.contains("cfg!(unix)")
                || guard_context.contains("!cfg!(windows)");

            if !guarded_for_non_windows {
                unguarded_commands.push(format!("line {}: {}", index + 1, trimmed));
            }
        }
    }

    assert!(
        unguarded_commands.is_empty(),
        "Windows legacy console rejects crossterm keyboard enhancement commands with \
         `Keyboard progressive enhancement not implemented for the legacy Windows API.`; \
         gate these commands away from Windows before entering or restoring terminal mode: {}",
        unguarded_commands.join("; ")
    );
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
fn draw_with_typed_input_does_not_scale_with_full_transcript_history() {
    fn state_with_transcript_entries(entries: usize) -> AppState {
        let mut state = test_state();
        for index in 0..entries {
            let content = if index + 1 == entries {
                format!("transcript entry {index:05} LAG_TAIL_VISIBLE")
            } else {
                format!("transcript entry {index:05} filler text for redraw scaling")
            };
            state.apply_workflow_event(WorkflowEvent::new(
                "run-1",
                WorkflowEventKind::AgentResponse {
                    step_id: "review".to_string(),
                    content,
                },
            ));
        }

        state.push_input("typed input stays responsive");
        state
    }

    fn timed_draw(state: &AppState) -> (std::time::Duration, String) {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let started = std::time::Instant::now();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        let elapsed = started.elapsed();
        let rendered =
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .fold(String::new(), |mut rendered, cell| {
                    rendered.push_str(cell.symbol());
                    rendered
                });
        std::hint::black_box(rendered.len());
        (elapsed, rendered)
    }

    let short_state = state_with_transcript_entries(64);
    let long_state = state_with_transcript_entries(20_000);

    let (short_draw, short_rendered) = timed_draw(&short_state);
    let (long_draw, long_rendered) = timed_draw(&long_state);
    let budget = short_draw
        .checked_mul(8)
        .unwrap_or(std::time::Duration::MAX)
        + std::time::Duration::from_millis(3);

    assert!(
        short_rendered.contains("> typed input stays responsive"),
        "{short_rendered}"
    );
    assert!(
        long_rendered.contains("> typed input stays responsive"),
        "{long_rendered}"
    );
    assert!(
        long_rendered.contains("LAG_TAIL_VISIBLE"),
        "{long_rendered}"
    );
    assert!(
        long_draw <= budget,
        "redrawing typed input should render only visible transcript tail rows, not scale with \
         the full transcript; 64 entries took {short_draw:?}, 20_000 entries took \
         {long_draw:?}, budget was {budget:?}"
    );
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
