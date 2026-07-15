use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::commands;
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
        KeyCode::Up if state.composer_accepts_submit() => {
            state.history_previous();
            KeyHandling::Continue
        }
        KeyCode::Up => KeyHandling::Continue,
        KeyCode::Down if state.composer_accepts_submit() => {
            state.history_next();
            KeyHandling::Continue
        }
        KeyCode::Down => KeyHandling::Continue,
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
            state.push_input(&ch.to_string());
            KeyHandling::Continue
        }
        _ => KeyHandling::Continue,
    }
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
        assert!(!state.composer_accepts_submit());
        assert!(state.pending_prompt().is_none());
    }

    fn seed_history(state: &mut AppState) {
        state.push_input("from history");
        assert_eq!(
            state.take_submitted_input(),
            Some("from history".to_string())
        );
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
    async fn active_run_blocks_tab_completion_and_history_navigation() {
        let mut tab_state = test_state();
        tab_state.push_input("/ru");
        lock_composer_with_pending_task(&mut tab_state);

        let handling = handle_key_press(
            &mut tab_state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(tab_state.input(), "/ru");
        assert_eq!(tab_state.input_cursor(), "/ru".chars().count());
        assert!(!tab_state.composer_accepts_submit());
        tab_state.cancel_background_tasks();

        let mut history_state = test_state();
        seed_history(&mut history_state);
        history_state.history_previous();
        assert_eq!(history_state.input(), "from history");
        let cursor = history_state.input_cursor();
        lock_composer_with_pending_task(&mut history_state);

        let handling = handle_key_press(
            &mut history_state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        assert_eq!(handling, KeyHandling::Continue);
        assert_eq!(history_state.input(), "from history");
        assert_eq!(history_state.input_cursor(), cursor);
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
}
