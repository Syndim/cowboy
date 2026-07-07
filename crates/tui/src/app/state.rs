use anyhow::Result;
use cowboy_workflow_engine::{RunReport, WorkflowEvent, WorkflowEventKind};
use tui_input::{Input, InputRequest};

use super::events::render_workflow_event;
use super::history::{HISTORY_LOAD_LIMIT, InputHistory};
use super::markup::render_markup;
use super::styles::{
    style_accent, style_error, style_transcript_metadata, style_transcript_normal, style_warning,
};
use crate::config::AppConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingPrompt {
    run_id: String,
    step: String,
    prompt_id: String,
    message: String,
    choices: Vec<String>,
}

impl PendingPrompt {
    pub(in crate::app) fn step(&self) -> &str {
        &self.step
    }

    pub(in crate::app) fn prompt_id(&self) -> &str {
        &self.prompt_id
    }

    pub(in crate::app) fn message(&self) -> &str {
        &self.message
    }

    pub(in crate::app) fn choices(&self) -> &[String] {
        &self.choices
    }
}

#[derive(Debug, Clone)]
pub(super) enum TranscriptEntry {
    Workflow(WorkflowEvent),
    Card { title: String, details: Vec<String> },
    Plain(String),
}

impl TranscriptEntry {
    pub(in crate::app) fn render_lines(&self) -> Vec<ratatui::text::Line<'static>> {
        match self {
            Self::Workflow(event) => render_workflow_event(event).lines().to_vec(),
            Self::Card { title, details } => render_card_lines(title, details),
            Self::Plain(text) => text
                .lines()
                .map(|line| {
                    ratatui::text::Line::from(ratatui::text::Span::styled(
                        line.to_string(),
                        super::styles::style_transcript_normal(),
                    ))
                })
                .collect(),
        }
    }

    pub(in crate::app) fn plain_text(&self) -> String {
        match self {
            Self::Workflow(event) => render_workflow_event(event).text().to_string(),
            Self::Card { title, details } => card_plain_text(title, details),
            Self::Plain(text) => text.clone(),
        }
    }

    pub(in crate::app) fn contains(&self, needle: &str) -> bool {
        self.plain_text().contains(needle)
    }

    #[cfg(test)]
    pub(in crate::app) fn matches(&self, needle: &str) -> usize {
        self.plain_text().matches(needle).count()
    }
}

pub(in crate::app) fn render_pending_prompt_lines(
    prompt: &PendingPrompt,
) -> Vec<ratatui::text::Line<'static>> {
    let choices = display_prompt_choices(prompt.choices());
    let mut lines = vec![ratatui::text::Line::from(vec![
        ratatui::text::Span::styled("Waiting for input", style_warning()),
        ratatui::text::Span::styled("  step=", style_transcript_metadata()),
        ratatui::text::Span::styled(prompt.step().to_string(), style_accent()),
        ratatui::text::Span::styled("  prompt=", style_transcript_metadata()),
        ratatui::text::Span::styled(prompt.prompt_id().to_string(), style_transcript_metadata()),
        ratatui::text::Span::styled("  choices=", style_transcript_metadata()),
        ratatui::text::Span::styled(choices, style_warning()),
    ])];
    lines.extend(render_markup(prompt.message(), style_transcript_normal()));
    lines
}

fn display_prompt_choices(choices: &[String]) -> String {
    if choices.is_empty() {
        "<freeform>".to_string()
    } else {
        choices.join(", ")
    }
}

fn render_card_lines(title: &str, details: &[String]) -> Vec<ratatui::text::Line<'static>> {
    let title_style = match title {
        "Cancelled" => style_error(),
        "Notice" => style_warning(),
        _ => style_accent(),
    };
    let mut lines = vec![ratatui::text::Line::from(ratatui::text::Span::styled(
        title.to_string(),
        title_style,
    ))];
    lines.extend(details.iter().map(|detail| {
        ratatui::text::Line::from(ratatui::text::Span::styled(
            format!("         {detail}"),
            style_transcript_normal(),
        ))
    }));
    lines
}

