use chrono::{DateTime, Utc};
use cowboy_workflow_core::{RunStatus, StepRecord, WorkflowRun};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

/// UI-facing workflow event with a stable run id and timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowEvent {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub kind: WorkflowEventKind,
}

impl WorkflowEvent {
    pub fn new(run_id: impl Into<String>, kind: WorkflowEventKind) -> Self {
        Self {
            run_id: run_id.into(),
            timestamp: Utc::now(),
            kind,
        }
    }

    pub fn run_started(run: &WorkflowRun) -> Self {
        Self::new(
            run.id.clone(),
            WorkflowEventKind::RunStarted {
                workflow_name: run.workflow_name.clone(),
                current_step: run.current_step.clone(),
            },
        )
    }

    pub fn run_status(run_id: impl Into<String>, status: &RunStatus) -> Self {
        Self::new(run_id, WorkflowEventKind::from(status))
    }

    pub fn step_completed(run_id: impl Into<String>, record: &StepRecord) -> Self {
        let output = record.output.as_ref();
        Self::new(
            run_id,
            WorkflowEventKind::StepCompleted {
                step_id: record.step.clone(),
                action: record.action.clone(),
                status: output.map(|output| output.status.clone()),
                body: output.map(|output| output.body.clone()).unwrap_or_default(),
            },
        )
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
    Suspended {
        step: String,
        reason: String,
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
            RunStatus::Suspended { step, reason } => Self::Suspended {
                step: step.clone(),
                reason: reason.clone(),
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
                record_id: "record-1".to_string(),
                prev: Some("prev".to_string()),
                started_at: Utc::now(),
                output_status: "answered".to_string(),
                output_fields: serde_json::json!({ "secret": "internal" }),
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
