use cowboy_workflow_engine::WorkflowEventKind;
use ratatui::Terminal;
use ratatui::layout::Position;

use super::state::AppState;
use super::*;
use crate::app::styles::{
    style_border_accent, style_muted, style_transcript_thought, style_warning,
};
use crate::config::AppConfig;
use std::cell::Cell;
use std::io::{self, Write};

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

fn composer_border_fg_for_title(
    state: &AppState,
    width: u16,
    height: u16,
    title_marker: &str,
) -> ratatui::style::Color {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    let mut rendered_rows = Vec::new();

    for row in buffer.content.chunks(width) {
        let text = row.iter().map(|cell| cell.symbol()).collect::<String>();
        if text.contains(title_marker) {
            assert_eq!(row[0].symbol(), "┌", "{text}");
            return row[0].fg;
        }
        rendered_rows.push(text);
    }

    panic!(
        "composer title containing `{title_marker}` not found in rendered buffer:\n{}",
        rendered_rows.join("\n")
    );
}

#[test]
fn idle_draw_hides_debug_paths() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("visible-state-dir");
    let workflow_store = dir.path().join("visible-workflow.redb");
    let state = AppState::new(AppConfig {
        state_dir: state_dir.clone(),
        workflow_store: workflow_store.clone(),
        config_sets: std::collections::BTreeMap::from([(
            "default".to_string(),
            crate::config::ConfigSetConfig {
                max_steps_per_run: 1,
                max_visits_per_step: 1,
                ..Default::default()
            },
        )]),
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
    let unsupported_commands = [
        "PushKeyboardEnhancementFlags",
        "PopKeyboardEnhancementFlags",
    ];
    let mut unguarded_commands = Vec::new();

    for command in unsupported_commands {
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let executes_command =
                trimmed.starts_with(command) && (trimmed.contains(',') || trimmed.contains('('));

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

struct FailAfterMouseCapture {
    bytes: Vec<u8>,
    failures_remaining: usize,
}

impl Write for FailAfterMouseCapture {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.failures_remaining > 0
            && self
                .bytes
                .windows(b"?1000h".len())
                .any(|part| part == b"?1000h")
        {
            self.failures_remaining -= 1;
            return Err(io::Error::other("injected terminal setup failure"));
        }

        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn terminal_screen_commands_pair_mouse_capture_and_bracketed_paste() {
    let mut bytes = Vec::new();

    enter_terminal_screen(&mut bytes).unwrap();
    restore_terminal_screen(&mut bytes).unwrap();

    let commands = String::from_utf8(bytes).unwrap();
    assert!(commands.contains("?1000h"), "{commands:?}");
    assert!(commands.contains("?1000l"), "{commands:?}");
    assert!(commands.contains("?2004h"), "{commands:?}");
    assert!(commands.contains("?2004l"), "{commands:?}");
}

#[test]
fn terminal_screen_setup_failure_rolls_back_mouse_capture() {
    let mut writer = FailAfterMouseCapture {
        bytes: Vec::new(),
        failures_remaining: 1,
    };

    let error = enter_terminal_screen(&mut writer).unwrap_err();

    let commands = String::from_utf8(writer.bytes).unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(writer.failures_remaining, 0);
    assert!(commands.contains("?1000h"), "{commands:?}");
    assert!(commands.contains("?1000l"), "{commands:?}");
    assert!(commands.contains("?2004l"), "{commands:?}");
}

#[test]
fn terminal_guard_retries_screen_cleanup_after_setup_rollback_fails() {
    let mut writer = FailAfterMouseCapture {
        bytes: Vec::new(),
        failures_remaining: 2,
    };
    let mut guard = TerminalModeGuard {
        raw_mode_active: false,
        screen_active: false,
        keyboard_enhancement_active: false,
    };

    let setup_result =
        guard.activate_screen(|| enter_terminal_screen(&mut writer).map_err(anyhow::Error::from));

    assert!(setup_result.is_err());
    assert!(guard.screen_active);
    assert_eq!(writer.failures_remaining, 0);

    guard
        .restore_with(
            || Ok(()),
            || Ok(()),
            || restore_terminal_screen(&mut writer).map_err(anyhow::Error::from),
        )
        .unwrap();

    let commands = String::from_utf8(writer.bytes).unwrap();
    assert!(guard.is_restored());
    assert!(commands.contains("?1000l"), "{commands:?}");
    assert!(commands.contains("?2004l"), "{commands:?}");
}

#[test]
fn terminal_guard_retries_only_failed_restoration_components() {
    let raw_calls = Cell::new(0);
    let keyboard_calls = Cell::new(0);
    let screen_calls = Cell::new(0);
    let mut guard = TerminalModeGuard {
        raw_mode_active: true,
        screen_active: true,
        keyboard_enhancement_active: true,
    };

    let first_result = guard.restore_with(
        || {
            raw_calls.set(raw_calls.get() + 1);
            Ok(())
        },
        || {
            keyboard_calls.set(keyboard_calls.get() + 1);
            Ok(())
        },
        || {
            screen_calls.set(screen_calls.get() + 1);
            Err(anyhow::anyhow!("injected screen restoration failure"))
        },
    );

    assert!(first_result.is_err());
    assert!(!guard.is_restored());
    assert!(!guard.raw_mode_active);
    assert!(!guard.keyboard_enhancement_active);
    assert!(guard.screen_active);

    guard
        .restore_with(
            || {
                raw_calls.set(raw_calls.get() + 1);
                Ok(())
            },
            || {
                keyboard_calls.set(keyboard_calls.get() + 1);
                Ok(())
            },
            || {
                screen_calls.set(screen_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    assert!(guard.is_restored());
    assert_eq!(raw_calls.get(), 1);
    assert_eq!(keyboard_calls.get(), 1);
    assert_eq!(screen_calls.get(), 2);
}

#[test]
fn shared_layout_tracks_composer_height_and_preserves_region_boundaries() {
    let mut state = test_state();
    let area = Rect::new(0, 0, 60, 20);
    let compact = AppLayout::new(area, &state);
    state.push_input("one\ntwo\nthree\nfour");
    let expanded = AppLayout::new(area, &state);

    assert_eq!(compact.header, Rect::new(0, 0, 60, 1));
    assert!(expanded.composer.height > compact.composer.height);
    assert!(expanded.transcript.height < compact.transcript.height);
    assert_eq!(expanded.header.bottom(), expanded.transcript.y);
    assert_eq!(expanded.transcript.bottom(), expanded.status.y);
    assert_eq!(expanded.status.bottom(), expanded.composer.y);
    assert_eq!(expanded.composer.bottom(), area.bottom());
}

fn scroll_to_oldest(state: &mut AppState, layout: AppLayout) {
    loop {
        let limit = transcript::next_scroll_limit(state, layout.transcript);
        state.set_transcript_scroll_limit(limit);
        if !state.scroll_events_up() {
            break;
        }
    }

    assert!(state.scroll_offset() > 0);
    assert!(!state.is_following_events());
}

fn assert_new_tail_visible(state: &AppState, layout: AppLayout, marker: &str) {
    let rows = transcript::lines(
        state,
        layout.transcript.height as usize,
        layout.transcript.width as usize,
    );
    assert!(rows.iter().any(|line| line.to_string().contains(marker)));
}

#[test]
fn terminal_growth_reconciles_scroll_state_and_follows_new_tail() {
    let mut state = test_state();
    state.push_card("Transcript", (0..10).map(|index| format!("line {index}")));
    let short_layout = AppLayout::new(Rect::new(0, 0, 80, 9), &state);
    scroll_to_oldest(&mut state, short_layout);

    let tall_layout = AppLayout::new(Rect::new(0, 0, 80, 40), &state);
    assert_eq!(
        transcript::current_scroll_limit(&state, tall_layout.transcript),
        0
    );
    reconcile_layout_transition(&mut state, short_layout, tall_layout);

    assert_eq!(state.scroll_offset(), 0);
    assert!(state.is_following_events());
    state.push_card("Notice", ["TERMINAL_GROWTH_TAIL".to_string()]);
    assert_new_tail_visible(&state, tall_layout, "TERMINAL_GROWTH_TAIL");
}

#[test]
fn composer_shrink_reconciles_scroll_state_and_follows_new_tail() {
    let mut state = test_state();
    state.push_card("Transcript", (0..10).map(|index| format!("line {index}")));
    state.push_input(&"composer line\n".repeat(20));
    let area = Rect::new(0, 0, 80, 25);
    let expanded_composer_layout = AppLayout::new(area, &state);
    scroll_to_oldest(&mut state, expanded_composer_layout);

    assert!(state.take_submitted_input().is_some());
    let shrunk_composer_layout = AppLayout::new(area, &state);
    assert!(shrunk_composer_layout.transcript.height > expanded_composer_layout.transcript.height);
    assert_eq!(
        transcript::current_scroll_limit(&state, shrunk_composer_layout.transcript),
        0
    );
    reconcile_layout_transition(&mut state, expanded_composer_layout, shrunk_composer_layout);

    assert_eq!(state.scroll_offset(), 0);
    assert!(state.is_following_events());
    state.push_card("Notice", ["COMPOSER_SHRINK_TAIL".to_string()]);
    assert_new_tail_visible(&state, shrunk_composer_layout, "COMPOSER_SHRINK_TAIL");
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

#[tokio::test]
async fn draw_places_cursor_in_active_run_draft_input() {
    let mut state = test_state();
    state.push_input("abc");
    state.spawn_test_card_report_task("pending".to_string(), async {
        std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
            .await
    });
    let backend = ratatui::backend::TestBackend::new(40, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| draw(frame, &state)).unwrap();

    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(6, 8));
    state.cancel_background_tasks();
}

#[tokio::test]
async fn paste_appends_to_active_run_draft_input() {
    let mut state = test_state();
    state.push_input("ad");
    state.set_input_cursor(1);
    state.spawn_test_card_report_task("pending".to_string(), async {
        std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
            .await
    });

    state.push_input("bc");

    assert_eq!(state.input(), "abcd");
    assert_eq!(state.input_cursor(), 3);
    assert_eq!(state.background_task_count(), 1);
    state.cancel_background_tasks();
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
fn draw_moves_whole_word_to_continuation_row() {
    let mut state = test_state();
    state.push_input("hello bananas");

    let rows = rendered_rows(&state, 16, 10);

    assert!(
        rows.iter().any(|row| row.contains("> hello       "))
            && rows.iter().any(|row| row.contains("  bananas     ")),
        "expected the whole word on the continuation row; rendered rows:\n{}",
        rows.join("\n")
    );
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

    // OMP assigns a wrap boundary to the continuation row when later input remains.
    terminal
        .backend_mut()
        .assert_cursor_position(Position::new(3, 8));
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
fn draw_uses_rounded_cards_without_outer_transcript_border() {
    let mut state = test_state();
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::RunStarted {
            workflow_name: "bugfix".to_string(),
            current_step: "plan".to_string(),
            request_topic: None,
        },
    ));

    let rows = rendered_rows(&state, 100, 14);
    let rendered = rows.join("\n");
    let composer_row = rows
        .iter()
        .position(|row| row.contains("Enter submits"))
        .unwrap_or_else(|| panic!("{rendered}"));

    assert!(
        rendered.contains("● Run started · ↳ plan · ▶ 170dc431 · ⎇ bugfix"),
        "{rendered}"
    );
    assert!(!rendered.contains("╭"), "{rendered}");
    assert!(!rendered.contains("╰"), "{rendered}");
    assert!(rows[composer_row].starts_with('┌'), "{rendered}");
    assert!(
        rows[..composer_row]
            .iter()
            .all(|row| !row.starts_with('┌') && !row.starts_with('└')),
        "{rendered}"
    );
    assert!(!rendered.contains("step="), "{rendered}");
    assert!(!rendered.contains("run="), "{rendered}");
    assert!(!rendered.contains("workflow="), "{rendered}");
}

#[test]
fn draw_smoke_covers_workflow_tool_cards_and_resize() {
    let mut state = test_state();
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::RunStarted {
            workflow_name: "bugfix".to_string(),
            current_step: "implement".to_string(),
            request_topic: None,
        },
    ));
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::AgentThought {
            step_id: "implement".to_string(),
            content: "thinking through cards".to_string(),
        },
    ));
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::AgentToolCall {
            step_id: "implement".to_string(),
            tool_call_id: "call_read".to_string(),
            title: "Read artifact://28".to_string(),
            tool_kind: "read".to_string(),
            status: "pending".to_string(),
        },
    ));
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::AgentToolCallUpdate {
            step_id: "implement".to_string(),
            tool_call_id: "call_read".to_string(),
            title: "Read artifact://28".to_string(),
            status: "completed".to_string(),
            content: Some(serde_json::json!({"text":"diff output"})),
        },
    ));
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::AgentResponse {
            step_id: "implement".to_string(),
            content: "implemented cards".to_string(),
        },
    ));
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::StepCompleted {
            step_id: "implement".to_string(),
            action: "agent".to_string(),
            status: Some("implemented".to_string()),
            body: "done".to_string(),
        },
    ));
    state.apply_workflow_event(WorkflowEvent::new(
        "run-170dc431-abc",
        WorkflowEventKind::WaitingForInput {
            step: "review".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: vec!["approve".to_string(), "reject".to_string()],
        },
    ));

    let wide_rows = rendered_rows(&state, 120, 40);
    let wide = wide_rows.join("\n");
    assert!(
        wide.contains("● Run started · ↳ implement · ▶ 170dc431 · ⎇ bugfix"),
        "{wide}"
    );
    assert!(
        wide.contains("● Agent thinking · ↳ implement · ▶ 170dc431"),
        "{wide}"
    );
    assert!(
        wide.contains("✓ • Read artifact://28 · ↳ implement · ▶ 170dc431"),
        "{wide}"
    );
    assert!(wide.contains("├─── Output "), "{wide}");
    assert!(wide.contains("diff output"), "{wide}");
    assert!(
        wide.contains("● Agent response · ↳ implement · ▶ 170dc431"),
        "{wide}"
    );
    assert!(
        wide.contains("✓ Step completed · ↳ implement · ▶ 170dc431"),
        "{wide}"
    );
    assert!(
        wide.contains("◔ Waiting for input · ↳ review · ▶ 170dc431"),
        "{wide}"
    );
    assert!(wide.contains("approve · reject"), "{wide}");
    assert!(!wide.contains("step="), "{wide}");
    assert!(!wide.contains("run="), "{wide}");
    assert!(!wide.contains("workflow="), "{wide}");
    assert!(!wide.contains("call_read"), "{wide}");
    assert!(
        wide_rows
            .iter()
            .any(|row| row.contains("┌ Enter answers active prompt")),
        "{wide}"
    );

    let narrow_rows = rendered_rows(&state, 36, 18);
    let narrow = narrow_rows.join("\n");
    assert!(narrow.contains("╭"), "{narrow}");
    assert!(narrow.contains("╰"), "{narrow}");
    assert!(narrow.contains("◔ Waiting for input"), "{narrow}");
    assert!(
        narrow_rows.iter().all(|row| row.chars().count() <= 36),
        "{narrow}"
    );
}

