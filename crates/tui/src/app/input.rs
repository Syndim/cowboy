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
        KeyCode::Char('q') | KeyCode::Esc => KeyHandling::Exit,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.cancel_background_tasks();
            KeyHandling::Continue
        }
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
        KeyCode::Enter => KeyHandling::Submit,
        KeyCode::Tab => {
            commands::complete_slash_suggestion(state);
            KeyHandling::Continue
        }
        KeyCode::Backspace => {
            state.pop_input_char();
            KeyHandling::Continue
        }
        KeyCode::Up => {
            state.history_previous();
            KeyHandling::Continue
        }
        KeyCode::Down => {
            state.history_next();
            KeyHandling::Continue
        }
        KeyCode::PageUp => {
            state.scroll_events_up();
            KeyHandling::Continue
        }
        KeyCode::PageDown => {
            state.scroll_events_down();
            KeyHandling::Continue
        }
        KeyCode::End => {
            state.follow_latest();
            KeyHandling::Continue
        }
        KeyCode::Char(ch) => {
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
            max_steps_per_run: 1,
            max_visits_per_step: 1,
            ..AppConfig::default()
        })
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
}
