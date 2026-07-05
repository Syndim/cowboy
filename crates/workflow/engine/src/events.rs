use chrono::{DateTime, Utc};
use cowboy_workflow_core::{RunStatus, StepRecord, WorkflowRun};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

/// UI-facing workflow event with a stable run id and timestamps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowEvent {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub run_started_at: Option<DateTime<Utc>>,
    pub kind: WorkflowEventKind,
}

impl WorkflowEvent {
    pub fn new(run_id: impl Into<String>, kind: WorkflowEventKind) -> Self {
        Self {
            run_id: run_id.into(),
            timestamp: Utc::now(),
            run_started_at: None,
            kind,
        }
    }

    pub fn for_run(run: &WorkflowRun, kind: WorkflowEventKind) -> Self {
        Self::with_run_started_at(run.id.clone(), run.created_at, kind)
    }

    pub fn with_run_started_at(
        run_id: impl Into<String>,
        run_started_at: DateTime<Utc>,
        kind: WorkflowEventKind,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            timestamp: Utc::now(),
            run_started_at: Some(run_started_at),
            kind,
        }
    }

    pub fn run_started(run: &WorkflowRun) -> Self {
        Self::for_run(
            run,
            WorkflowEventKind::RunStarted {
                workflow_name: run.workflow_name.clone(),
                current_step: run.current_step.clone(),
            },
        )
    }

    pub fn run_status(run_id: impl Into<String>, status: &RunStatus) -> Self {
        Self::new(run_id, WorkflowEventKind::from(status))
    }

    pub fn run_status_for_run(run: &WorkflowRun, status: &RunStatus) -> Self {
        Self::for_run(run, WorkflowEventKind::from(status))
    }

    pub fn step_completed(run_id: impl Into<String>, record: &StepRecord) -> Self {
        Self::new(run_id, Self::step_completed_kind(record))
    }

    pub fn step_completed_for_run(run: &WorkflowRun, record: &StepRecord) -> Self {
        Self::for_run(run, Self::step_completed_kind(record))
    }

    fn step_completed_kind(record: &StepRecord) -> WorkflowEventKind {
        let output = record.output.as_ref();
        WorkflowEventKind::StepCompleted {
            step_id: record.step.clone(),
            action: record.action.clone(),
            status: output.map(|output| output.status.clone()),
            body: output.map(|output| output.body.clone()).unwrap_or_default(),
        }
    }
}

/// Events the terminal renderer understands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowEventKind {
    RunStarted {
        workflow_name: String,
        current_step: String,
    },
    StepStarted {
        step_id: String,
    },
    StepProgress {
        step_id: String,
        message: String,
    },
    AgentSessionReady {
        step_id: String,
        role: String,
        session_id: String,
    },
    AgentPrompt {
        step_id: String,
        role: String,
        session_id: String,
        prompt: String,
    },
    AgentResponse {
        step_id: String,
        content: String,
    },
    AgentThought {
        step_id: String,
        content: String,
    },
    AgentToolCall {
        step_id: String,
        tool_call_id: String,
        title: String,
        tool_kind: String,
        status: String,
    },
    AgentToolCallUpdate {
        step_id: String,
        tool_call_id: String,
        title: String,
        status: String,
        content: Option<Value>,
    },
    AgentPlan {
        step_id: String,
        entries: Vec<Value>,
    },
    StepCompleted {
        step_id: String,
        action: String,
        status: Option<String>,
        body: String,
    },
    WaitingForInput {
        step: String,
        prompt_id: String,
        message: String,
        choices: Vec<String>,
    },
    RunCompleted,
    RunFailed {
        reason: String,
    },
    RunCancelled,
    RunStatusChanged {
        status: String,
    },
}

impl From<&RunStatus> for WorkflowEventKind {
    fn from(status: &RunStatus) -> Self {
        match status {
            RunStatus::Running => Self::RunStatusChanged {
                status: "running".to_string(),
            },
            RunStatus::WaitingForInput {
                step,
                prompt_id,
                message,
                choices,
                ..
            } => Self::WaitingForInput {
                step: step.clone(),
                prompt_id: prompt_id.clone(),
                message: message.clone(),
                choices: choices.clone(),
            },
            RunStatus::Completed => Self::RunCompleted,
            RunStatus::Failed { reason } => Self::RunFailed {
                reason: reason.clone(),
            },
            RunStatus::Cancelled => Self::RunCancelled,
        }
    }
}

