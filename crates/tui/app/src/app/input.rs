use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use super::AppLayout;
use super::commands;
use super::controls::composer;
use super::controls::transcript;
use super::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyHandling {
    Continue,
    Submit,
    Exit,
}

pub(super) fn handle_key_press(state: &mut AppState, key: KeyEvent) -> KeyHandling {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyHandling::Exit,
        KeyCode::Esc => {
            state.cancel_background_tasks();
            KeyHandling::Continue
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_events_up();
            KeyHandling::Continue
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_events_down();
            KeyHandling::Continue
        }
        KeyCode::End => {
            state.follow_latest();
            KeyHandling::Continue
        }
        _ if !state.composer_accepts_edits() => KeyHandling::Continue,
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) =>
        {
            state.push_input("\n");
            KeyHandling::Continue
        }
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.push_input("\n");
            KeyHandling::Continue
        }
        KeyCode::Enter if state.composer_accepts_submit() => KeyHandling::Submit,
        KeyCode::Enter => KeyHandling::Continue,
        KeyCode::Tab if state.composer_accepts_submit() => {
            commands::complete_slash_suggestion(state);
            KeyHandling::Continue
        }
        KeyCode::Tab => KeyHandling::Continue,
        KeyCode::Up => {
            let allow_history = state.composer_accepts_submit();
            composer::move_input_up(state, allow_history);
            KeyHandling::Continue
        }
        KeyCode::Down => {
            let allow_history = state.composer_accepts_submit();
            composer::move_input_down(state, allow_history);
            KeyHandling::Continue
        }
        KeyCode::PageUp => {
            let allow_history = state.composer_accepts_submit();
            composer::move_input_page_up(state, allow_history);
            KeyHandling::Continue
        }
        KeyCode::PageDown => {
            let allow_history = state.composer_accepts_submit();
            composer::move_input_page_down(state, allow_history);
            KeyHandling::Continue
        }
        KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.move_input_cursor_prev_word();
            KeyHandling::Continue
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.move_input_cursor_next_word();
            KeyHandling::Continue
        }
        KeyCode::Left => {
            state.move_input_cursor_left();
            KeyHandling::Continue
        }
        KeyCode::Right => {
            state.move_input_cursor_right();
            KeyHandling::Continue
        }
        KeyCode::Backspace => {
            state.pop_input_char();
            KeyHandling::Continue
        }
        KeyCode::Delete => {
            state.delete_input_char();
            KeyHandling::Continue
        }
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state.push_typed_char(ch);
            KeyHandling::Continue
        }
        _ => KeyHandling::Continue,
    }
}

pub(super) fn handle_key_press_with_layout(
    state: &mut AppState,
    key: KeyEvent,
    layout: AppLayout,
) -> KeyHandling {
    let transcript_is_collapsed = layout.transcript.width == 0 || layout.transcript.height == 0;
    let is_transcript_scroll = key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('u') | KeyCode::Char('d'));
    if transcript_is_collapsed && is_transcript_scroll {
        return KeyHandling::Continue;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        let limit = match key.code {
            KeyCode::Char('u') => Some(transcript::next_scroll_limit(state, layout.transcript)),
            KeyCode::Char('d') => Some(transcript::current_scroll_limit(state, layout.transcript)),
            _ => None,
        };

        if let Some(limit) = limit {
            state.set_transcript_scroll_limit(limit);
        }
    }

    handle_key_press(state, key)
}