fn card_plain_text(title: &str, details: &[String]) -> String {
    let mut lines = vec![title.to_string()];
    for detail in details {
        lines.push(format!("         {detail}"));
    }
    lines.join("\n")
}

#[derive(Debug, Clone)]
struct ActiveEvent {
    index: usize,
    event: WorkflowEvent,
}

impl ActiveEvent {
    fn accepts(&self, event: &WorkflowEvent) -> bool {
        self.event.run_id == event.run_id && same_active_event_kind(&self.event.kind, &event.kind)
    }

    fn merge(&mut self, event: &WorkflowEvent) {
        match (&mut self.event.kind, &event.kind) {
            (
                WorkflowEventKind::AgentResponse { content, .. },
                WorkflowEventKind::AgentResponse { content: chunk, .. },
            )
            | (
                WorkflowEventKind::AgentThought { content, .. },
                WorkflowEventKind::AgentThought { content: chunk, .. },
            ) => content.push_str(chunk),
            (
                WorkflowEventKind::AgentToolCallUpdate { .. },
                WorkflowEventKind::AgentToolCallUpdate { .. },
            ) => self.event = event.clone(),
            _ => {}
        }
    }
}

fn same_active_event_kind(left: &WorkflowEventKind, right: &WorkflowEventKind) -> bool {
    match (left, right) {
        (
            WorkflowEventKind::AgentResponse { step_id: left, .. },
            WorkflowEventKind::AgentResponse { step_id: right, .. },
        )
        | (
            WorkflowEventKind::AgentThought { step_id: left, .. },
            WorkflowEventKind::AgentThought { step_id: right, .. },
        ) => left == right,
        (
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: left_step,
                tool_call_id: left_call,
                ..
            },
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: right_step,
                tool_call_id: right_call,
                ..
            },
        ) => left_step == right_step && left_call == right_call,
        _ => false,
    }
}

fn is_active_event(kind: &WorkflowEventKind) -> bool {
    matches!(
        kind,
        WorkflowEventKind::AgentResponse { .. }
            | WorkflowEventKind::AgentThought { .. }
            | WorkflowEventKind::AgentToolCallUpdate { .. }
    )
}

struct DrainResult {
    events: Vec<WorkflowEvent>,
    lagged: bool,
}

