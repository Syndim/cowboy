use std::cell::Cell;

use anyhow::Result;
use cowboy_tui_animation::FrameCycle;
use cowboy_workflow_engine::{
    RunReport, RunStatusDetail, RunStatusState, RunSummaryLine, WorkflowEvent, WorkflowEventKind,
};
use tui_input::{Input, InputRequest};

use super::card::{Card, CardMetadata, CardSection, CardTone};
use super::controls::chrome::status_icon;
use super::events::{render_workflow_event, render_workflow_event_width};
use super::history::{HISTORY_LOAD_LIMIT, InputHistory};
use super::markup::render_content;
use super::styles::{style_transcript_normal, style_warning};
use crate::config::AppConfig;
use crate::run_summary::render_run_summary_lines;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingPrompt {
    run_id: String,
    step: String,
    prompt_id: String,
    message: String,
    choices: Vec<String>,
}

impl PendingPrompt {
    pub(in crate::app) fn run_id(&self) -> &str {
        &self.run_id
    }

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
    Card {
        title: String,
        title_prefix: Vec<String>,
        title_suffix: Vec<String>,
        details: Vec<String>,
    },
}

impl TranscriptEntry {
    pub(in crate::app) fn render_lines_for_width(
        &self,
        width: usize,
    ) -> Vec<ratatui::text::Line<'static>> {
        match self {
            Self::Workflow(event) => render_workflow_event_width(event, width).lines().to_vec(),
            Self::Card {
                title,
                title_prefix,
                title_suffix,
                details,
            } => render_card_lines(title, title_prefix, title_suffix, details, width),
        }
    }

    #[allow(dead_code)]
    pub(in crate::app) fn plain_text(&self) -> String {
        match self {
            Self::Workflow(event) => render_workflow_event(event).text().to_string(),
            Self::Card {
                title,
                title_prefix,
                title_suffix,
                details,
            } => card_plain_text(title, title_prefix, title_suffix, details),
        }
    }

    #[allow(dead_code)]
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
    width: usize,
) -> Vec<ratatui::text::Line<'static>> {
    let mut card = Card::new(
        status_icon("waiting"),
        "Waiting for input",
        CardTone::Warning,
    )
    .metadata([
        CardMetadata::step(prompt.step()),
        CardMetadata::run(prompt.run_id()),
    ])
    .section(CardSection::body(render_content(
        prompt.message(),
        style_transcript_normal(),
    )));

    if !prompt.choices().is_empty() {
        card = card.section(CardSection::named(
            "Choices",
            vec![ratatui::text::Line::from(ratatui::text::Span::styled(
                display_prompt_choices(prompt.choices()),
                style_warning(),
            ))],
        ));
    }

    card.render(width)
}

fn display_prompt_choices(choices: &[String]) -> String {
    if choices.is_empty() {
        "<freeform>".to_string()
    } else {
        choices.join(" · ")
    }
}

fn render_card_lines(
    title: &str,
    title_prefix: &[String],
    title_suffix: &[String],
    details: &[String],
    width: usize,
) -> Vec<ratatui::text::Line<'static>> {
    app_card(title, title_prefix, title_suffix, details).render(width)
}

#[allow(dead_code)]
fn card_plain_text(
    title: &str,
    title_prefix: &[String],
    title_suffix: &[String],
    details: &[String],
) -> String {
    app_card(title, title_prefix, title_suffix, details).plain_text()
}

fn app_card(
    title: &str,
    title_prefix: &[String],
    title_suffix: &[String],
    details: &[String],
) -> Card {
    let (status, tone) = app_card_status_and_tone(title, title_suffix);
    let body = details
        .iter()
        .flat_map(|detail| render_content(detail, style_transcript_normal()))
        .collect::<Vec<_>>();
    let card = title_prefix
        .iter()
        .fold(Card::new(status, title, tone), |card, prefix| {
            card.title_prefix(prefix.clone())
        });
    let card = title_suffix
        .iter()
        .fold(card, |card, suffix| card.title_suffix(suffix.clone()));
    card.section(CardSection::body(body))
}

fn app_card_status_and_tone(title: &str, title_suffix: &[String]) -> (&'static str, CardTone) {
    if is_submitted_background_task_card(title, title_suffix)
        || is_loading_runs_background_card(title, title_suffix)
    {
        return (status_icon("running"), CardTone::Accent);
    }

    match title {
        "Error" => (status_icon("failed"), CardTone::Error),
        "Cancelled" => (status_icon("cancelled"), CardTone::Error),
        "Notice" => (status_icon("waiting"), CardTone::Warning),
        "Exit" | "Improve" | "Resolve" => (status_icon("completed"), CardTone::Success),
        "Run" => ("◌", CardTone::Neutral),
        "Usage" | "Help" | "Workflows" | "Runs" | "Transcript" => {
            (status_icon("idle"), CardTone::Neutral)
        }
        _ => (status_icon("idle"), CardTone::Accent),
    }
}

fn is_submitted_background_task_card(title: &str, title_suffix: &[String]) -> bool {
    title_suffix.iter().any(|suffix| match title {
        "Run" => suffix == "submitted run" || suffix.starts_with("submitted run "),
        "Step" => suffix == "submitted step",
        "Resume" => suffix == "submitted resume",
        "Answer" => suffix == "submitted answer",
        "Resolve" => suffix == "submitted resolve",
        _ => false,
    })
}

fn is_loading_runs_background_card(title: &str, title_suffix: &[String]) -> bool {
    title == "Runs" && title_suffix.iter().any(|suffix| suffix == "loading runs")
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
                WorkflowEventKind::AgentToolCall { .. },
                WorkflowEventKind::AgentToolCallUpdate { .. },
            )
            | (
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
            WorkflowEventKind::AgentToolCall {
                step_id: left_step,
                tool_call_id: left_call,
                ..
            }
            | WorkflowEventKind::AgentToolCallUpdate {
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
            | WorkflowEventKind::AgentToolCall { .. }
            | WorkflowEventKind::AgentToolCallUpdate { .. }
    )
}