pub(super) fn handle_mouse_event(
    state: &mut AppState,
    event: MouseEvent,
    layout: AppLayout,
) -> bool {
    let position = Position::new(event.column, event.row);

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(point) = transcript::selection_point_at(state, layout.transcript, position)
            else {
                state.clear_transcript_selection();
                return false;
            };

            state.start_transcript_selection(point);
            true
        }
        MouseEventKind::Drag(MouseButton::Left) if state.transcript_selection_is_active() => {
            let Some(point) = transcript::selection_point_at(state, layout.transcript, position)
            else {
                return false;
            };

            state.update_transcript_selection(point);
            let selected_text = transcript::selected_text(state, layout.transcript);
            state.set_transcript_selection_text(selected_text);
            true
        }
        MouseEventKind::Up(MouseButton::Left) if state.transcript_selection_is_active() => {
            if let Some(point) = transcript::selection_point_at(state, layout.transcript, position)
            {
                state.update_transcript_selection(point);
            }

            let selected_text = transcript::selected_text(state, layout.transcript);
            state.finalize_transcript_selection(selected_text);
            true
        }
        MouseEventKind::ScrollUp if layout.transcript.contains(position) => {
            let limit = transcript::next_scroll_limit(state, layout.transcript);
            state.set_transcript_scroll_limit(limit);
            state.scroll_events_up()
        }
        MouseEventKind::ScrollDown if layout.transcript.contains(position) => {
            let limit = transcript::current_scroll_limit(state, layout.transcript);
            state.set_transcript_scroll_limit(limit);
            state.scroll_events_down()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crossterm::event::MouseButton;
    use ratatui::layout::Rect;
    use unicode_width::UnicodeWidthStr;

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

    fn lock_composer_with_pending_task(state: &mut AppState) {
        state.spawn_test_card_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<cowboy_workflow_engine::RunReport, String>>()
                .await
        });
        assert!(state.composer_accepts_edits());
        assert!(state.pending_prompt().is_none());
    }

    fn seed_history(state: &mut AppState) {
        state.push_input("from history");
        assert_eq!(
            state.take_submitted_input(),
            Some("from history".to_string())
        );
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    #[test]
    fn modified_enter_adds_newline_without_submitting() {
        let mut state = test_state();

        for modifiers in [KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
            state.replace_input_from_completion("hello".to_string());

            let handling = handle_key_press(&mut state, KeyEvent::new(KeyCode::Enter, modifiers));

            assert_eq!(handling, KeyHandling::Continue);
            assert_eq!(state.input(), "hello\n");
            assert!(state.history_is_empty());
            assert_eq!(state.background_task_count(), 0);
        }
    }

    #[test]
    fn plain_enter_requests_submit_without_mutating_input() {
        let mut state = test_state();
        state.push_input("hello");

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Submit);
        assert_eq!(state.input(), "hello");
    }

    #[test]
    fn control_c_requests_exit_without_mutating_input() {
        let mut state = test_state();
        state.push_input("hello");

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );

        assert_eq!(handling, KeyHandling::Exit);
        assert_eq!(state.input(), "hello");
    }

    #[tokio::test]
    async fn control_c_exits_even_when_composer_is_blocked() {
        let mut state = test_state();
        state.push_input("hello");
        lock_composer_with_pending_task(&mut state);
        assert!(!state.composer_accepts_submit());

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );

        assert_eq!(handling, KeyHandling::Exit);
        assert_eq!(state.input(), "hello");
        assert_eq!(state.background_task_count(), 1);
        assert!(!state.composer_accepts_submit());

        state.cancel_background_tasks();
    }

    #[test]
    fn plain_q_appends_to_composer() {
        let mut state = test_state();

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "q");
    }

    #[test]
    fn left_and_right_move_cursor_without_mutating_input_and_clamp() {
        let mut state = test_state();
        state.push_input("abc");

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(state.input(), "abc");
        assert_eq!(state.input_cursor(), 1);

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        );
        assert_eq!(state.input(), "abc");
        assert_eq!(state.input_cursor(), 2);

        for _ in 0..4 {
            handle_key_press(
                &mut state,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            );
        }
        assert_eq!(state.input_cursor(), 3);

        for _ in 0..4 {
            handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        assert_eq!(state.input_cursor(), 0);
    }

    #[test]
    fn typed_characters_insert_at_cursor() {
        let mut state = test_state();
        state.push_input("abcd");
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "abXcd");
        assert_eq!(state.input_cursor(), 3);
    }

    #[test]
    fn control_left_and_right_jump_by_words() {
        let mut state = test_state();
        state.push_input("alpha beta gamma");

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
        );
        assert_eq!(state.input_cursor(), "alpha beta ".chars().count());

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
        );
        assert_eq!(state.input_cursor(), "alpha ".chars().count());

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert_eq!(state.input_cursor(), "alpha beta ".chars().count());
    }

    #[test]
    fn backspace_and_delete_edit_at_cursor() {
        let mut state = test_state();
        state.push_input("abcd");
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(state.input(), "acd");
        assert_eq!(state.input_cursor(), 1);

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        );
        assert_eq!(state.input(), "ad");
        assert_eq!(state.input_cursor(), 1);
    }

    #[test]
    fn paste_and_newline_insert_at_cursor() {
        let mut state = test_state();
        state.push_input("abcd");
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        state.push_input("XY");
        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        );

        assert_eq!(state.input(), "abXY\ncd");
        assert_eq!(state.input_cursor(), "abXY\n".chars().count());
    }

    #[test]
    fn unicode_movement_and_deletion_stay_on_character_boundaries() {
        let mut state = test_state();
        state.push_input("a中éb");
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(state.input(), "aéb");
        assert_eq!(state.input_cursor(), 1);

        handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        );
        assert_eq!(state.input(), "ab");
        assert_eq!(state.input_cursor(), 1);
    }

    #[tokio::test]
    async fn esc_cancels_background_tasks_without_mutating_input() {
        let mut state = test_state();
        state.push_input("hello");
        lock_composer_with_pending_task(&mut state);
        assert!(!state.composer_accepts_submit());

        let handling =
            handle_key_press(&mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "hello");
        assert_eq!(state.status(), "cancelled 1 background task(s)");
        assert_eq!(state.background_task_count(), 0);
        assert!(state.composer_accepts_submit());
    }

    #[test]
    fn control_j_still_adds_newline() {
        let mut state = test_state();
        state.push_input("hello");

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "hello\n");
    }

    #[test]
    fn tab_completes_first_visible_slash_suggestion() {
        let mut state = test_state();
        state.push_input("/ru");

        let handling =
            handle_key_press(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "/run ");
    }

    #[test]
    fn tab_completion_omits_space_for_commands_without_arguments() {
        let mut state = test_state();
        state.push_input("/runs");

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(state.input(), "/runs");
    }

    #[test]
    fn non_slash_tab_is_inert() {
        let mut state = test_state();
        state.push_input("plain request");

        let handling =
            handle_key_press(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "plain request");
    }

    fn populate_scrollable_transcript(state: &mut AppState) {
        state.push_card("Transcript", (0..20).map(|index| format!("line {index}")));
    }

    fn visible_transcript_rows(state: &AppState, layout: AppLayout) -> Vec<String> {
        transcript::lines(
            state,
            layout.transcript.height as usize,
            layout.transcript.width as usize,
        )
        .into_iter()
        .map(|line| line.to_string())
        .collect()
    }

    #[tokio::test]
    async fn active_run_allows_draft_edits_but_plain_enter_does_not_submit() {
        let mut state = test_state();
        state.push_input("alpha beta");
        state.set_input_cursor("alpha ".chars().count());
        lock_composer_with_pending_task(&mut state);

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "alpha Xbeta");
        assert_eq!(state.input_cursor(), "alpha X".chars().count());

        let handling =
            handle_key_press(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input_cursor(), "alpha ".chars().count());

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "alphaXbeta");
        assert_eq!(state.input_cursor(), "alpha".chars().count());

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "alphabeta");
        assert_eq!(state.input_cursor(), "alpha".chars().count());

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input_cursor(), "alphabeta".chars().count());

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "alphabeta\n");

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), "alphabeta\n\n");

        let input = state.input().to_string();
        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.input(), input);
        assert!(state.history_is_empty());
        assert_eq!(state.background_task_count(), 1);
        assert!(state.composer_accepts_edits());
        assert!(!state.composer_accepts_submit());
        state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn active_run_allows_slash_completion_but_blocks_plain_history_navigation() {
        let mut tab_state = test_state();
        tab_state.push_input("/ru");
        lock_composer_with_pending_task(&mut tab_state);

        let handling = handle_key_press(
            &mut tab_state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(tab_state.input(), "/run ");
        assert!(tab_state.composer_accepts_submit());
        tab_state.cancel_background_tasks();

        let mut history_state = test_state();
        let recalled = "first\nsecond";
        history_state.push_input(recalled);
        assert_eq!(
            history_state.take_submitted_input(),
            Some(recalled.to_string())
        );
        history_state.history_previous();
        assert_eq!(history_state.input(), recalled);
        assert_eq!(history_state.input_cursor(), 0);
        lock_composer_with_pending_task(&mut history_state);

        let handling = handle_key_press(
            &mut history_state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(history_state.input(), recalled);
        assert_eq!(history_state.input_cursor(), "first\n".chars().count());

        handle_key_press(
            &mut history_state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        assert_eq!(history_state.input(), recalled);
        assert_eq!(history_state.input_cursor(), recalled.chars().count());
        assert!(!history_state.composer_accepts_submit());
        history_state.cancel_background_tasks();
    }

    #[tokio::test]
    async fn active_run_allows_scroll_follow_latest_and_exit_keys() {
        let mut state = test_state();
        populate_scrollable_transcript(&mut state);
        state.push_input("draft");
        lock_composer_with_pending_task(&mut state);

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert!(state.scroll_offset() > 0);
        assert_eq!(state.input(), "draft");

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.scroll_offset(), 0);
        assert!(state.is_following_events());
        assert_eq!(state.input(), "draft");

        state.scroll_events_up();
        assert!(state.scroll_offset() > 0);
        let handling =
            handle_key_press(&mut state, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.scroll_offset(), 0);
        assert!(state.is_following_events());
        assert_eq!(state.input(), "draft");

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert_eq!(handling, KeyHandling::Exit);
        assert_eq!(state.input(), "draft");
        assert_eq!(state.background_task_count(), 1);
        assert!(!state.composer_accepts_submit());
        state.cancel_background_tasks();
    }

    #[test]
    fn control_u_scrolls_up_without_mutating_input() {
        let mut state = test_state();
        populate_scrollable_transcript(&mut state);
        state.push_input("draft");

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert!(state.scroll_offset() > 0);
        assert_eq!(state.input(), "draft");
    }

    #[test]
    fn control_d_scrolls_down_and_restores_follow_latest_without_mutating_input() {
        let mut state = test_state();
        populate_scrollable_transcript(&mut state);
        state.scroll_events_up();
        state.push_input("draft");
        assert!(state.scroll_offset() > 0);
        assert!(!state.is_following_events());

        let handling = handle_key_press(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(state.scroll_offset(), 0);
        assert!(state.is_following_events());
        assert_eq!(state.input(), "draft");
    }

    #[test]
    fn page_keys_do_not_scroll_transcript() {
        let mut state = test_state();
        populate_scrollable_transcript(&mut state);
        state.scroll_events_up();
        state.push_input("draft");
        let offset = state.scroll_offset();
        assert!(offset > 0);

        for code in [KeyCode::PageUp, KeyCode::PageDown] {
            let handling = handle_key_press(&mut state, KeyEvent::new(code, KeyModifiers::NONE));

            assert_eq!(handling, KeyHandling::Continue);
            assert_eq!(state.scroll_offset(), offset);
            assert_eq!(state.input(), "draft");
        }
    }

    #[test]
    fn composer_mouse_wheel_does_not_switch_input_history() {
        let mut observed_inputs = Vec::new();
        let mut expected_inputs = Vec::new();

        for (kind, history_steps, expected) in [
            (crossterm::event::MouseEventKind::ScrollUp, 1, "newer input"),
            (
                crossterm::event::MouseEventKind::ScrollDown,
                2,
                "older input",
            ),
        ] {
            let mut state = test_state();
            for entry in ["older input", "newer input"] {
                state.push_input(entry);
                assert_eq!(state.take_submitted_input(), Some(entry.to_string()));
            }

            for _ in 0..history_steps {
                handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            }

            assert_eq!(state.input(), expected);
            let layout =
                crate::app::AppLayout::new(ratatui::layout::Rect::new(0, 0, 80, 20), &state);
            handle_mouse_event(
                &mut state,
                crossterm::event::MouseEvent {
                    kind,
                    column: layout.composer.x,
                    row: layout.composer.y,
                    modifiers: KeyModifiers::NONE,
                },
                layout,
            );
            observed_inputs.push(state.input().to_string());
            expected_inputs.push(expected.to_string());
        }

        assert_eq!(
            observed_inputs, expected_inputs,
            "composer input changed after mouse scrolling"
        );
    }

    #[test]
    fn up_restores_persisted_history_from_fresh_state() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let mut first_state = AppState::new(config.clone());
        first_state.push_input("persist me");
        assert_eq!(
            first_state.take_submitted_input(),
            Some("persist me".to_string())
        );

        let mut second_state = AppState::new(config);
        let handling = handle_key_press(
            &mut second_state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(second_state.input(), "persist me");
    }

    #[test]
    fn down_after_restored_history_clears_composer_after_newest_entry() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/workflow.redb"),
            config_sets: std::collections::BTreeMap::from([(
                "default".to_string(),
                crate::config::ConfigSetConfig {
                    max_steps_per_run: 1,
                    max_visits_per_step: 1,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };
        let mut first_state = AppState::new(config.clone());
        first_state.push_input("persist me");
        assert_eq!(
            first_state.take_submitted_input(),
            Some("persist me".to_string())
        );
        let mut second_state = AppState::new(config);
        handle_key_press(
            &mut second_state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        let handling = handle_key_press(
            &mut second_state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(second_state.input(), "");
    }

    #[test]
    fn history_recall_insert_paths_match_omp_anchors() {
        fn recalled_state() -> AppState {
            let mut state = test_state();
            state.push_input("line1\nline2");
            assert_eq!(
                state.take_submitted_input(),
                Some("line1\nline2".to_string())
            );
            handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            assert_eq!(state.input_cursor(), 0);
            state
        }

        let mut typed = recalled_state();
        handle_key_press(
            &mut typed,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(typed.input(), "line1\nline2x");

        let mut newline = recalled_state();
        handle_key_press(
            &mut newline,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        );
        assert_eq!(newline.input(), "\nline1\nline2");

        let mut pasted = recalled_state();
        pasted.push_input("[paste]\n");
        assert_eq!(pasted.input(), "[paste]\nline1\nline2");
    }

    #[test]
    fn history_entries_change_only_at_visual_boundaries() {
        let mut state = test_state();
        for entry in ["older one\nolder two", "newer one\nnewer two"] {
            state.push_input(entry);
            assert_eq!(state.take_submitted_input(), Some(entry.to_string()));
        }

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.input(), "newer one\nnewer two");
        assert_eq!(state.input_cursor(), 0);

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.input(), "newer one\nnewer two");
        assert_eq!(state.input_cursor(), "newer one\n".chars().count());

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.input(), "older one\nolder two");
        assert_eq!(state.input_cursor(), 0);

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.input(), "older one\nolder two");
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.input(), "newer one\nnewer two");
        assert_eq!(state.input_cursor(), "newer one\nnewer two".chars().count());
    }

    #[test]
    fn up_on_oldest_history_entry_resets_cursor_to_start() {
        let mut state = test_state();
        for entry in ["oldest entry", "newest entry"] {
            state.push_input(entry);
            assert_eq!(state.take_submitted_input(), Some(entry.to_string()));
        }

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.input(), "oldest entry");
        assert_eq!(state.input_cursor(), 0);

        for _ in 0..3 {
            handle_key_press(
                &mut state,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            );
        }
        assert_eq!(state.input_cursor(), 3);

        handle_key_press(&mut state, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        assert_eq!(state.input(), "oldest entry");
        assert_eq!(state.input_cursor(), 0);
    }

    #[test]
    fn transcript_mouse_drag_is_handled_for_text_selection() {
        let mut state = test_state();
        state.push_card("Transcript", ["selectable transcript text".to_string()]);
        let layout = AppLayout::new(Rect::new(0, 0, 80, 20), &state);
        let row = layout.transcript.y.saturating_add(1);

        for (kind, column) in [
            (
                MouseEventKind::Down(MouseButton::Left),
                layout.transcript.x.saturating_add(1),
            ),
            (
                MouseEventKind::Drag(MouseButton::Left),
                layout.transcript.x.saturating_add(10),
            ),
            (
                MouseEventKind::Up(MouseButton::Left),
                layout.transcript.x.saturating_add(10),
            ),
        ] {
            assert!(
                handle_mouse_event(&mut state, mouse(kind, column, row), layout),
                "{kind:?} over transcript was ignored, so captured mouse input cannot select text"
            );
        }
    }

    #[test]
    fn transcript_mouse_selection_updates_finalizes_and_queues_copy() {
        let mut state = test_state();
        state.push_card("Transcript", ["selectable transcript text".to_string()]);
        let layout = AppLayout::new(Rect::new(0, 0, 80, 20), &state);
        let rows = visible_transcript_rows(&state, layout);
        let row_index = rows
            .iter()
            .position(|row| row.contains("selectable transcript text"))
            .unwrap();
        let line = &rows[row_index];
        let start_column = UnicodeWidthStr::width(&line[..line.find("selectable").unwrap()]) as u16;
        let end_column = start_column + "selectable".chars().count() as u16;
        let row = layout.transcript.y + row_index as u16;

        assert!(handle_mouse_event(
            &mut state,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                layout.transcript.x + start_column,
                row,
            ),
            layout,
        ));
        assert!(state.transcript_selection_is_active());

        assert!(handle_mouse_event(
            &mut state,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                layout.transcript.x + end_column,
                row,
            ),
            layout,
        ));
        assert_eq!(
            state.transcript_selection().unwrap().selected_text,
            "selectable"
        );

        assert!(handle_mouse_event(
            &mut state,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                layout.transcript.x + end_column,
                row,
            ),
            layout,
        ));
        let selection = state.transcript_selection().unwrap();
        assert!(!selection.active);
        assert_eq!(selection.selected_text, "selectable");
        assert_eq!(
            state.take_pending_clipboard_text(),
            Some("selectable".to_string())
        );
    }

    #[test]
    fn non_transcript_left_button_events_do_not_edit_composer_state() {
        let mut state = test_state();
        seed_history(&mut state);
        state.push_input("draft");
        let layout = AppLayout::new(Rect::new(0, 0, 80, 20), &state);
        let input = state.input().to_string();

        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            assert!(!handle_mouse_event(
                &mut state,
                mouse(kind, layout.composer.x, layout.composer.y),
                layout,
            ));
        }

        assert_eq!(state.input(), input);
        assert_eq!(state.scroll_offset(), 0);
        assert!(state.is_following_events());
        assert!(state.transcript_selection().is_none());
    }

    #[test]
    fn transcript_wheel_changes_only_transcript_scroll_state() {
        let mut state = test_state();
        populate_scrollable_transcript(&mut state);
        state.push_input("draft");
        let layout = AppLayout::new(Rect::new(0, 0, 80, 20), &state);
        let point = (layout.transcript.x, layout.transcript.y);

        assert!(handle_mouse_event(
            &mut state,
            mouse(MouseEventKind::ScrollUp, point.0, point.1),
            layout,
        ));
        assert!((1..=10).contains(&state.scroll_offset()));
        assert!(!state.is_following_events());
        assert_eq!(state.input(), "draft");

        assert!(handle_mouse_event(
            &mut state,
            mouse(MouseEventKind::ScrollDown, point.0, point.1),
            layout,
        ));
        assert_eq!(state.scroll_offset(), 0);
        assert!(state.is_following_events());
        assert_eq!(state.input(), "draft");
    }

    #[test]
    fn transcript_wheel_does_not_scroll_empty_or_short_content() {
        let area = Rect::new(0, 0, 80, 20);

        for mut state in [test_state(), {
            let mut state = test_state();
            state.push_card("Notice", ["short".to_string()]);
            state
        }] {
            let layout = AppLayout::new(area, &state);
            let event = mouse(
                MouseEventKind::ScrollUp,
                layout.transcript.x,
                layout.transcript.y,
            );

            assert!(!handle_mouse_event(&mut state, event, layout));
            assert!(!handle_mouse_event(&mut state, event, layout));
            assert_eq!(state.scroll_offset(), 0);
            assert!(state.is_following_events());
        }
    }

    #[test]
    fn transcript_wheel_stops_at_oldest_reachable_row() {
        let mut state = test_state();
        populate_scrollable_transcript(&mut state);
        let layout = AppLayout::new(Rect::new(0, 0, 80, 20), &state);
        let scroll_up = mouse(
            MouseEventKind::ScrollUp,
            layout.transcript.x,
            layout.transcript.y,
        );

        let mut handled = 0;
        while handle_mouse_event(&mut state, scroll_up, layout) {
            handled += 1;
            assert!(handled < 20, "scrolling never reached the oldest row");
        }

        let oldest_offset = state.scroll_offset();
        assert!(oldest_offset > 0);
        assert!(!handle_mouse_event(&mut state, scroll_up, layout));
        assert_eq!(state.scroll_offset(), oldest_offset);

        assert!(handle_mouse_event(
            &mut state,
            mouse(
                MouseEventKind::ScrollDown,
                layout.transcript.x,
                layout.transcript.y,
            ),
            layout,
        ));
        assert!(state.scroll_offset() < oldest_offset);
    }

    #[test]
    fn keyboard_scroll_preserves_position_in_zero_dimension_layouts() {
        for collapsed_area in [Rect::new(0, 0, 80, 5), Rect::new(0, 0, 0, 20)] {
            let mut state = test_state();
            state.push_card("Transcript", (0..40).map(|index| format!("line {index}")));
            let usable_layout = AppLayout::new(Rect::new(0, 0, 80, 20), &state);
            let handling = handle_key_press_with_layout(
                &mut state,
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                usable_layout,
            );
            assert_eq!(handling, KeyHandling::Continue);
            let saved_offset = state.scroll_offset();
            let saved_rows = visible_transcript_rows(&state, usable_layout);
            assert!(saved_offset > 0);
            assert!(!state.is_following_events());

            let collapsed_layout = AppLayout::new(collapsed_area, &state);
            assert!(
                collapsed_layout.transcript.width == 0 || collapsed_layout.transcript.height == 0
            );
            for code in [KeyCode::Char('u'), KeyCode::Char('d')] {
                let handling = handle_key_press_with_layout(
                    &mut state,
                    KeyEvent::new(code, KeyModifiers::CONTROL),
                    collapsed_layout,
                );

                assert_eq!(handling, KeyHandling::Continue);
                assert_eq!(state.scroll_offset(), saved_offset);
                assert!(!state.is_following_events());
            }

            assert_eq!(visible_transcript_rows(&state, usable_layout), saved_rows);
        }
    }

    #[test]
    fn scroll_down_routes_move_immediately_after_viewport_grows() {
        for use_mouse in [true, false] {
            let mut state = test_state();
            state.push_card("Transcript", (0..40).map(|index| format!("line {index}")));
            let short_layout = AppLayout::new(Rect::new(0, 0, 80, 9), &state);
            let scroll_up = mouse(
                MouseEventKind::ScrollUp,
                short_layout.transcript.x,
                short_layout.transcript.y,
            );
            while handle_mouse_event(&mut state, scroll_up, short_layout) {}

            let tall_layout = AppLayout::new(Rect::new(0, 0, 80, 25), &state);
            let resized_limit = transcript::current_scroll_limit(&state, tall_layout.transcript);
            assert!(state.scroll_offset() > resized_limit.saturating_add(10));
            let before = visible_transcript_rows(&state, tall_layout);

            if use_mouse {
                assert!(handle_mouse_event(
                    &mut state,
                    mouse(
                        MouseEventKind::ScrollDown,
                        tall_layout.transcript.x,
                        tall_layout.transcript.y,
                    ),
                    tall_layout,
                ));
            } else {
                let handling = handle_key_press_with_layout(
                    &mut state,
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                    tall_layout,
                );
                assert_eq!(handling, KeyHandling::Continue);
            }

            let after = visible_transcript_rows(&state, tall_layout);
            assert_ne!(after, before, "ScrollDown only drained hidden overscroll");
            assert_eq!(
                state.scroll_offset(),
                resized_limit.saturating_sub(10),
                "stored offset retained hidden overscroll"
            );
        }
    }

    #[test]
    fn non_scrollable_regions_and_unsupported_mouse_events_are_no_ops() {
        let mut state = test_state();
        seed_history(&mut state);
        state.push_input("draft");
        let layout = AppLayout::new(Rect::new(0, 0, 80, 20), &state);

        for (column, row) in [
            (layout.header.x, layout.header.y),
            (layout.status.x, layout.status.y),
            (layout.composer.right(), layout.composer.bottom()),
        ] {
            assert!(!handle_mouse_event(
                &mut state,
                mouse(MouseEventKind::ScrollUp, column, row),
                layout,
            ));
        }

        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            assert!(!handle_mouse_event(
                &mut state,
                mouse(kind, layout.transcript.x, layout.transcript.y),
                layout,
            ));
        }

        assert_eq!(state.input(), "draft");
        assert_eq!(state.scroll_offset(), 0);
        assert!(state.is_following_events());
    }

    #[test]
    fn expanded_composer_boundary_uses_current_shared_layout() {
        let mut state = test_state();
        seed_history(&mut state);
        let area = Rect::new(0, 0, 40, 20);
        let compact = AppLayout::new(area, &state);
        state.push_input("one\ntwo\nthree\nfour");
        let expanded = AppLayout::new(area, &state);

        assert!(expanded.composer.y < compact.composer.y);
        assert_eq!(expanded.transcript.bottom(), expanded.status.y);
        assert_eq!(expanded.status.bottom(), expanded.composer.y);
        let input = state.input().to_string();
        handle_mouse_event(
            &mut state,
            mouse(
                MouseEventKind::ScrollUp,
                expanded.composer.x,
                expanded.composer.y,
            ),
            expanded,
        );
        assert_eq!(state.input(), input);
        assert_eq!(state.scroll_offset(), 0);
    }
}