fn drain_available_events(
    workflow_events: &mut tokio::sync::broadcast::Receiver<WorkflowEvent>,
) -> DrainResult {
    let mut result = DrainResult {
        events: Vec::new(),
        lagged: false,
    };
    loop {
        match workflow_events.try_recv() {
            Ok(event) => result.events.push(event),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                result.lagged = true;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }
    result
}

#[derive(Debug)]
pub(super) struct AppState {
    active_run_id: Option<String>,
    current_step: Option<String>,
    workflow_name: Option<String>,
    pending_prompt: Option<PendingPrompt>,
    run_state: String,
    status: String,
    event_log: Vec<TranscriptEntry>,
    active_event: Option<ActiveEvent>,
    scroll_offset: usize,
    follow_events: bool,
    input: Input,
    history: Vec<String>,
    history_index: Option<usize>,
    history_store: InputHistory,
    background: Vec<tokio::task::JoinHandle<Result<RunReport, String>>>,
    exit_requested: bool,
}

impl AppState {
    pub(super) fn new(config: AppConfig) -> Self {
        let history_store = InputHistory::new(config.state_dir);
        let history = history_store.load();

        Self {
            active_run_id: None,
            current_step: None,
            workflow_name: None,
            pending_prompt: None,
            run_state: "idle".to_string(),
            status: "workflow runtime shell is ready".to_string(),
            event_log: Vec::new(),
            active_event: None,
            scroll_offset: 0,
            follow_events: true,
            input: Input::default(),
            history,
            history_index: None,
            history_store,
            background: Vec::new(),
            exit_requested: false,
        }
    }

    pub(in crate::app) fn active_run_id(&self) -> Option<&str> {
        self.active_run_id.as_deref()
    }

    pub(in crate::app) fn current_step(&self) -> Option<&str> {
        self.current_step.as_deref()
    }

    pub(in crate::app) fn workflow_name(&self) -> Option<&str> {
        self.workflow_name.as_deref()
    }

    pub(in crate::app) fn pending_prompt(&self) -> Option<&PendingPrompt> {
        self.pending_prompt.as_ref()
    }

    pub(in crate::app) fn composer_enabled(&self) -> bool {
        self.pending_prompt.is_some() || self.background.is_empty()
    }

    pub(in crate::app) fn display_state(&self) -> String {
        if self.pending_prompt.is_some() {
            "waiting".to_string()
        } else {
            self.run_state.clone()
        }
    }

    pub(in crate::app) fn status(&self) -> &str {
        &self.status
    }

    pub(in crate::app) fn event_entries(&self) -> &[TranscriptEntry] {
        &self.event_log
    }

    pub(in crate::app) fn event_log_is_empty(&self) -> bool {
        self.event_log.is_empty()
    }

    pub(in crate::app) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub(in crate::app) fn input(&self) -> &str {
        self.input.value()
    }

    pub(in crate::app) fn input_cursor(&self) -> usize {
        self.input.cursor()
    }

    #[cfg(test)]
    pub(in crate::app) fn set_input_cursor(&mut self, cursor: usize) {
        self.input.handle(InputRequest::SetCursor(cursor));
    }

    pub(in crate::app) fn background_task_count(&self) -> usize {
        self.background.len()
    }

    pub(in crate::app) fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    #[cfg(test)]
    pub(in crate::app) fn history_is_empty(&self) -> bool {
        self.history.is_empty()
    }

    #[cfg(test)]
    pub(in crate::app) fn is_following_events(&self) -> bool {
        self.follow_events
    }

    pub(in crate::app) fn push_input(&mut self, text: &str) {
        for ch in text.chars() {
            self.input.handle(InputRequest::InsertChar(ch));
        }
        self.history_index = None;
    }

    pub(in crate::app) fn pop_input_char(&mut self) {
        self.input.handle(InputRequest::DeletePrevChar);
        self.history_index = None;
    }

    pub(in crate::app) fn delete_input_char(&mut self) {
        self.input.handle(InputRequest::DeleteNextChar);
        self.history_index = None;
    }

    pub(in crate::app) fn move_input_cursor_left(&mut self) {
        self.input.handle(InputRequest::GoToPrevChar);
    }

    pub(in crate::app) fn move_input_cursor_right(&mut self) {
        self.input.handle(InputRequest::GoToNextChar);
    }

    pub(in crate::app) fn move_input_cursor_prev_word(&mut self) {
        self.input.handle(InputRequest::GoToPrevWord);
    }

    pub(in crate::app) fn move_input_cursor_next_word(&mut self) {
        self.input.handle(InputRequest::GoToNextWord);
    }

    pub(in crate::app) fn replace_input_from_completion(&mut self, input: String) {
        self.input = Input::new(input);
        self.history_index = None;
    }

    pub(in crate::app) fn take_submitted_input(&mut self) -> Option<String> {
        let input = self.input.value_and_reset();
        let input = input.trim();
        self.history_index = None;
        if input.is_empty() {
            return None;
        }
        self.persist_submitted_history(input);
        Some(input.to_string())
    }

    fn persist_submitted_history(&mut self, input: &str) {
        if let Some(history) = self.history_store.append(input) {
            self.history = history;
            return;
        }

        if self.history.last().is_none_or(|last| last != input) {
            self.history.push(input.to_string());
        }

        if self.history.len() > HISTORY_LOAD_LIMIT {
            let keep_from = self.history.len() - HISTORY_LOAD_LIMIT;
            self.history.drain(0..keep_from);
        }
    }

    pub(in crate::app) fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub(in crate::app) fn mark_exit_requested(&mut self) {
        self.exit_requested = true;
    }

    pub(in crate::app) fn clear_pending_prompt(&mut self) {
        self.pending_prompt = None;
    }

    pub(in crate::app) fn pending_prompt_answer_target(&self) -> Option<(String, String)> {
        self.pending_prompt
            .as_ref()
            .map(|prompt| (prompt.run_id.clone(), prompt.prompt_id.clone()))
    }

    pub(in crate::app) fn push_card(
        &mut self,
        title: &str,
        details: impl IntoIterator<Item = String>,
    ) {
        self.push_event(TranscriptEntry::Card {
            title: title.to_string(),
            details: details.into_iter().collect(),
        });
    }

    pub(in crate::app) fn cancel_background_tasks(&mut self) {
        if self.background.is_empty() {
            self.status = "no active background task".to_string();
            self.push_card("Notice", [self.status.clone()]);
            return;
        }

        let cancelled = self.background.len();
        for task in &self.background {
            task.abort();
        }
        self.background.clear();
        self.status = format!("cancelled {cancelled} background task(s)");
        self.run_state = "cancelled".to_string();
        self.push_card("Cancelled", [self.status.clone()]);
    }

    pub(in crate::app) fn scroll_events_up(&mut self) {
        self.follow_events = false;
        self.scroll_offset = self.scroll_offset.saturating_add(10);
    }

    pub(in crate::app) fn scroll_events_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(10);
        if self.scroll_offset == 0 {
            self.follow_events = true;
        }
    }

    pub(in crate::app) fn follow_latest(&mut self) {
        self.scroll_offset = 0;
        self.follow_events = true;
    }

    pub(in crate::app) fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = self
            .history_index
            .map(|index| index.saturating_sub(1))
            .unwrap_or_else(|| self.history.len() - 1);
        self.history_index = Some(next);
        self.input = Input::new(self.history[next].clone());
    }

    pub(in crate::app) fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.input.reset();
        } else {
            let next = index + 1;
            self.history_index = Some(next);
            self.input = Input::new(self.history[next].clone());
        }
    }

    pub(in crate::app) fn spawn_report_task<F>(&mut self, label: String, future: F)
    where
        F: Future<Output = Result<RunReport, String>> + Send + 'static,
    {
        self.status = label.clone();
        self.run_state = "running".to_string();
        self.push_event(TranscriptEntry::Plain(label));
        self.background.push(tokio::spawn(future));
    }

    pub(super) fn drain_workflow_events(
        &mut self,
        workflow_events: &mut tokio::sync::broadcast::Receiver<WorkflowEvent>,
    ) -> bool {
        let result = drain_available_events(workflow_events);
        let changed = result.lagged || !result.events.is_empty();
        if result.lagged {
            self.active_event = None;
        }
        for event in result.events {
            self.apply_workflow_event(event);
        }
        changed
    }

    pub(in crate::app) fn apply_workflow_event(&mut self, event: WorkflowEvent) {
        if self.try_coalesce_active_event(&event) {
            return;
        }

        let rendered = render_workflow_event(&event);
        self.status = rendered.text().to_string();
        let is_active_event = is_active_event(&event.kind);
        self.apply_workflow_event_metadata(&event);
        self.push_event(TranscriptEntry::Workflow(event.clone()));
        if is_active_event {
            self.active_event = Some(ActiveEvent {
                index: self.event_log.len().saturating_sub(1),
                event,
            });
        } else {
            self.active_event = None;
        }
    }

    fn try_coalesce_active_event(&mut self, event: &WorkflowEvent) -> bool {
        let (index, rendered, active_event) = {
            let Some(active) = self.active_event.as_mut() else {
                return false;
            };
            if active.index + 1 != self.event_log.len() || !active.accepts(event) {
                return false;
            }
            active.merge(event);
            (
                active.index,
                render_workflow_event(&active.event),
                active.event.clone(),
            )
        };

        self.status = rendered.text().to_string();
        self.event_log[index] = TranscriptEntry::Workflow(active_event);
        self.apply_workflow_event_metadata(event);
        if self.follow_events {
            self.scroll_offset = 0;
        }
        true
    }

    fn apply_workflow_event_metadata(&mut self, event: &WorkflowEvent) {
        self.active_run_id = Some(event.run_id.clone());
        match &event.kind {
            WorkflowEventKind::RunStarted {
                workflow_name,
                current_step,
            } => {
                self.workflow_name = Some(workflow_name.clone());
                self.current_step = Some(current_step.clone());
                self.run_state = "running".to_string();
            }
            WorkflowEventKind::StepStarted {
                step_id: current_step,
            }
            | WorkflowEventKind::StepProgress {
                step_id: current_step,
                ..
            }
            | WorkflowEventKind::AgentSessionReady {
                step_id: current_step,
                ..
            }
            | WorkflowEventKind::AgentPrompt {
                step_id: current_step,
                ..
            }
            | WorkflowEventKind::AgentResponse {
                step_id: current_step,
                ..
            }
            | WorkflowEventKind::AgentThought {
                step_id: current_step,
                ..
            }
            | WorkflowEventKind::AgentToolCall {
                step_id: current_step,
                ..
            }
            | WorkflowEventKind::AgentToolCallUpdate {
                step_id: current_step,
                ..
            }
            | WorkflowEventKind::AgentPlan {
                step_id: current_step,
                ..
            } => {
                self.current_step = Some(current_step.clone());
                self.run_state = "running".to_string();
            }
            WorkflowEventKind::WaitingForInput {
                step,
                prompt_id,
                message,
                choices,
            } => {
                self.current_step = Some(step.clone());
                self.run_state = "waiting".to_string();
                self.pending_prompt = Some(PendingPrompt {
                    run_id: event.run_id.clone(),
                    step: step.clone(),
                    prompt_id: prompt_id.clone(),
                    message: message.clone(),
                    choices: choices.clone(),
                });
            }
            WorkflowEventKind::StepCompleted { step_id, .. } => {
                self.current_step = Some(step_id.clone());
            }
            WorkflowEventKind::StepRetrying { step_id, .. } => {
                self.current_step = Some(step_id.clone());
                self.run_state = "retrying".to_string();
            }
            WorkflowEventKind::ManuallyResolved { step_id, .. } => {
                self.current_step = Some(step_id.clone());
                self.run_state = "running".to_string();
                self.pending_prompt = None;
            }
            WorkflowEventKind::RunCompleted => {
                self.run_state = "completed".to_string();
                self.pending_prompt = None;
            }
            WorkflowEventKind::RunFailed { .. } => {
                self.run_state = "failed".to_string();
                self.pending_prompt = None;
            }
            WorkflowEventKind::RunCancelled => {
                self.run_state = "cancelled".to_string();
                self.pending_prompt = None;
            }
            WorkflowEventKind::RunStatusChanged { status } => self.run_state = status.clone(),
        }
    }

    pub(super) async fn drain_background_tasks(&mut self) -> bool {
        let mut changed = false;
        let mut pending = Vec::new();
        let tasks = std::mem::take(&mut self.background);
        for task in tasks {
            if task.is_finished() {
                changed = true;
                match task.await {
                    Ok(Ok(report)) => self.apply_report(report),
                    Ok(Err(err)) => {
                        self.status = format!("error: {err}");
                        self.push_event(TranscriptEntry::Plain(self.status.clone()));
                    }
                    Err(err) if err.is_cancelled() => {
                        self.status = "background task cancelled".to_string();
                        self.run_state = "cancelled".to_string();
                        self.push_event(TranscriptEntry::Plain(self.status.clone()));
                    }
                    Err(err) => {
                        self.status = format!("background task failed: {err}");
                        self.push_event(TranscriptEntry::Plain(self.status.clone()));
                    }
                }
            } else {
                pending.push(task);
            }
        }
        self.background = pending;
        changed
    }

    fn push_event(&mut self, event: TranscriptEntry) {
        self.event_log.push(event);
        if self.follow_events {
            self.scroll_offset = 0;
        }
    }

    fn apply_report(&mut self, report: RunReport) {
        self.active_run_id = Some(report.run.id.clone());
        self.workflow_name = Some(report.run.workflow_name.clone());
        self.current_step = Some(report.run.current_step.clone());
        self.run_state = format!("{:?}", report.run.status).to_ascii_lowercase();
        self.status = format!(
            "run={} status={:?} step={}",
            report.run.id, report.run.status, report.run.current_step
        );
        for event in report.events {
            self.apply_workflow_event(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};
    use std::fs;
    use std::fs::OpenOptions;
    use std::path::Path;
    use std::process::{Child, Command};
    use std::thread;
    use std::time::{Duration, Instant};

    use fs2::FileExt;

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

    fn spawn_pending_report_task(state: &mut AppState) {
        state.spawn_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<RunReport, String>>().await
        });
    }

    #[test]
    fn consecutive_agent_response_chunks_append_to_current_transcript_entry() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "Hello".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: ", world".to_string(),
            },
        ));

        assert_eq!(state.event_entries().len(), 1);
        let entry = &state.event_entries()[0];
        assert_eq!(entry.matches("Agent response"), 1);
        assert!(entry.contains("Hello, world"), "{}", entry.plain_text());
        assert!(
            !entry.contains("content: Hello, world"),
            "{}",
            entry.plain_text()
        );
    }

    #[test]
    fn consecutive_agent_thought_chunks_append_to_current_transcript_entry() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentThought {
                step_id: "plan".to_string(),
                content: "checking".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentThought {
                step_id: "plan".to_string(),
                content: " approach".to_string(),
            },
        ));

        assert_eq!(state.event_entries().len(), 1);
        let entry = &state.event_entries()[0];
        assert_eq!(entry.matches("Agent thinking"), 1);
        assert!(
            entry.contains("checking approach"),
            "{}",
            entry.plain_text()
        );
        assert!(
            !entry.contains("thought: checking approach"),
            "{}",
            entry.plain_text()
        );
    }

    #[test]
    fn stream_chunk_after_intervening_entry_starts_new_transcript_entry() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "Hel".to_string(),
            },
        ));
        state.push_card("Notice", ["non-workflow boundary".to_string()]);
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "lo".to_string(),
            },
        ));

        assert_eq!(state.event_entries().len(), 3);
        assert!(state.event_entries()[0].contains("Agent response"));
        assert!(state.event_entries()[0].contains("Hel"));
        assert!(!state.event_entries()[0].contains("content: Hel"));
        assert_eq!(state.event_entries()[0].matches("Agent response"), 1);
        assert!(state.event_entries()[1].contains("Notice"));
        assert!(state.event_entries()[1].contains("non-workflow boundary"));
        assert!(state.event_entries()[2].contains("Agent response"));
        assert!(state.event_entries()[2].contains("lo"));
        assert!(!state.event_entries()[2].contains("content: lo"));
    }

    #[test]
    fn consecutive_tool_call_updates_replace_current_transcript_entry() {
        let mut state = test_state();
        let first = r#"{"content":[{"type":"text","text":""}],"details":{"jobs":[{"id":"job-123","type":"task","status":"running","label":"TuiLagRegressionTest","durationMs":123798}]}}"#;
        let latest = r#"{"content":[{"type":"text","text":""}],"details":{"jobs":[{"id":"job-123","type":"task","status":"running","label":"TuiLagRegressionTest","durationMs":124300}]}}"#;

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "investigate".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Waiting on tester".to_string(),
                status: "in_progress".to_string(),
                content: Some(serde_json::json!(first)),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "investigate".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Waiting on tester".to_string(),
                status: "in_progress".to_string(),
                content: Some(serde_json::json!(latest)),
            },
        ));

        assert_eq!(state.event_entries().len(), 1);
        let entry = &state.event_entries()[0];
        assert_eq!(entry.matches("Agent tool update"), 1);
        assert!(
            entry.contains("TuiLagRegressionTest"),
            "{}",
            entry.plain_text()
        );
        assert!(entry.contains("running"), "{}", entry.plain_text());
        assert!(!entry.contains("durationMs"), "{}", entry.plain_text());
        assert!(!entry.contains("job-123"), "{}", entry.plain_text());
        assert!(!entry.contains("{"), "{}", entry.plain_text());

        let TranscriptEntry::Workflow(event) = entry else {
            panic!("expected workflow entry");
        };
        let WorkflowEventKind::AgentToolCallUpdate { content, .. } = &event.kind else {
            panic!("expected tool update event");
        };
        assert!(content.as_ref().is_some_and(|value| value == latest));
        assert!(!content.as_ref().is_some_and(|value| value == first));
    }

    #[test]
    fn workflow_event_drain_returns_false_when_no_events_are_pending() {
        let mut state = test_state();
        let bus = cowboy_workflow_engine::EventBus::new(8);
        let mut rx = bus.subscribe();
        let status = state.status().to_string();

        assert!(!state.drain_workflow_events(&mut rx));

        assert_eq!(state.status(), status);
        assert!(state.event_log_is_empty());
    }

    #[test]
    fn workflow_event_drain_returns_true_when_event_is_applied() {
        let mut state = test_state();
        let bus = cowboy_workflow_engine::EventBus::new(8);
        let mut rx = bus.subscribe();
        bus.emit(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunStatusChanged {
                status: "running".to_string(),
            },
        ));

        assert!(state.drain_workflow_events(&mut rx));

        assert_eq!(state.event_entries().len(), 1);
        assert_eq!(state.display_state(), "running");
    }

    #[tokio::test]
    async fn background_task_drain_returns_false_when_no_task_finished() {
        let mut state = test_state();

        assert!(!state.drain_background_tasks().await);

        state.spawn_report_task("pending".to_string(), std::future::pending());

        assert!(!state.drain_background_tasks().await);
        assert_eq!(state.background_task_count(), 1);
    }

    #[tokio::test]
    async fn composer_enabled_tracks_background_prompt_and_cancel_transitions() {
        let mut state = test_state();
        assert!(state.composer_enabled());

        spawn_pending_report_task(&mut state);
        assert!(!state.composer_enabled());

        state.cancel_background_tasks();
        assert_eq!(state.background_task_count(), 0);
        assert!(state.composer_enabled());

        let mut waiting_state = test_state();
        spawn_pending_report_task(&mut waiting_state);
        assert!(!waiting_state.composer_enabled());

        waiting_state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
            },
        ));

        assert_eq!(waiting_state.display_state(), "waiting");
        assert_eq!(
            waiting_state.pending_prompt_answer_target(),
            Some(("run-1".to_string(), "approval".to_string()))
        );
        assert!(waiting_state.composer_enabled());

        waiting_state.cancel_background_tasks();
        assert_eq!(waiting_state.background_task_count(), 0);
        assert!(waiting_state.composer_enabled());
    }

    #[tokio::test]
    async fn background_task_drain_returns_true_when_task_finished() {
        let mut state = test_state();
        state.spawn_report_task("finished".to_string(), async { Err("boom".to_string()) });
        tokio::task::yield_now().await;

        assert!(state.drain_background_tasks().await);

        assert_eq!(state.background_task_count(), 0);
        assert_eq!(state.status(), "error: boom");
        assert!(
            state
                .event_entries()
                .last()
                .unwrap()
                .contains("error: boom")
        );
    }

    #[tokio::test]
    async fn workflow_event_drain_breaks_active_stream_after_receiver_lag() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "pre-lag".to_string(),
            },
        ));
        let bus = cowboy_workflow_engine::EventBus::new(2);
        let mut rx = bus.subscribe();

        for content in ["one", "two", "three"] {
            bus.emit(WorkflowEvent::new(
                "run-1",
                WorkflowEventKind::AgentResponse {
                    step_id: "implement".to_string(),
                    content: content.to_string(),
                },
            ));
        }

        assert!(state.drain_workflow_events(&mut rx));

        assert_eq!(state.event_entries().len(), 2);
        assert!(state.event_entries()[0].contains("pre-lag"));
        assert!(!state.event_entries()[0].contains("content: pre-lag"));
        assert!(!state.event_entries()[0].contains("three"));
        assert!(state.event_entries()[1].contains("Agent response"));
        assert!(state.event_entries()[1].contains("two"));
        assert!(state.event_entries()[1].contains("three"));
    }

    #[test]
    fn contended_history_lock_does_not_block_startup_or_submission() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("input_history"),
            r#"{"version":1,"entry":"on disk"}"#.to_string() + "\n",
        )
        .unwrap();
        let ready_path = dir.path().join("ready");
        let mut helper = spawn_lock_helper(&state_dir.join("input_history.lock"), &ready_path);
        wait_for_ready(&ready_path);
        let config = AppConfig {
            state_dir: state_dir.clone(),
            workflow_store: state_dir.join("workflow.redb"),
            max_steps_per_run: 1,
            max_visits_per_step: 1,
            ..AppConfig::default()
        };

        let started = Instant::now();
        let mut state = AppState::new(config);
        let startup_elapsed = started.elapsed();
        state.push_input("during contention");
        let started = Instant::now();
        let submitted = state.take_submitted_input();
        let submit_elapsed = started.elapsed();

        stop_helper(&mut helper);
        assert!(
            startup_elapsed < Duration::from_secs(1),
            "startup elapsed: {startup_elapsed:?}"
        );
        assert!(
            submit_elapsed < Duration::from_secs(1),
            "submit elapsed: {submit_elapsed:?}"
        );
        assert_eq!(submitted, Some("during contention".to_string()));
        assert!(
            !fs::read_to_string(state_dir.join("input_history"))
                .unwrap()
                .contains("during contention")
        );
        state.history_previous();
        assert_eq!(state.input(), "during contention");
    }

    fn spawn_lock_helper(lock_path: &Path, ready_path: &Path) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("app::state::tests::hold_history_lock_helper")
            .arg("--ignored")
            .env("COWBOY_HISTORY_LOCK_PATH", lock_path)
            .env("COWBOY_HISTORY_LOCK_READY", ready_path)
            .spawn()
            .unwrap()
    }

    fn wait_for_ready(ready_path: &Path) {
        let started = Instant::now();
        while !ready_path.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "lock helper did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn stop_helper(helper: &mut Child) {
        let _ = helper.kill();
        let _ = helper.wait();
    }

    #[test]
    #[ignore]
    fn hold_history_lock_helper() {
        let Ok(lock_path) = std::env::var("COWBOY_HISTORY_LOCK_PATH") else {
            return;
        };
        let ready_path = std::env::var("COWBOY_HISTORY_LOCK_READY").unwrap();
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        FileExt::lock_exclusive(&lock_file).unwrap();
        fs::write(ready_path, "ready").unwrap();
        thread::sleep(Duration::from_secs(10));
    }
}