fn active_event_status_text(event: &WorkflowEvent) -> String {
    match &event.kind {
        WorkflowEventKind::AgentResponse { .. } => "Agent response streaming".to_string(),
        WorkflowEventKind::AgentThought { .. } => "Agent thinking".to_string(),
        _ => render_workflow_event(event).text().to_string(),
    }
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

const DEFAULT_COMPOSER_CONTENT_WIDTH: usize = 80;

#[derive(Clone, Copy, Debug)]
struct ComposerViewState {
    content_width: usize,
    visible_input_rows: usize,
    viewport_start: usize,
    preferred_column: Option<usize>,
}

impl Default for ComposerViewState {
    fn default() -> Self {
        Self {
            content_width: DEFAULT_COMPOSER_CONTENT_WIDTH,
            visible_input_rows: 1,
            viewport_start: 0,
            preferred_column: None,
        }
    }
}

fn run_status_state_from_str(status: &str) -> Option<RunStatusState> {
    match status {
        "running" | "retrying" => Some(RunStatusState::Running),
        "waiting" | "waiting_for_input" => Some(RunStatusState::WaitingForInput),
        "completed" => Some(RunStatusState::Completed),
        "failed" => Some(RunStatusState::Failed),
        "cancelled" => Some(RunStatusState::Cancelled),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ComposerSubmissionMode {
    Idle,
    PendingAnswer,
    AgentPrompt,
    ExecutionBlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundTaskKind {
    WorkflowExecution,
    RunsList,
}

#[derive(Debug)]
enum BackgroundTaskResult {
    WorkflowReport(Box<RunReport>),
    RunsList {
        runs: Vec<RunSummaryLine>,
        partial_run_id: Option<String>,
    },
}

#[derive(Debug)]
struct BackgroundTask {
    kind: BackgroundTaskKind,
    handle: tokio::task::JoinHandle<Result<BackgroundTaskResult, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct AgentPromptWindowState {
    pub run_id: String,
    pub step_id: String,
    pub window_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct TranscriptSelectionPoint {
    pub row: usize,
    pub column: usize,
}

impl TranscriptSelectionPoint {
    pub(in crate::app) fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct TranscriptSelection {
    pub anchor: TranscriptSelectionPoint,
    pub focus: TranscriptSelectionPoint,
    pub active: bool,
    pub selected_text: String,
}

#[derive(Debug)]
pub(super) struct AppState {
    active_run_id: Option<String>,
    current_step: Option<String>,
    workflow_name: Option<String>,
    current_topic_run_id: Option<String>,
    current_run_topic: Option<String>,
    pending_prompt: Option<PendingPrompt>,
    durable_run_status: Option<RunStatusState>,
    agent_prompt_window: Option<AgentPromptWindowState>,
    run_state: String,
    status: String,
    event_log: Vec<TranscriptEntry>,
    active_event: Option<ActiveEvent>,
    scroll_offset: usize,
    transcript_scroll_limit: usize,
    mouse_scroll_lines: usize,
    transcript_selection: Option<TranscriptSelection>,
    pending_clipboard_text: Option<String>,
    follow_events: bool,
    input: Input,
    history: Vec<String>,
    history_index: Option<usize>,
    composer_view: Cell<ComposerViewState>,
    status_animation: FrameCycle,
    history_store: InputHistory,
    background: Vec<BackgroundTask>,
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
            current_topic_run_id: None,
            current_run_topic: None,
            pending_prompt: None,
            durable_run_status: None,
            agent_prompt_window: None,
            run_state: "idle".to_string(),
            status: "workflow runtime shell is ready".to_string(),
            event_log: Vec::new(),
            active_event: None,
            scroll_offset: 0,
            transcript_scroll_limit: usize::MAX,
            mouse_scroll_lines: usize::from(config.mouse_scroll_lines),
            transcript_selection: None,
            pending_clipboard_text: None,
            follow_events: true,
            input: Input::default(),
            history,
            history_index: None,
            status_animation: FrameCycle::running_status(),
            composer_view: Cell::new(ComposerViewState::default()),
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

    pub(in crate::app) fn current_run_topic(&self) -> Option<&str> {
        self.current_run_topic.as_deref()
    }

    pub(in crate::app) fn pending_prompt(&self) -> Option<&PendingPrompt> {
        self.pending_prompt.as_ref()
    }

    pub(in crate::app) fn workflow_execution_running(&self) -> bool {
        self.background
            .iter()
            .any(|task| task.kind == BackgroundTaskKind::WorkflowExecution)
    }

    pub(in crate::app) fn agent_prompt_window(&self) -> Option<&AgentPromptWindowState> {
        self.agent_prompt_window.as_ref()
    }

    pub(in crate::app) fn composer_submission_mode(&self) -> ComposerSubmissionMode {
        if self.pending_prompt.is_some() {
            return ComposerSubmissionMode::PendingAnswer;
        }
        if !self.workflow_execution_running() {
            return ComposerSubmissionMode::Idle;
        }
        if self.durable_run_status == Some(RunStatusState::Running)
            && self
                .agent_prompt_window
                .as_ref()
                .is_some_and(|window| self.active_run_id.as_deref() == Some(window.run_id.as_str()))
        {
            ComposerSubmissionMode::AgentPrompt
        } else {
            ComposerSubmissionMode::ExecutionBlocked
        }
    }

    pub(in crate::app) fn composer_accepts_edits(&self) -> bool {
        true
    }

    pub(in crate::app) fn composer_accepts_submit(&self) -> bool {
        self.composer_submission_mode() != ComposerSubmissionMode::ExecutionBlocked
            || self.input().trim().starts_with('/')
    }

    pub(in crate::app) fn composer_shows_cursor(&self) -> bool {
        self.composer_accepts_edits() && !self.status_animation_active()
    }

    pub(in crate::app) fn display_state(&self) -> String {
        if self.pending_prompt.is_some() {
            "waiting".to_string()
        } else {
            self.run_state.clone()
        }
    }

    pub(in crate::app) fn status_animation_active(&self) -> bool {
        self.pending_prompt.is_none() && self.run_state == "running"
    }

    pub(in crate::app) fn status_animation_frame(&self) -> &'static str {
        self.status_animation.current()
    }

    pub(in crate::app) fn advance_status_animation(&mut self) -> bool {
        if self.status_animation_active() {
            self.status_animation.advance();
            return true;
        }

        self.status_animation.reset();
        false
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
    pub(in crate::app) fn mouse_scroll_lines(&self) -> usize {
        self.mouse_scroll_lines
    }

    pub(in crate::app) fn next_scroll_offset_by(&self, visual_rows: usize) -> usize {
        self.scroll_offset.saturating_add(visual_rows)
    }

    pub(in crate::app) fn set_transcript_scroll_limit(&mut self, limit: usize) {
        let previous_offset = self.scroll_offset;
        self.transcript_scroll_limit = limit;
        self.scroll_offset = self.scroll_offset.min(limit);
        if self.scroll_offset != previous_offset {
            self.clear_transcript_selection();
        }

        if self.scroll_offset == 0 {
            self.follow_events = true;
        }
    }

    pub(in crate::app) fn transcript_selection(&self) -> Option<&TranscriptSelection> {
        self.transcript_selection.as_ref()
    }

    pub(in crate::app) fn transcript_selection_is_active(&self) -> bool {
        self.transcript_selection
            .as_ref()
            .is_some_and(|selection| selection.active)
    }

    pub(in crate::app) fn start_transcript_selection(&mut self, point: TranscriptSelectionPoint) {
        self.transcript_selection = Some(TranscriptSelection {
            anchor: point,
            focus: point,
            active: true,
            selected_text: String::new(),
        });
    }

    pub(in crate::app) fn update_transcript_selection(&mut self, point: TranscriptSelectionPoint) {
        if let Some(selection) = self.transcript_selection.as_mut()
            && selection.active
        {
            selection.focus = point;
        }
    }

    pub(in crate::app) fn set_transcript_selection_text(&mut self, selected_text: String) {
        if let Some(selection) = self.transcript_selection.as_mut() {
            selection.selected_text = selected_text;
        }
    }

    pub(in crate::app) fn finalize_transcript_selection(&mut self, selected_text: String) {
        if let Some(selection) = self.transcript_selection.as_mut() {
            selection.active = false;
            selection.selected_text = selected_text.clone();
        }

        if !selected_text.is_empty() {
            self.pending_clipboard_text = Some(selected_text);
        }
    }

    pub(in crate::app) fn clear_transcript_selection(&mut self) {
        self.transcript_selection = None;
    }

    pub(in crate::app) fn take_pending_clipboard_text(&mut self) -> Option<String> {
        self.pending_clipboard_text.take()
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
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn publish_composer_layout(
        &self,
        content_width: usize,
        visible_input_rows: usize,
    ) {
        let mut view = self.composer_view.get();
        let content_width = content_width.max(1);
        if view.content_width != content_width {
            view.content_width = content_width;
            view.viewport_start = 0;
            view.preferred_column = None;
        }

        view.visible_input_rows = visible_input_rows.max(1);
        self.composer_view.set(view);
    }

    pub(in crate::app) fn composer_layout_metrics(&self) -> (usize, usize) {
        let view = self.composer_view.get();
        (view.content_width, view.visible_input_rows)
    }

    pub(in crate::app) fn composer_viewport_start(
        &self,
        row_count: usize,
        cursor_row: usize,
    ) -> usize {
        let mut view = self.composer_view.get();
        let visible_rows = view.visible_input_rows.max(1);
        let max_start = row_count.saturating_sub(visible_rows);
        let mut start = view.viewport_start.min(max_start);
        if cursor_row < start {
            start = cursor_row;
        } else if cursor_row >= start.saturating_add(visible_rows) {
            start = cursor_row.saturating_add(1).saturating_sub(visible_rows);
        }

        view.viewport_start = start.min(max_start);
        self.composer_view.set(view);
        view.viewport_start
    }

    pub(in crate::app) fn composer_page_step(&self) -> usize {
        self.composer_view
            .get()
            .visible_input_rows
            .saturating_sub(1)
            .max(1)
    }

    pub(in crate::app) fn composer_vertical_target_column(
        &self,
        current_column: usize,
        source_max_column: usize,
        target_max_column: usize,
    ) -> usize {
        let mut view = self.composer_view.get();
        let cursor_in_middle = current_column < source_max_column;
        let target_too_short = target_max_column < current_column;
        let target_column = match view.preferred_column {
            None if target_too_short => {
                view.preferred_column = Some(current_column);
                target_max_column
            }
            None => current_column,
            Some(_) if cursor_in_middle && target_too_short => {
                view.preferred_column = Some(current_column);
                target_max_column
            }
            Some(_) if cursor_in_middle => {
                view.preferred_column = None;
                current_column
            }
            Some(preferred) if target_too_short || target_max_column < preferred => {
                target_max_column
            }
            Some(preferred) => {
                view.preferred_column = None;
                preferred
            }
        };

        self.composer_view.set(view);
        target_column
    }

    pub(in crate::app) fn set_input_cursor_vertical(&mut self, cursor: usize) {
        self.input.handle(InputRequest::SetCursor(cursor));
    }

    pub(in crate::app) fn set_input_cursor_boundary(&mut self, cursor: usize) {
        self.input.handle(InputRequest::SetCursor(cursor));
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn history_is_active(&self) -> bool {
        self.history_index.is_some()
    }

    #[cfg(test)]
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
        self.history_index = None;
        for ch in text.chars() {
            self.input.handle(InputRequest::InsertChar(ch));
        }

        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn push_typed_char(&mut self, ch: char) {
        self.prepare_typed_history_edit();
        self.input.handle(InputRequest::InsertChar(ch));
        self.invalidate_composer_preferred_column();
    }

    fn prepare_typed_history_edit(&mut self) {
        if self.history_index.is_none() {
            return;
        }

        if self.input.cursor() == 0 {
            let input_end = self.input.value().chars().count();
            self.input.handle(InputRequest::SetCursor(input_end));
        }

        self.history_index = None;
    }

    pub(in crate::app) fn pop_input_char(&mut self) {
        self.input.handle(InputRequest::DeletePrevChar);
        self.history_index = None;
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn delete_input_char(&mut self) {
        self.input.handle(InputRequest::DeleteNextChar);
        self.history_index = None;
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn move_input_cursor_left(&mut self) {
        self.input.handle(InputRequest::GoToPrevChar);
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn move_input_cursor_right(&mut self) {
        self.input.handle(InputRequest::GoToNextChar);
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn move_input_cursor_prev_word(&mut self) {
        self.input.handle(InputRequest::GoToPrevWord);
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn move_input_cursor_next_word(&mut self) {
        self.input.handle(InputRequest::GoToNextWord);
        self.invalidate_composer_preferred_column();
    }

    pub(in crate::app) fn replace_input_from_completion(&mut self, input: String) {
        self.input = Input::new(input);
        self.history_index = None;
        self.reset_composer_view();
    }

    pub(in crate::app) fn submitted_input(&self) -> Option<String> {
        let input = self.input.value();
        (!input.trim().is_empty()).then(|| input.to_string())
    }

    pub(in crate::app) fn commit_submitted_input(&mut self, input: &str) {
        self.input.reset();
        self.history_index = None;
        self.reset_composer_view();
        self.persist_submitted_history(input);
    }

    #[cfg(test)]
    pub(in crate::app) fn take_submitted_input(&mut self) -> Option<String> {
        let input = self.submitted_input()?;
        let trimmed = input.trim().to_string();
        self.commit_submitted_input(&trimmed);
        Some(trimmed)
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
            title_prefix: Vec::new(),
            title_suffix: Vec::new(),
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
        let cancelled_workflow = self
            .background
            .iter()
            .any(|task| task.kind == BackgroundTaskKind::WorkflowExecution);
        for task in &self.background {
            task.handle.abort();
        }
        self.background.clear();
        if cancelled_workflow {
            self.agent_prompt_window = None;
            self.durable_run_status = Some(RunStatusState::Cancelled);
            self.run_state = "cancelled".to_string();
        }
        self.status = format!("cancelled {cancelled} background task(s)");
        self.push_card("Cancelled", [self.status.clone()]);
    }

    pub(in crate::app) fn scroll_events_up(&mut self) -> bool {
        self.scroll_events_up_by(10)
    }

    pub(in crate::app) fn scroll_events_up_by(&mut self, visual_rows: usize) -> bool {
        let next_offset = self
            .next_scroll_offset_by(visual_rows)
            .min(self.transcript_scroll_limit);
        if next_offset == self.scroll_offset {
            return false;
        }

        self.scroll_offset = next_offset;
        self.follow_events = false;
        self.clear_transcript_selection();
        true
    }

    pub(in crate::app) fn scroll_events_down(&mut self) -> bool {
        self.scroll_events_down_by(10)
    }

    pub(in crate::app) fn scroll_events_down_by(&mut self, visual_rows: usize) -> bool {
        let previous = (self.scroll_offset, self.follow_events);
        self.scroll_offset = self.scroll_offset.saturating_sub(visual_rows);
        if self.scroll_offset == 0 {
            self.follow_events = true;
        }

        if previous == (self.scroll_offset, self.follow_events) {
            return false;
        }

        self.clear_transcript_selection();
        true
    }

    pub(in crate::app) fn follow_latest(&mut self) {
        self.scroll_offset = 0;
        self.follow_events = true;
        self.clear_transcript_selection();
    }

    pub(in crate::app) fn history_previous(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }

        let next = self
            .history_index
            .map(|index| index.saturating_sub(1))
            .unwrap_or_else(|| self.history.len() - 1);

        self.history_index = Some(next);
        self.input = Input::new(self.history[next].clone());
        self.input.handle(InputRequest::SetCursor(0));
        self.reset_composer_view();
        true
    }

    pub(in crate::app) fn history_next(&mut self) -> bool {
        let Some(index) = self.history_index else {
            return false;
        };

        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.input.reset();
        } else {
            let next = index + 1;
            self.history_index = Some(next);
            self.input = Input::new(self.history[next].clone());
        }

        self.reset_composer_view();
        true
    }

    fn invalidate_composer_preferred_column(&self) {
        let mut view = self.composer_view.get();
        view.preferred_column = None;
        self.composer_view.set(view);
    }

    fn reset_composer_view(&self) {
        let mut view = self.composer_view.get();
        view.viewport_start = 0;
        view.preferred_column = None;
        self.composer_view.set(view);
    }
    pub(in crate::app) fn spawn_card_report_task<F>(
        &mut self,
        title: &str,
        title_prefix: impl IntoIterator<Item = String>,
        title_suffix: impl IntoIterator<Item = String>,
        status: String,
        details: impl IntoIterator<Item = String>,
        future: F,
    ) where
        F: Future<Output = Result<RunReport, String>> + Send + 'static,
    {
        self.spawn_report_task_with_entry(
            status,
            TranscriptEntry::Card {
                title: title.to_string(),
                title_prefix: title_prefix.into_iter().collect(),
                title_suffix: title_suffix.into_iter().collect(),
                details: details.into_iter().collect(),
            },
            future,
        );
    }

    pub(in crate::app) fn spawn_runs_list_task<F>(
        &mut self,
        status: String,
        partial_run_id: Option<String>,
        future: F,
    ) where
        F: Future<Output = Result<Vec<RunSummaryLine>, String>> + Send + 'static,
    {
        self.spawn_background_task(
            BackgroundTaskKind::RunsList,
            status,
            TranscriptEntry::Card {
                title: "Runs".to_string(),
                title_prefix: Vec::new(),
                title_suffix: vec!["loading runs".to_string()],
                details: vec!["Loading runs".to_string()],
            },
            async move {
                future.await.map(|runs| BackgroundTaskResult::RunsList {
                    runs,
                    partial_run_id,
                })
            },
        );
    }

    #[cfg(test)]
    pub(in crate::app) fn spawn_test_card_report_task<F>(&mut self, status: String, future: F)
    where
        F: Future<Output = Result<RunReport, String>> + Send + 'static,
    {
        self.spawn_card_report_task("Task", [], [], status.clone(), [status], future);
    }

    fn spawn_report_task_with_entry<F>(&mut self, status: String, entry: TranscriptEntry, future: F)
    where
        F: Future<Output = Result<RunReport, String>> + Send + 'static,
    {
        self.spawn_background_task(
            BackgroundTaskKind::WorkflowExecution,
            status,
            entry,
            async move {
                future
                    .await
                    .map(|report| BackgroundTaskResult::WorkflowReport(Box::new(report)))
            },
        );
    }

    fn spawn_background_task<F>(
        &mut self,
        kind: BackgroundTaskKind,
        status: String,
        entry: TranscriptEntry,
        future: F,
    ) where
        F: Future<Output = Result<BackgroundTaskResult, String>> + Send + 'static,
    {
        self.status = status;
        if kind == BackgroundTaskKind::WorkflowExecution {
            self.run_state = "running".to_string();
        }

        self.push_event(entry);
        self.background.push(BackgroundTask {
            kind,
            handle: tokio::spawn(future),
        });
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
        let (index, status, active_event) = {
            let Some(active) = self.active_event.as_mut() else {
                return false;
            };
            if active.index + 1 != self.event_log.len() || !active.accepts(event) {
                return false;
            }
            active.merge(event);
            let status = active_event_status_text(&active.event);
            (active.index, status, active.event.clone())
        };

        self.status = status;
        self.event_log[index] = TranscriptEntry::Workflow(active_event);
        self.clear_transcript_selection();
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
                request_topic,
            } => {
                match request_topic {
                    Some(topic) => {
                        self.current_topic_run_id = Some(event.run_id.clone());
                        self.current_run_topic = Some(topic.clone());
                    }
                    None if self.current_topic_run_id.as_deref() == Some(event.run_id.as_str()) => {
                    }
                    None => {
                        self.current_topic_run_id = None;
                        self.current_run_topic = None;
                    }
                }
                self.workflow_name = Some(workflow_name.clone());
                self.current_step = Some(current_step.clone());
                self.run_state = "running".to_string();
                self.durable_run_status = Some(RunStatusState::Running);
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::StepStarted {
                step_id: current_step,
            } => {
                self.current_step = Some(current_step.clone());
                self.run_state = "running".to_string();
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::AgentPromptWindowOpened {
                step_id, window_id, ..
            } => {
                self.current_step = Some(step_id.clone());
                self.run_state = "running".to_string();
                self.durable_run_status = Some(RunStatusState::Running);
                self.agent_prompt_window = Some(AgentPromptWindowState {
                    run_id: event.run_id.clone(),
                    step_id: step_id.clone(),
                    window_id: window_id.clone(),
                });
            }
            WorkflowEventKind::AgentPromptWindowClosed { window_id, .. } => {
                if self
                    .agent_prompt_window
                    .as_ref()
                    .is_some_and(|window| window.window_id == *window_id)
                {
                    self.agent_prompt_window = None;
                }
            }
            WorkflowEventKind::StepProgress {
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
            WorkflowEventKind::WorkflowStoreWaiting { .. } => {
                self.run_state = "waiting".to_string();
            }
            WorkflowEventKind::WaitingForInput {
                step,
                prompt_id,
                message,
                choices,
            } => {
                self.current_step = Some(step.clone());
                self.run_state = "waiting".to_string();
                self.durable_run_status = Some(RunStatusState::WaitingForInput);
                self.agent_prompt_window = None;
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
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::StepRetrying { step_id, .. } => {
                self.current_step = Some(step_id.clone());
                self.run_state = "retrying".to_string();
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::ManuallyResolved { step_id, .. } => {
                self.current_step = Some(step_id.clone());
                self.run_state = "running".to_string();
                self.pending_prompt = None;
                self.durable_run_status = Some(RunStatusState::Running);
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::RunCompleted => {
                self.run_state = "completed".to_string();
                self.pending_prompt = None;
                self.durable_run_status = Some(RunStatusState::Completed);
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::RunFailed { .. } => {
                self.run_state = "failed".to_string();
                self.pending_prompt = None;
                self.durable_run_status = Some(RunStatusState::Failed);
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::RunCancelled => {
                self.run_state = "cancelled".to_string();
                self.pending_prompt = None;
                self.durable_run_status = Some(RunStatusState::Cancelled);
                self.agent_prompt_window = None;
            }
            WorkflowEventKind::RunStatusChanged { status } => {
                self.run_state = status.clone();
                self.durable_run_status = run_status_state_from_str(status);
                if self.durable_run_status != Some(RunStatusState::Running) {
                    self.agent_prompt_window = None;
                }
            }
        }
    }

    pub(super) async fn drain_background_tasks(&mut self) -> bool {
        let mut changed = false;
        let mut pending = Vec::new();
        let tasks = std::mem::take(&mut self.background);
        for task in tasks {
            if task.handle.is_finished() {
                changed = true;
                let kind = task.kind;
                match task.handle.await {
                    Ok(Ok(BackgroundTaskResult::WorkflowReport(report))) => {
                        self.apply_report(*report);
                    }
                    Ok(Ok(BackgroundTaskResult::RunsList {
                        runs,
                        partial_run_id,
                    })) => {
                        self.apply_runs_list(runs, partial_run_id);
                    }
                    Ok(Err(err)) => {
                        self.status = format!("error: {err}");
                        self.push_card("Error", [self.status.clone()]);
                        if kind == BackgroundTaskKind::WorkflowExecution {
                            self.agent_prompt_window = None;
                        }
                    }
                    Err(err) if err.is_cancelled() => {
                        self.status = "background task cancelled".to_string();
                        if kind == BackgroundTaskKind::WorkflowExecution {
                            self.run_state = "cancelled".to_string();
                            self.agent_prompt_window = None;
                        }
                        self.push_card("Cancelled", [self.status.clone()]);
                    }
                    Err(err) => {
                        self.status = format!("background task failed: {err}");
                        self.push_card("Error", [self.status.clone()]);
                        if kind == BackgroundTaskKind::WorkflowExecution {
                            self.agent_prompt_window = None;
                        }
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
        self.clear_transcript_selection();
        if self.follow_events {
            self.scroll_offset = 0;
        }
    }

    fn report_has_topic_for_run(report: &RunReport) -> bool {
        report.events.iter().any(|event| {
            event.run_id.as_str() == report.run.id.as_str()
                && matches!(
                    &event.kind,
                    WorkflowEventKind::RunStarted {
                        request_topic: Some(_),
                        ..
                    }
                )
        })
    }

    fn clear_stale_topic_for_report(&mut self, report: &RunReport) {
        if self.current_topic_run_id.as_deref() == Some(report.run.id.as_str()) {
            return;
        }

        if Self::report_has_topic_for_run(report) {
            return;
        }

        self.current_topic_run_id = None;
        self.current_run_topic = None;
    }

    fn apply_runs_list(&mut self, runs: Vec<RunSummaryLine>, partial_run_id: Option<String>) {
        self.status = format!("{} run(s)", runs.len());
        if runs.is_empty() {
            let message = match partial_run_id {
                Some(partial_run_id) => format!("matching runs for {partial_run_id}: 0"),
                None => "known runs: 0".to_string(),
            };
            self.push_card("Runs", [message]);
        } else {
            for run in runs {
                self.push_card("Run", render_run_summary_lines(&run));
            }
        }
    }

    fn apply_report(&mut self, report: RunReport) {
        self.clear_stale_topic_for_report(&report);
        self.active_run_id = Some(report.run.id.clone());
        self.workflow_name = Some(report.run.workflow_name.clone());
        self.current_step = Some(report.run.current_step.clone());
        self.run_state = format!("{:?}", report.run.status).to_ascii_lowercase();
        self.durable_run_status = Some(RunStatusDetail::from_status(&report.run.status).state);
        self.agent_prompt_window = None;
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
    use std::fs;
    use std::fs::OpenOptions;
    use std::path::Path;
    use std::process::{Child, Command};
    use std::thread;
    use std::time::{Duration, Instant};

    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind, WorkflowRuntime};

    use fs2::FileExt;
    use ratatui::style::Modifier;

    use super::*;
    use crate::app::card::DEFAULT_CARD_WIDTH;
    use crate::app::styles::style_transcript_code_fallback;
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

    #[test]
    fn status_animation_advances_only_while_running() {
        let mut state = test_state();
        assert!(!state.status_animation_active());
        assert!(!state.advance_status_animation());

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "implement".to_string(),
                request_topic: None,
            },
        ));

        let first = state.status_animation_frame();
        assert!(state.status_animation_active());
        assert!(state.advance_status_animation());
        let second = state.status_animation_frame();
        assert_ne!(first, second);
        assert!(state.advance_status_animation());
        assert_ne!(second, state.status_animation_frame());

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WaitingForInput {
                step: "confirm".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string()],
            },
        ));

        let waiting_frame = state.status_animation_frame();
        assert!(!state.status_animation_active());
        assert!(!state.advance_status_animation());
        assert_eq!(state.status_animation_frame(), first);
        assert_ne!(waiting_frame, state.status_animation_frame());
    }

    #[test]
    fn composer_hides_cursor_only_during_running_animation() {
        let mut state = test_state();
        assert!(state.composer_shows_cursor());

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "implement".to_string(),
                request_topic: None,
            },
        ));
        assert!(state.status_animation_active());
        assert!(!state.composer_shows_cursor());

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WaitingForInput {
                step: "confirm".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string()],
            },
        ));
        assert!(!state.status_animation_active());
        assert!(state.composer_shows_cursor());
    }

    #[test]
    fn app_state_uses_configured_mouse_scroll_lines() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(AppConfig {
            state_dir: dir.path().to_path_buf(),
            workflow_store: dir.path().join("workflow.redb"),
            mouse_scroll_lines: 7,
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

        assert_eq!(state.mouse_scroll_lines(), 7);
    }

    fn assert_last_entry_is_card(state: &AppState, expected_title: &str, expected_body: &str) {
        let rendered = state
            .event_entries()
            .last()
            .expect("feedback should append a transcript entry")
            .plain_text();
        assert_eq!(rendered.lines().next(), Some(expected_title), "{rendered}");
        for border in ['╭', '╮', '╰', '╯'] {
            assert!(rendered.contains(border), "{rendered}");
        }
        assert!(
            rendered.contains(&format!("│{expected_body}")),
            "{rendered}"
        );
    }

    fn spawn_pending_report_task(state: &mut AppState) {
        state.spawn_test_card_report_task("pending".to_string(), async {
            std::future::pending::<std::result::Result<RunReport, String>>().await
        });
    }

    fn runtime_with_completed_workflow(dir: &tempfile::TempDir) -> WorkflowRuntime {
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("complete.lua"),
            r#"
            local start = step("start")
            start.run = function(ctx)
              return action.status { status = "success", body = "done" }
            end

            return workflow("complete", start)
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
                    max_steps_per_run: 2,
                    max_visits_per_step: 2,
                    ..Default::default()
                },
            )]),
            ..AppConfig::default()
        };

        WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
            .with_deterministic_selector()
    }

    #[test]
    fn pending_prompt_message_renders_markdown() {
        let prompt = PendingPrompt {
            run_id: "run-1".to_string(),
            step: "confirm".to_string(),
            prompt_id: "approval".to_string(),
            message: "first line\nsecond **literal** `plan`?".to_string(),
            choices: vec!["approve".to_string()],
        };
        let lines = render_pending_prompt_lines(&prompt, DEFAULT_CARD_WIDTH);
        let rows = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        let rendered = rows.join("\n");
        let first_row = rows
            .iter()
            .position(|row| row.contains("first line"))
            .unwrap();
        let second_row = rows
            .iter()
            .position(|row| row.contains("second literal plan?"))
            .unwrap();

        assert_ne!(first_row, second_row, "{rendered}");
        assert!(rendered.contains("◔ Waiting for input · ↳ confirm · ▶ run-1"));
        assert!(rendered.contains("├─── Choices "), "{rendered}");
        assert!(rendered.contains("approve"), "{rendered}");
        assert!(!rendered.contains("**"), "{rendered}");
        assert!(lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
            span.content == "literal"
                && span.style == style_transcript_normal().add_modifier(Modifier::BOLD)
        }));
        assert!(lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
            span.content == "plan" && span.style == style_transcript_code_fallback()
        }));
    }

    #[test]
    fn application_card_details_render_markdown() {
        let entry = TranscriptEntry::Card {
            title: "Notice".to_string(),
            title_prefix: Vec::new(),
            title_suffix: Vec::new(),
            details: vec!["first line\nsecond **literal** `detail`".to_string()],
        };
        let lines = entry.render_lines_for_width(DEFAULT_CARD_WIDTH);
        let rows = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        let rendered = rows.join("\n");
        let first_row = rows
            .iter()
            .position(|row| row.contains("first line"))
            .unwrap();
        let second_row = rows
            .iter()
            .position(|row| row.contains("second literal detail"))
            .unwrap();

        assert_ne!(first_row, second_row, "{rendered}");
        assert!(rendered.contains("◔ Notice"), "{rendered}");
        assert!(rows[1].starts_with('╭'), "{rendered}");
        assert!(
            rows.last().is_some_and(|row| row.starts_with('╰')),
            "{rendered}"
        );
        assert!(!rendered.contains("**"), "{rendered}");
        assert!(lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
            span.content == "literal"
                && span.style == style_transcript_normal().add_modifier(Modifier::BOLD)
        }));
        assert!(lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
            span.content == "detail" && span.style == style_transcript_code_fallback()
        }));
    }

    #[test]
    fn submitted_background_task_cards_use_running_status() {
        for (title, suffix) in [
            ("Run", "submitted run"),
            ("Run", "submitted run --workflow slow"),
            ("Step", "submitted step"),
            ("Resume", "submitted resume"),
            ("Answer", "submitted answer"),
            ("Resolve", "submitted resolve"),
        ] {
            let entry = TranscriptEntry::Card {
                title: title.to_string(),
                title_prefix: Vec::new(),
                title_suffix: vec![suffix.to_string()],
                details: vec!["background work is pending".to_string()],
            };
            let rendered = entry.plain_text();

            assert!(rendered.starts_with(&format!("● {title}")), "{rendered}");
            assert!(rendered.contains(suffix), "{rendered}");
        }

        let run_summary = TranscriptEntry::Card {
            title: "Run".to_string(),
            title_prefix: Vec::new(),
            title_suffix: Vec::new(),
            details: vec!["run summary".to_string()],
        };
        let resolve_options = TranscriptEntry::Card {
            title: "Resolve".to_string(),
            title_prefix: Vec::new(),
            title_suffix: Vec::new(),
            details: vec!["resolve options".to_string()],
        };

        assert!(run_summary.plain_text().starts_with("◌ Run"));
        assert!(resolve_options.plain_text().starts_with("✓ Resolve"));
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
        assert_eq!(entry.matches("• Waiting on tester"), 1);
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
    fn matching_tool_call_update_replaces_call_card() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCall {
                step_id: "investigate".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Read artifact://1".to_string(),
                tool_kind: "read".to_string(),
                status: "pending".to_string(),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "investigate".to_string(),
                tool_call_id: "call_abc".to_string(),
                title: "Read artifact://1".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"text":"read complete"})),
            },
        ));

        assert_eq!(state.event_entries().len(), 1);
        let entry = &state.event_entries()[0];
        assert_eq!(entry.matches("• Read artifact://1"), 1);
        assert!(entry.contains("read complete"), "{}", entry.plain_text());
        assert!(!entry.contains("pending"), "{}", entry.plain_text());
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

    #[test]
    fn workflow_store_waiting_updates_visible_state_without_pending_prompt() {
        let mut state = test_state();
        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "start".to_string(),
                request_topic: None,
            },
        ));

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::WorkflowStoreWaiting {
                message: "Workflow store is busy; waiting for another Cowboy instance to finish a database operation.".to_string(),
            },
        ));

        assert_eq!(state.display_state(), "waiting");
        assert_eq!(state.durable_run_status, Some(RunStatusState::Running));
        assert!(state.pending_prompt().is_none());
        let rendered = state.event_entries().last().unwrap().plain_text();
        assert!(rendered.contains("Workflow store waiting"), "{rendered}");
        assert!(
            rendered.contains("Workflow store is busy; waiting"),
            "{rendered}"
        );
        assert!(!rendered.contains("/home/"), "{rendered}");
        assert!(!rendered.contains("workflow.redb"), "{rendered}");
    }

    #[test]
    fn run_started_topic_lifecycle_sets_preserves_and_clears_by_run() {
        let mut state = test_state();

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "start".to_string(),
                request_topic: Some("Add health route".to_string()),
            },
        ));
        assert_eq!(state.current_run_topic(), Some("Add health route"));

        state.apply_workflow_event(WorkflowEvent::new(
            "run-1",
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "finish".to_string(),
                request_topic: None,
            },
        ));
        assert_eq!(state.current_run_topic(), Some("Add health route"));

        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "start".to_string(),
                request_topic: None,
            },
        ));
        assert_eq!(state.current_run_topic(), None);

        state.apply_workflow_event(WorkflowEvent::new(
            "run-2",
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "start".to_string(),
                request_topic: Some("Review changes".to_string()),
            },
        ));
        state.apply_workflow_event(WorkflowEvent::new("run-2", WorkflowEventKind::RunCompleted));
        assert_eq!(state.current_run_topic(), Some("Review changes"));
    }

    #[tokio::test]
    async fn report_for_different_run_without_topic_clears_stale_topic() {
        let mut state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_with_completed_workflow(&dir);
        let run_a = runtime.start_run("first").await.unwrap().run.id;
        let run_b = runtime.start_run("second").await.unwrap().run.id;
        state.apply_workflow_event(WorkflowEvent::new(
            run_a.clone(),
            WorkflowEventKind::RunStarted {
                workflow_name: "default".to_string(),
                current_step: "start".to_string(),
                request_topic: Some("Add health route".to_string()),
            },
        ));

        let same_run_report = runtime.resume_run(&run_a).await.unwrap();
        assert!(same_run_report.events.is_empty());
        state.apply_report(same_run_report);
        assert_eq!(state.current_run_topic(), Some("Add health route"));

        let different_run_report = runtime.resume_run(&run_b).await.unwrap();
        assert!(different_run_report.events.is_empty());
        state.apply_report(different_run_report);

        assert_eq!(state.active_run_id(), Some(run_b.as_str()));
        assert_eq!(state.current_run_topic(), None);
        assert_eq!(crate::app::controls::header::text(&state, 120), "Cowboy");
    }

    #[tokio::test]
    async fn background_task_drain_returns_false_when_no_task_finished() {
        let mut state = test_state();

        assert!(!state.drain_background_tasks().await);

        state.spawn_test_card_report_task("pending".to_string(), std::future::pending());

        assert!(!state.drain_background_tasks().await);
        assert_eq!(state.background_task_count(), 1);
    }

    #[tokio::test]
    async fn composer_edit_and_submit_gates_track_background_prompt_and_terminal_states() {
        let mut state = test_state();
        assert!(state.composer_accepts_edits());
        assert!(state.composer_accepts_submit());

        spawn_pending_report_task(&mut state);
        assert!(state.composer_accepts_edits());
        assert!(!state.composer_accepts_submit());

        state.cancel_background_tasks();
        assert_eq!(state.background_task_count(), 0);
        assert!(state.composer_accepts_edits());
        assert!(state.composer_accepts_submit());

        let mut waiting_state = test_state();
        spawn_pending_report_task(&mut waiting_state);
        assert!(waiting_state.composer_accepts_edits());
        assert!(!waiting_state.composer_accepts_submit());

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
        assert!(waiting_state.composer_accepts_edits());
        assert!(waiting_state.composer_accepts_submit());

        waiting_state.cancel_background_tasks();
        assert_eq!(waiting_state.background_task_count(), 0);
        assert!(waiting_state.composer_accepts_submit());

        let mut drained_state = test_state();
        drained_state
            .spawn_test_card_report_task("finished".to_string(), async { Err("boom".to_string()) });
        tokio::task::yield_now().await;
        assert!(drained_state.drain_background_tasks().await);
        assert_eq!(drained_state.background_task_count(), 0);
        assert!(drained_state.composer_accepts_submit());

        for kind in [
            WorkflowEventKind::RunCompleted,
            WorkflowEventKind::RunFailed {
                reason: "boom".to_string(),
            },
            WorkflowEventKind::RunCancelled,
        ] {
            let mut terminal_state = test_state();
            terminal_state.apply_workflow_event(WorkflowEvent::new("run-1", kind));
            assert_eq!(terminal_state.background_task_count(), 0);
            assert!(terminal_state.composer_accepts_edits());
            assert!(terminal_state.composer_accepts_submit());
        }
    }

    #[tokio::test]
    async fn workflow_background_task_records_running_card() {
        let mut pending_state = test_state();
        pending_state.spawn_card_report_task(
            "Run",
            ["00:00:00".to_string()],
            ["submitted run --workflow slow".to_string()],
            "submitted run --workflow slow: smoke-main-message".to_string(),
            ["smoke-main-message".to_string()],
            async { std::future::pending::<Result<RunReport, String>>().await },
        );

        assert_eq!(pending_state.background_task_count(), 1);
        assert_eq!(pending_state.event_entries().len(), 1);
        let started = pending_state.event_entries()[0].plain_text();
        assert!(started.contains("● Run"), "{started}");
        assert!(
            started.contains("submitted run --workflow slow"),
            "{started}"
        );
        assert!(started.contains("smoke-main-message"), "{started}");

        pending_state.cancel_background_tasks();

        let mut completed_state = test_state();
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_with_completed_workflow(&dir);
        completed_state.spawn_card_report_task(
            "Run",
            ["00:00:00".to_string()],
            ["submitted run".to_string()],
            "submitted run: complete".to_string(),
            ["complete".to_string()],
            async move {
                runtime
                    .start_run_with_workflow("complete", "complete")
                    .await
                    .map_err(|err| err.to_string())
            },
        );
        let initial_entry = completed_state.event_entries()[0].plain_text();

        tokio::time::timeout(Duration::from_secs(2), async {
            while completed_state.background_task_count() > 0 {
                tokio::task::yield_now().await;
                completed_state.drain_background_tasks().await;
            }
        })
        .await
        .expect("completed workflow task should drain");

        assert_eq!(completed_state.background_task_count(), 0);
        assert!(completed_state.event_entries().len() > 1);
        assert_eq!(
            completed_state.event_entries()[0].plain_text(),
            initial_entry
        );
        assert!(
            completed_state
                .event_entries()
                .iter()
                .any(|entry| entry.contains("Run completed")),
            "{:?}",
            completed_state.event_entries()
        );
    }

    #[tokio::test]
    async fn runs_list_background_task_records_loading_card() {
        let mut pending_state = test_state();
        pending_state.spawn_runs_list_task("loading runs".to_string(), None, async {
            std::future::pending::<Result<Vec<RunSummaryLine>, String>>().await
        });

        assert_eq!(pending_state.background_task_count(), 1);
        assert_eq!(pending_state.event_entries().len(), 1);
        let started = pending_state.event_entries()[0].plain_text();
        assert!(started.contains("● Runs"), "{started}");
        assert!(started.contains("loading runs"), "{started}");
        assert!(started.contains("Loading runs"), "{started}");

        pending_state.cancel_background_tasks();

        let mut completed_state = test_state();
        completed_state
            .spawn_runs_list_task("loading runs".to_string(), None, async { Ok(Vec::new()) });
        let initial_entry = completed_state.event_entries()[0].plain_text();
        tokio::task::yield_now().await;
        assert!(completed_state.drain_background_tasks().await);

        assert_eq!(completed_state.background_task_count(), 0);
        assert_eq!(
            completed_state.event_entries()[0].plain_text(),
            initial_entry
        );
        assert!(
            completed_state
                .event_entries()
                .last()
                .is_some_and(|entry| entry.contains("known runs: 0"))
        );
    }

    #[tokio::test]
    async fn runtime_error_completion_renders_error_card() {
        let mut state = test_state();
        state
            .spawn_test_card_report_task("finished".to_string(), async { Err("boom".to_string()) });
        tokio::task::yield_now().await;

        assert!(state.drain_background_tasks().await);

        assert_eq!(state.background_task_count(), 0);
        assert_eq!(state.status(), "error: boom");
        assert_eq!(state.display_state(), "running");
        assert_last_entry_is_card(&state, "✗ Error", "error: boom");
    }

    #[tokio::test]
    async fn cancelled_join_renders_cancelled_card_and_state() {
        let mut state = test_state();
        state.spawn_test_card_report_task("pending".to_string(), async {
            std::future::pending::<Result<RunReport, String>>().await
        });
        state.background[0].handle.abort();
        tokio::task::yield_now().await;

        assert!(state.drain_background_tasks().await);

        assert_eq!(state.background_task_count(), 0);
        assert_eq!(state.status(), "background task cancelled");
        assert_eq!(state.display_state(), "cancelled");
        assert_last_entry_is_card(&state, "■ Cancelled", "background task cancelled");
    }

    #[tokio::test]
    async fn failed_join_renders_error_card_without_changing_run_state() {
        let mut state = test_state();
        state.spawn_test_card_report_task("panicking".to_string(), async {
            if true {
                panic!("join boom");
            }

            Err("unreachable".to_string())
        });
        tokio::task::yield_now().await;

        assert!(state.drain_background_tasks().await);

        assert_eq!(state.background_task_count(), 0);
        assert!(
            state.status().starts_with("background task failed: "),
            "{}",
            state.status()
        );
        assert_eq!(state.display_state(), "running");
        let status = state.status().to_string();
        assert_last_entry_is_card(&state, "✗ Error", &status);
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
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        FileExt::lock_exclusive(&lock_file).unwrap();
        fs::write(ready_path, "ready").unwrap();
        thread::sleep(Duration::from_secs(10));
    }
}
