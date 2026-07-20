use chrono::{DateTime, Utc};
use cowboy_workflow_core::{Result, RunStore, StepRecord, WorkflowRun};

use crate::events::{WorkflowEvent, WorkflowEventKind};

/// Persisted active-runtime stopwatch for one workflow run operation.
#[derive(Debug, Clone)]
pub(crate) struct ActiveRunClock {
    run_started_at: DateTime<Utc>,
    base_active_duration_ms: u64,
    active_window_started_at: DateTime<Utc>,
}

impl ActiveRunClock {
    pub(crate) fn open(run: &WorkflowRun) -> Self {
        Self::open_at(run, Utc::now())
    }

    pub(crate) fn open_at(run: &WorkflowRun, active_window_started_at: DateTime<Utc>) -> Self {
        Self {
            run_started_at: run.created_at,
            base_active_duration_ms: run.active_duration_ms,
            active_window_started_at,
        }
    }

    pub(crate) fn event(
        &self,
        run_id: impl Into<String>,
        kind: WorkflowEventKind,
    ) -> WorkflowEvent {
        self.event_at(run_id, Utc::now(), kind)
    }

    pub(crate) fn event_for_run(
        &self,
        run: &WorkflowRun,
        kind: WorkflowEventKind,
    ) -> WorkflowEvent {
        self.event(run.id.clone(), kind)
    }

    pub(crate) fn run_started_with_topic(
        &self,
        run: &WorkflowRun,
        request_topic: Option<String>,
    ) -> WorkflowEvent {
        self.event_for_run(
            run,
            WorkflowEventKind::RunStarted {
                workflow_name: run.workflow_name.clone(),
                current_step: run.current_step.clone(),
                request_topic,
            },
        )
    }

    pub(crate) fn run_status_for_run(
        &self,
        run: &WorkflowRun,
        status: &cowboy_workflow_core::RunStatus,
    ) -> WorkflowEvent {
        self.event_for_run(run, WorkflowEventKind::from(status))
    }

    pub(crate) fn step_completed_for_run(
        &self,
        run: &WorkflowRun,
        record: &StepRecord,
    ) -> WorkflowEvent {
        self.event_for_run(run, WorkflowEvent::step_completed_kind(record))
    }

    pub(crate) fn event_at(
        &self,
        run_id: impl Into<String>,
        timestamp: DateTime<Utc>,
        kind: WorkflowEventKind,
    ) -> WorkflowEvent {
        WorkflowEvent::with_timing(
            run_id,
            timestamp,
            Some(self.run_started_at),
            Some(self.active_duration_at(timestamp)),
            kind,
        )
    }

    pub(crate) fn active_duration_at(&self, timestamp: DateTime<Utc>) -> u64 {
        self.base_active_duration_ms
            .saturating_add(nonnegative_milliseconds_since(
                self.active_window_started_at,
                timestamp,
            ))
    }

    pub(crate) fn close<S: RunStore>(&self, store: &S, run: &mut WorkflowRun) -> Result<()> {
        self.close_at(store, run, Utc::now())
    }

    pub(crate) fn close_at<S: RunStore>(
        &self,
        store: &S,
        run: &mut WorkflowRun,
        timestamp: DateTime<Utc>,
    ) -> Result<()> {
        run.active_duration_ms = self.active_duration_at(timestamp);
        store.save_run(run)?;
        store.update_run_head(&run.id, cowboy_workflow_core::RunHead::from_run(run))?;
        Ok(())
    }
}

fn nonnegative_milliseconds_since(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    let elapsed_ms = end.signed_duration_since(start).num_milliseconds();
    u64::try_from(elapsed_ms).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use chrono::{Duration, TimeZone};
    use cowboy_workflow_core::RunStatus;
    use serde_json::Value;

    #[test]
    fn event_at_uses_supplied_timestamp_for_event_and_active_elapsed() {
        let run_started_at = Utc.with_ymd_and_hms(2026, 7, 5, 12, 0, 0).unwrap();
        let active_window_started_at = Utc.with_ymd_and_hms(2026, 7, 5, 12, 10, 0).unwrap();
        let timestamp = active_window_started_at + Duration::milliseconds(2_500);
        let clock = ActiveRunClock::open_at(&run(run_started_at, 1_200), active_window_started_at);

        let event = clock.event_at("run-1", timestamp, WorkflowEventKind::RunCompleted);

        assert_eq!(event.timestamp, timestamp);
        assert_eq!(event.run_started_at, Some(run_started_at));
        assert_eq!(event.run_active_duration_ms, Some(3_700));
        assert_eq!(event.kind, WorkflowEventKind::RunCompleted);
    }

    #[test]
    fn event_at_keeps_base_duration_when_timestamp_precedes_active_window() {
        let run_started_at = Utc.with_ymd_and_hms(2026, 7, 5, 12, 0, 0).unwrap();
        let active_window_started_at = Utc.with_ymd_and_hms(2026, 7, 5, 12, 10, 0).unwrap();
        let timestamp = active_window_started_at - Duration::milliseconds(1);
        let clock = ActiveRunClock::open_at(&run(run_started_at, 9_000), active_window_started_at);

        let event = clock.event_at("run-1", timestamp, WorkflowEventKind::RunCompleted);

        assert_eq!(event.timestamp, timestamp);
        assert_eq!(event.run_active_duration_ms, Some(9_000));
    }

    #[test]
    fn event_at_saturates_cumulative_active_duration() {
        let run_started_at = Utc.with_ymd_and_hms(2026, 7, 5, 12, 0, 0).unwrap();
        let active_window_started_at = Utc.with_ymd_and_hms(2026, 7, 5, 12, 10, 0).unwrap();
        let timestamp = active_window_started_at + Duration::milliseconds(10);
        let clock =
            ActiveRunClock::open_at(&run(run_started_at, u64::MAX - 5), active_window_started_at);

        let event = clock.event_at("run-1", timestamp, WorkflowEventKind::RunCompleted);

        assert_eq!(event.run_active_duration_ms, Some(u64::MAX));
    }

    fn run(created_at: DateTime<Utc>, active_duration_ms: u64) -> WorkflowRun {
        WorkflowRun {
            id: "run-1".to_string(),
            workflow_name: "wf".to_string(),
            workflow_api_version: 1,
            workflow_hash: "source".to_string(),
            workflow_sources: BTreeMap::new(),
            original_request: "do it".to_string(),
            request_topic: None,
            config_set: Default::default(),
            status: RunStatus::Running,
            retries_used: 0,
            step_retries_used: Default::default(),
            current_step: "start".to_string(),
            head: None,
            resume: Value::Null,
            steps_executed: 0,
            step_visits: BTreeMap::new(),
            active_duration_ms,
            created_at,
            updated_at: created_at,
        }
    }
}