/// Broadcast workflow events to the TUI and future session loggers.
#[derive(Debug)]
pub struct EventBus {
    sender: broadcast::Sender<WorkflowEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn emit(&self, event: WorkflowEvent) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkflowEvent> {
        self.sender.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(8192)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use cowboy_workflow_core::{StepDetail, StepInput, StepOutput};
    use serde_json::Value;

    #[tokio::test]
    async fn event_bus_broadcasts_workflow_events() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        bus.emit(WorkflowEvent::new("run-1", WorkflowEventKind::RunCompleted));

        let event = rx.recv().await.unwrap();
        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.kind, WorkflowEventKind::RunCompleted);
    }

    #[test]
    fn maps_waiting_status_to_input_event() {
        let event = WorkflowEvent::run_status(
            "run-1",
            &RunStatus::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
                resume_callback: cowboy_workflow_core::ResumeCallback::new(
                    "ask_user",
                    serde_json::json!({ "secret": "internal" }),
                )
                .unwrap(),
            },
        );

        assert_eq!(
            event.kind,
            WorkflowEventKind::WaitingForInput {
                step: "approve".to_string(),
                prompt_id: "approval".to_string(),
                message: "Approve?".to_string(),
                choices: vec!["yes".to_string(), "no".to_string()],
            }
        );
    }

    #[test]
    fn maps_step_record_to_completed_event() {
        let now = Utc::now();
        let record = StepRecord {
            id: "record-1".to_string(),
            prev: None,
            step: "implement".to_string(),
            action: "status".to_string(),
            input: StepInput {
                prompt: None,
                context: Value::Null,
            },
            output: Some(StepOutput {
                status: "success".to_string(),
                fields: Value::Null,
                body: "done".to_string(),
                raw: Value::Null,
            }),
            detail: StepDetail {
                backend: None,
                session_id: None,
                duration_ms: 0,
                turn_count: 0,
                usage: None,
            },
            started_at: now,
            completed_at: Some(now),
        };

        let event = WorkflowEvent::step_completed("run-1", &record);
        assert_eq!(
            event.kind,
            WorkflowEventKind::StepCompleted {
                step_id: "implement".to_string(),
                action: "status".to_string(),
                status: Some("success".to_string()),
                body: "done".to_string(),
            }
        );
    }

    #[test]
    fn workflow_event_round_trips_with_run_started_at() {
        let run_started_at = Utc.with_ymd_and_hms(2026, 7, 5, 12, 30, 0).unwrap();
        let timestamp = Utc.with_ymd_and_hms(2026, 7, 5, 12, 34, 56).unwrap();
        let mut event = WorkflowEvent::with_run_started_at(
            "run-1",
            run_started_at,
            WorkflowEventKind::RunCompleted,
        );
        event.timestamp = timestamp;

        let raw = serde_json::to_string(&event).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["run_started_at"], "2026-07-05T12:30:00Z");

        let reparsed: WorkflowEvent = serde_json::from_str(&raw).unwrap();
        assert_eq!(reparsed, event);
    }

    #[test]
    fn legacy_workflow_event_json_defaults_missing_run_started_at() {
        let raw = serde_json::json!({
            "run_id": "run-1",
            "timestamp": "2026-07-05T12:34:56Z",
            "kind": { "kind": "run_completed" },
        })
        .to_string();

        let event: WorkflowEvent = serde_json::from_str(&raw).unwrap();

        assert_eq!(event.run_id, "run-1");
        assert_eq!(event.run_started_at, None);
        assert_eq!(event.kind, WorkflowEventKind::RunCompleted);
    }

    #[test]
    fn agent_workflow_event_variants_round_trip_json() {
        let variants = vec![
            WorkflowEventKind::AgentSessionReady {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                session_id: "session-1".to_string(),
            },
            WorkflowEventKind::AgentPrompt {
                step_id: "implement".to_string(),
                role: "developer".to_string(),
                session_id: "session-1".to_string(),
                prompt: "Do work".to_string(),
            },
            WorkflowEventKind::AgentResponse {
                step_id: "implement".to_string(),
                content: "answer".to_string(),
            },
            WorkflowEventKind::AgentThought {
                step_id: "implement".to_string(),
                content: "thinking".to_string(),
            },
            WorkflowEventKind::AgentToolCall {
                step_id: "implement".to_string(),
                tool_call_id: "call_1".to_string(),
                title: "Read file".to_string(),
                tool_kind: "read".to_string(),
                status: "pending".to_string(),
            },
            WorkflowEventKind::AgentToolCallUpdate {
                step_id: "implement".to_string(),
                tool_call_id: "call_1".to_string(),
                title: "Read file".to_string(),
                status: "completed".to_string(),
                content: Some(serde_json::json!({"text":"done"})),
            },
            WorkflowEventKind::AgentPlan {
                step_id: "implement".to_string(),
                entries: vec![serde_json::json!({"content":"first"})],
            },
        ];

        for variant in variants {
            let raw = serde_json::to_string(&variant).unwrap();
            let reparsed: WorkflowEventKind = serde_json::from_str(&raw).unwrap();
            assert_eq!(reparsed, variant);
        }
    }
}
