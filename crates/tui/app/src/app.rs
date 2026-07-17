use std::io;
use std::time::Duration;

use anyhow::Result;
use cowboy_workflow_engine::{WorkflowEvent, WorkflowRuntime};
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
};

#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::config::AppConfig;

mod card;
mod commands;
mod controls;
mod events;
mod history;
mod input;
mod markup;
mod state;
mod styles;

use controls::{composer, header, status, transcript};
use input::KeyHandling;
use state::AppState;

/// Start the new workflow-first terminal shell.
pub async fn run_tui(config: AppConfig) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let runtime = WorkflowRuntime::new(config.runtime_config(cwd));
    let events = runtime.events();
    let mut workflow_events = events.subscribe();
    let state = AppState::new(config);
    let mut terminal_mode = TerminalModeGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    tracing::debug!("TUI terminal session started");

    let result = run_loop(&mut terminal, state, &runtime, &mut workflow_events).await;
    if let Err(err) = &result {
        tracing::error!(error = ?err, "TUI loop exited with error");
    }

    terminal_mode.restore()?;
    tracing::debug!("TUI terminal session restored");

    result
}

fn tui_input_cursor_style() -> SetCursorStyle {
    SetCursorStyle::BlinkingBlock
}

struct TerminalModeGuard {
    restored: bool,
    keyboard_enhancement_active: bool,
}

impl TerminalModeGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        if let Err(err) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            tui_input_cursor_style()
        ) {
            let _ = disable_raw_mode();
            return Err(err.into());
        }

        let keyboard_enhancement_active = match push_keyboard_enhancement_flags(&mut stdout) {
            Ok(active) => active,
            Err(err) => {
                let _ = disable_raw_mode();
                return Err(err);
            }
        };

        Ok(Self {
            restored: false,
            keyboard_enhancement_active,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }

        disable_raw_mode()?;
        let mut stdout = io::stdout();
        pop_keyboard_enhancement_flags(&mut stdout, self.keyboard_enhancement_active)?;
        self.keyboard_enhancement_active = false;
        execute!(
            stdout,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            SetCursorStyle::DefaultUserShape,
            Show
        )?;
        self.restored = true;
        Ok(())
    }
}

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags(stdout: &mut io::Stdout) -> Result<bool> {
    #[cfg(not(windows))]
    execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    Ok(true)
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags(_stdout: &mut io::Stdout) -> Result<bool> {
    Ok(false)
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags(stdout: &mut io::Stdout, active: bool) -> Result<()> {
    if !active {
        return Ok(());
    }

    #[cfg(not(windows))]
    execute!(stdout, PopKeyboardEnhancementFlags)?;

    Ok(())
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags(_stdout: &mut io::Stdout, _active: bool) -> Result<()> {
    Ok(())
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        if self.restored {
            return;
        }

        if let Err(err) = self.restore() {
            tracing::error!(error = ?err, "failed to restore terminal after TUI exit");
        }
    }
}

#[derive(Debug)]
struct DrawScheduler {
    needs_draw: bool,
}

impl DrawScheduler {
    fn new() -> Self {
        Self { needs_draw: true }
    }

    fn should_draw(&self) -> bool {
        self.needs_draw
    }

    fn mark_dirty(&mut self) {
        self.needs_draw = true;
    }

    fn mark_dirty_if(&mut self, changed: bool) {
        if changed {
            self.mark_dirty();
        }
    }

    fn mark_clean(&mut self) {
        self.needs_draw = false;
    }
}

async fn run_loop<B>(
    terminal: &mut Terminal<B>,
    mut state: AppState,
    runtime: &WorkflowRuntime,
    workflow_events: &mut tokio::sync::broadcast::Receiver<WorkflowEvent>,
) -> Result<()>
where
    B: ratatui::backend::Backend,
    B::Error: Send + Sync + 'static,
{
    let mut draw_scheduler = DrawScheduler::new();
    loop {
        draw_scheduler.mark_dirty_if(state.drain_workflow_events(workflow_events));
        draw_scheduler.mark_dirty_if(state.drain_background_tasks().await);
        if state.exit_requested() {
            return Ok(());
        }
        if draw_scheduler.should_draw() {
            if let Err(err) = terminal.draw(|frame| draw(frame, &state)) {
                tracing::error!(error = ?err, "TUI draw failed");
                return Err(err.into());
            }
            draw_scheduler.mark_clean();
        }

        let has_event = match event::poll(Duration::from_millis(100)) {
            Ok(has_event) => has_event,
            Err(err) => {
                tracing::error!(error = ?err, "TUI event poll failed");
                return Err(err.into());
            }
        };
        if !has_event {
            continue;
        }

        let event = match event::read() {
            Ok(event) => event,
            Err(err) => {
                tracing::error!(error = ?err, "TUI event read failed");
                return Err(err.into());
            }
        };
        match event {
            Event::Paste(text) => {
                tracing::debug!(chars = text.chars().count(), "TUI paste received");
                state.push_input(&text);
                draw_scheduler.mark_dirty();
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                tracing::trace!(
                    code = key_code_name(&key.code),
                    modifiers = ?key.modifiers,
                    input_chars = state.input().chars().count(),
                    "TUI key received"
                );
                match input::handle_key_press(&mut state, key) {
                    KeyHandling::Continue => draw_scheduler.mark_dirty(),
                    KeyHandling::Submit => {
                        commands::submit_input(&mut state, runtime).await;
                        draw_scheduler.mark_dirty();
                    }
                    KeyHandling::Exit => return Ok(()),
                }
            }
            Event::Resize(_, _) => draw_scheduler.mark_dirty(),
            event => {
                tracing::trace!(event = ?event, "TUI event ignored");
            }
        }
    }
}

fn key_code_name(code: &KeyCode) -> &'static str {
    match code {
        KeyCode::Backspace => "backspace",
        KeyCode::Enter => "enter",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "page_up",
        KeyCode::PageDown => "page_down",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "back_tab",
        KeyCode::Delete => "delete",
        KeyCode::Insert => "insert",
        KeyCode::F(_) => "function",
        KeyCode::Char(_) => "char",
        KeyCode::Null => "null",
        KeyCode::Esc => "escape",
        KeyCode::CapsLock => "caps_lock",
        KeyCode::ScrollLock => "scroll_lock",
        KeyCode::NumLock => "num_lock",
        KeyCode::PrintScreen => "print_screen",
        KeyCode::Pause => "pause",
        KeyCode::Menu => "menu",
        KeyCode::KeypadBegin => "keypad_begin",
        KeyCode::Media(_) => "media",
        KeyCode::Modifier(_) => "modifier",
    }
}
fn draw(frame: &mut ratatui::Frame<'_>, state: &AppState) {
    let area = frame.area();
    let composer_height = composer::height(state, area.height, area.width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(composer_height),
        ])
        .split(area);

    header::render(frame, chunks[0], state);
    transcript::render(frame, chunks[1], state);
    status::render(frame, chunks[2], state);
    composer::render(frame, chunks[3], state);
}

#[cfg(test)]
mod tests;
