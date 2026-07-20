use std::io;
#[cfg(windows)]
use std::io::Write as _;
use std::time::Duration;

use anyhow::Result;
use cowboy_workflow_engine::{WorkflowEvent, WorkflowRuntime};
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind,
};
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};

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
    raw_mode_active: bool,
    screen_active: bool,
    keyboard_enhancement_active: bool,
}

impl TerminalModeGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut guard = Self {
            raw_mode_active: true,
            screen_active: false,
            keyboard_enhancement_active: false,
        };

        let mut stdout = io::stdout();
        guard
            .activate_screen(|| enter_terminal_screen(&mut stdout).map_err(anyhow::Error::from))?;
        guard.activate_keyboard(|| push_keyboard_enhancement_flags(&mut stdout))?;
        Ok(guard)
    }

    fn activate_screen(&mut self, activate: impl FnOnce() -> Result<()>) -> Result<()> {
        self.screen_active = true;
        activate()
    }

    fn activate_keyboard(&mut self, activate: impl FnOnce() -> Result<bool>) -> Result<()> {
        self.keyboard_enhancement_active = true;
        self.keyboard_enhancement_active = activate()?;
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        self.restore_with(
            || disable_raw_mode().map_err(anyhow::Error::from),
            || {
                let mut stdout = io::stdout();
                pop_keyboard_enhancement_flags(&mut stdout, true)
            },
            || {
                let mut stdout = io::stdout();
                restore_terminal_screen(&mut stdout).map_err(anyhow::Error::from)
            },
        )
    }

    fn restore_with(
        &mut self,
        disable_raw: impl FnOnce() -> Result<()>,
        pop_keyboard_enhancement: impl FnOnce() -> Result<()>,
        restore_screen: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let mut first_error = None;

        if self.raw_mode_active {
            match disable_raw() {
                Ok(()) => self.raw_mode_active = false,
                Err(err) => first_error = Some(err),
            }
        }

        if self.keyboard_enhancement_active {
            match pop_keyboard_enhancement() {
                Ok(()) => self.keyboard_enhancement_active = false,
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        if self.screen_active {
            match restore_screen() {
                Ok(()) => self.screen_active = false,
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn is_restored(&self) -> bool {
        !self.raw_mode_active && !self.screen_active && !self.keyboard_enhancement_active
    }
}

fn enter_terminal_screen(stdout: &mut impl io::Write) -> io::Result<()> {
    let result = (|| {
        execute!(stdout, EnterAlternateScreen)?;
        execute!(stdout, EnableBracketedPaste)?;
        execute!(stdout, EnableMouseCapture)?;
        execute!(stdout, tui_input_cursor_style())?;
        Ok(())
    })();

    if result.is_err() {
        let _ = restore_terminal_screen(stdout);
    }

    result
}

fn restore_terminal_screen(stdout: &mut impl io::Write) -> io::Result<()> {
    execute!(
        stdout,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape,
        Show
    )
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
fn push_keyboard_enhancement_flags(stdout: &mut io::Stdout) -> Result<bool> {
    if !crossterm::ansi_support::supports_ansi() {
        return Ok(false);
    }

    let command = PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES);
    write_keyboard_enhancement_ansi(stdout, command)?;
    Ok(true)
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
fn pop_keyboard_enhancement_flags(stdout: &mut io::Stdout, active: bool) -> Result<()> {
    if !active {
        return Ok(());
    }

    let command = PopKeyboardEnhancementFlags;
    write_keyboard_enhancement_ansi(stdout, command)
}

#[cfg(windows)]
fn write_keyboard_enhancement_ansi(
    stdout: &mut io::Stdout,
    command: impl crossterm::Command,
) -> Result<()> {
    let mut sequence = String::new();
    command
        .write_ansi(&mut sequence)
        .map_err(|err| anyhow::anyhow!("failed to build keyboard enhancement sequence: {err}"))?;
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        if self.is_restored() {
            return;
        }

        if let Err(err) = self.restore() {
            tracing::error!(error = ?err, "failed to restore terminal after TUI exit");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AppLayout {
    header: Rect,
    transcript: Rect,
    status: Rect,
    composer: Rect,
}

impl AppLayout {
    fn new(area: Rect, state: &AppState) -> Self {
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

        Self {
            header: chunks[0],
            transcript: chunks[1],
            status: chunks[2],
            composer: chunks[3],
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
    let mut current_layout = AppLayout::new(Rect::default(), &state);
    loop {
        draw_scheduler.mark_dirty_if(state.drain_workflow_events(workflow_events));
        draw_scheduler.mark_dirty_if(state.drain_background_tasks().await);
        if state.exit_requested() {
            return Ok(());
        }
        if draw_scheduler.should_draw() {
            if let Err(err) = terminal.draw(|frame| {
                current_layout = draw_production_frame(frame, &mut state, current_layout);
            }) {
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
                match input::handle_key_press_with_layout(&mut state, key, current_layout) {
                    KeyHandling::Continue => draw_scheduler.mark_dirty(),
                    KeyHandling::Submit => {
                        commands::submit_input(&mut state, runtime).await;
                        draw_scheduler.mark_dirty();
                    }
                    KeyHandling::Exit => return Ok(()),
                }
            }
            Event::Mouse(mouse) => {
                draw_scheduler.mark_dirty_if(input::handle_mouse_event(
                    &mut state,
                    mouse,
                    current_layout,
                ));
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

fn draw_production_frame(
    frame: &mut ratatui::Frame<'_>,
    state: &mut AppState,
    previous_layout: AppLayout,
) -> AppLayout {
    let layout = AppLayout::new(frame.area(), state);
    reconcile_layout_transition(state, previous_layout, layout);
    draw_with_layout(frame, state, layout);
    layout
}

fn reconcile_layout_transition(
    state: &mut AppState,
    previous_layout: AppLayout,
    layout: AppLayout,
) {
    if previous_layout.transcript == layout.transcript
        || layout.transcript.width == 0
        || layout.transcript.height == 0
    {
        return;
    }

    let limit = transcript::current_scroll_limit(state, layout.transcript);
    state.set_transcript_scroll_limit(limit);
}

#[cfg(test)]
fn draw(frame: &mut ratatui::Frame<'_>, state: &AppState) {
    let layout = AppLayout::new(frame.area(), state);
    draw_with_layout(frame, state, layout);
}

fn draw_with_layout(frame: &mut ratatui::Frame<'_>, state: &AppState, layout: AppLayout) {
    header::render(frame, layout.header, state);
    transcript::render(frame, layout.transcript, state);
    status::render(frame, layout.status, state);
    composer::render(frame, layout.composer, state);
}

#[cfg(test)]
mod tests;
