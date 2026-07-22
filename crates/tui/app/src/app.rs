use std::io;
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use cowboy_tui_terminal::TerminalModeGuard;
use cowboy_workflow_engine::{WorkflowEvent, WorkflowRuntime};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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
    runtime.cancel_store_waits();
    if let Err(err) = &result {
        tracing::error!(error = ?err, "TUI loop exited with error");
    }

    finish_tui(result, &mut terminal_mode, &mut io::stdout())?;
    tracing::debug!("TUI terminal session restored");

    Ok(())
}

/// Abstraction over restoring terminal modes on TUI teardown, so the
/// print-after-restore ordering is unit-testable without a real terminal.
trait TerminalRestore {
    fn restore(&mut self) -> Result<()>;
}

impl TerminalRestore for TerminalModeGuard {
    fn restore(&mut self) -> Result<()> {
        TerminalModeGuard::restore(self)
    }
}

/// Write the resume hint line, if any, to `out`. Writes `"{hint}\n"` for
/// `Some` and nothing for `None`.
fn print_resume_hint(out: &mut impl io::Write, hint: Option<&str>) -> io::Result<()> {
    if let Some(hint) = hint {
        writeln!(out, "{hint}")?;
    }

    Ok(())
}

/// Restore the terminal, then print the resume hint after restoration so it
/// lands on the normal screen. A restore error propagates before any output;
/// the hint prints only on `Ok(Some)`; the original loop error is returned
/// unchanged when the loop failed.
fn finish_tui(
    loop_result: Result<Option<String>>,
    guard: &mut impl TerminalRestore,
    out: &mut impl io::Write,
) -> Result<()> {
    guard.restore()?;
    print_resume_hint(out, loop_result.as_ref().ok().and_then(|hint| hint.as_deref()))?;
    loop_result.map(|_| ())
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

fn tick_status_animation(state: &mut AppState, draw_scheduler: &mut DrawScheduler) {
    draw_scheduler.mark_dirty_if(state.advance_status_animation());
}

fn draw_cursor_safe_production_frame<B>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    previous_layout: AppLayout,
) -> Result<AppLayout, B::Error>
where
    B: ratatui::backend::Backend,
{
    terminal.hide_cursor()?;

    let mut next_layout = previous_layout;
    terminal.draw(|frame| {
        next_layout = draw_production_frame(frame, state, previous_layout);
    })?;

    Ok(next_layout)
}

async fn run_loop<B>(
    terminal: &mut Terminal<B>,
    mut state: AppState,
    runtime: &WorkflowRuntime,
    workflow_events: &mut tokio::sync::broadcast::Receiver<WorkflowEvent>,
) -> Result<Option<String>>
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
            return Ok(state.resume_hint());
        }
        if draw_scheduler.should_draw() {
            match draw_cursor_safe_production_frame(terminal, &mut state, current_layout) {
                Ok(layout) => current_layout = layout,
                Err(err) => {
                    tracing::error!(error = ?err, "TUI draw failed");
                    return Err(err.into());
                }
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
            tick_status_animation(&mut state, &mut draw_scheduler);
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
                    KeyHandling::Cancel => {
                        runtime.cancel_store_waits();
                        draw_scheduler.mark_dirty();
                    }
                    KeyHandling::Submit => {
                        commands::submit_input(&mut state, runtime).await;
                        draw_scheduler.mark_dirty();
                    }
                    KeyHandling::Exit => return Ok(state.resume_hint()),
                }
            }
            Event::Mouse(mouse) => {
                let handled = input::handle_mouse_event(&mut state, mouse, current_layout);
                if handled && let Err(err) = emit_pending_clipboard_copy(&mut state) {
                    tracing::warn!(error = ?err, "failed to copy transcript selection");
                }

                draw_scheduler.mark_dirty_if(handled);
            }
            Event::Resize(_, _) => draw_scheduler.mark_dirty(),
            event => {
                tracing::trace!(event = ?event, "TUI event ignored");
            }
        }
    }
}

fn emit_pending_clipboard_copy(state: &mut AppState) -> io::Result<bool> {
    let Some(text) = state.take_pending_clipboard_text() else {
        return Ok(false);
    };

    let mut stdout = io::stdout();
    write_osc52_clipboard(&mut stdout, &text)?;
    Ok(true)
}

fn write_osc52_clipboard(stdout: &mut impl io::Write, text: &str) -> io::Result<()> {
    stdout.write_all(b"\x1b]52;c;")?;
    stdout.write_all(
        base64::engine::general_purpose::STANDARD
            .encode(text.as_bytes())
            .as_bytes(),
    )?;
    stdout.write_all(b"\x07")?;
    stdout.flush()
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