#[test]
fn production_draw_with_typed_input_does_not_scale_with_full_transcript_history() {
    fn state_with_transcript_entries(entries: usize) -> AppState {
        let mut state = test_state();
        for index in 0..entries {
            let content = if index + 1 == entries {
                format!("transcript entry {index:05} LAG_TAIL_VISIBLE")
            } else {
                format!("transcript entry {index:05} filler text for redraw scaling")
            };
            state.push_card("Notice", [content]);
        }

        assert_eq!(state.event_entries().len(), entries);

        state.push_input("typed input stays responsive");
        state
    }

    fn timed_draw(state: &mut AppState) -> (std::time::Duration, String) {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut layout = AppLayout::new(Rect::default(), state);
        let started = std::time::Instant::now();
        terminal
            .draw(|frame| {
                layout = draw_production_frame(frame, state, layout);
            })
            .unwrap();
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

    let mut short_state = state_with_transcript_entries(64);
    let mut long_state = state_with_transcript_entries(20_000);

    let (short_draw, short_rendered) = timed_draw(&mut short_state);
    let (long_draw, long_rendered) = timed_draw(&mut long_state);
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
    assert!(rendered.contains("Waiting for input"), "{rendered}");
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
fn draw_shows_transcript_scrollbar_without_overwriting_status_or_composer() {
    let mut state = test_state();
    for index in 0..30 {
        state.push_card("Notice", [format!("overflowing transcript entry {index}")]);
    }

    let area = Rect::new(0, 0, 40, 14);
    let layout = AppLayout::new(area, &state);
    let rows = rendered_rows(&state, area.width, area.height);
    let rendered = rows.join("\n");
    let scrollbar_column = usize::from(layout.transcript.right() - 1);

    assert!(
        rows[usize::from(layout.transcript.y)..usize::from(layout.transcript.bottom())]
            .iter()
            .any(|row| row.chars().nth(scrollbar_column) == Some('█')),
        "{rendered}"
    );
    assert!(
        rows[usize::from(layout.status.y)].starts_with('○'),
        "{rendered}"
    );
    assert!(
        rows[usize::from(layout.composer.y)].starts_with('┌'),
        "{rendered}"
    );
    assert!(
        rows.last().is_some_and(|row| row.starts_with('└')),
        "{rendered}"
    );
}

#[tokio::test]
async fn draw_active_run_composer_keeps_allowed_slash_suggestions() {
    let mut state = test_state();
    state.push_input("/");
    state.spawn_test_card_report_task("pending".to_string(), async {
        std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
            .await
    });

    let rows = rendered_rows(&state, 100, 14);
    let rendered = rows.join("\n");

    let title_row = rows
        .iter()
        .find(|row| row.contains("No agent accepting prompts") && row.contains("draft retained"))
        .unwrap_or_else(|| panic!("{rendered}"));
    assert!(title_row.contains("Esc cancels"), "{rendered}");
    assert!(rendered.contains("● · ◷ 1"), "{rendered}");
    assert!(rendered.contains("> /"), "{rendered}");
    assert!(rendered.contains("slash command suggestions"), "{rendered}");
    assert!(rendered.contains("/resume <run-id>"), "{rendered}");
    state.cancel_background_tasks();
}

#[tokio::test]
async fn draw_composer_border_color_tracks_visual_state() {
    let mut state = test_state();

    assert_eq!(
        composer_border_fg_for_title(&state, 100, 14, "Enter submits"),
        style_border_accent().fg.unwrap()
    );

    state.spawn_test_card_report_task("pending".to_string(), async {
        std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
            .await
    });
    assert_eq!(
        composer_border_fg_for_title(&state, 100, 14, "No agent accepting prompts"),
        style_muted().fg.unwrap()
    );

    state.apply_workflow_event(WorkflowEvent::new(
        "run-1",
        WorkflowEventKind::WaitingForInput {
            step: "confirm_result".to_string(),
            prompt_id: "approval".to_string(),
            message: "Approve?".to_string(),
            choices: Vec::new(),
        },
    ));
    assert_eq!(
        composer_border_fg_for_title(&state, 100, 14, "Enter answers active prompt"),
        style_warning().fg.unwrap()
    );

    state.cancel_background_tasks();
}

#[tokio::test]
async fn prompt_answer_submission_clears_prompt_and_locks_composer_while_answer_runs() {
    let dir = tempfile::tempdir().unwrap();
    let workflow_dir = dir.path().join("workflows");
    std::fs::create_dir(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("ask.lua"),
        r#"
        local confirm = step("confirm")
        confirm.run = function(ctx)
          return action.ask_user {
            id = "approval",
            message = "Approve?",
            choices = { "yes", "no" },
          }
        end

        local done = step("done")
        done.run = function(ctx)
          local fields = (ctx.prev and ctx.prev.fields) or {}
          return action.status { status = "success", body = "answer=" .. tostring(fields.answer) }
        end

        confirm:on("answered", done)
        return workflow("ask", confirm)
        "#,
    )
    .unwrap();
    let config = AppConfig {
        state_dir: dir.path().join("state"),
        workflow_store: dir.path().join("state/workflow.redb"),
        workflow_dirs: vec![workflow_dir],
        config_sets: std::collections::BTreeMap::from([(
            "default".to_string(),
            crate::config::ConfigSetConfig {
                max_steps_per_run: 5,
                max_visits_per_step: 5,
                ..Default::default()
            },
        )]),
        ..AppConfig::default()
    };
    let runtime = WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
        .with_deterministic_selector();
    let start = runtime
        .start_run_with_workflow("ask", "needs approval")
        .await
        .unwrap();
    let run_id = start.run.id.clone();
    assert!(start.events.iter().any(|event| matches!(
        &event.kind,
        WorkflowEventKind::WaitingForInput { prompt_id, .. } if prompt_id == "approval"
    )));
    let mut state = AppState::new(config);
    state.spawn_test_card_report_task("seed waiting run".to_string(), async move { Ok(start) });
    tokio::task::yield_now().await;
    assert!(state.drain_background_tasks().await);
    assert_eq!(
        state.pending_prompt_answer_target(),
        Some((run_id.clone(), "approval".to_string()))
    );
    assert!(state.composer_accepts_edits());
    assert!(state.composer_accepts_submit());

    state.push_input("yes");
    commands::submit_input(&mut state, &runtime).await;

    assert_eq!(state.input(), "");
    assert!(state.pending_prompt().is_none());
    assert_eq!(
        state.status(),
        format!("submitted answer: {run_id} approval")
    );
    assert_eq!(state.background_task_count(), 1);
    assert!(state.composer_accepts_edits());
    assert!(!state.composer_accepts_submit());

    tokio::task::yield_now().await;
    assert!(state.drain_background_tasks().await);
    assert_eq!(state.background_task_count(), 0);
    assert_eq!(state.display_state(), "completed");
    assert!(state.composer_accepts_submit());
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
