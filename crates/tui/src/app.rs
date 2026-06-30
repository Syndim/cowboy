use std::io;
use std::time::Duration;

use anyhow::Result;
use cowboy_workflow_engine::{WorkflowEvent, WorkflowRuntime};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
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

mod commands;
mod controls;
mod events;
mod input;
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
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, state, &runtime, &mut workflow_events).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    mut state: AppState,
    runtime: &WorkflowRuntime,
    workflow_events: &mut tokio::sync::broadcast::Receiver<WorkflowEvent>,
) -> Result<()> {
    loop {
        state.drain_workflow_events(workflow_events);
        state.drain_background_tasks().await;
        if state.exit_requested() {
            return Ok(());
        }
        terminal.draw(|frame| draw(frame, &state))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Paste(text) => state.push_input(&text),
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match input::handle_key_press(&mut state, key) {
                        KeyHandling::Continue => {}
                        KeyHandling::Submit => commands::submit_input(&mut state, runtime).await,
                        KeyHandling::Exit => return Ok(()),
                    }
                }
                _ => {}
            }
        }
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, state: &AppState) {
    let area = frame.area();
    let composer_height = composer::height(state, area.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
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
