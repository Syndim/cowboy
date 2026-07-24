#[cfg(test)]
pub(crate) mod tests {
    use chrono::Utc;
    use cowboy_workflow_core::{
        ConfigSetRef, RoleSession, RunStatus, StepDetail, StepInput, StepOutput, StepRecord,
        TurnRecord, WorkflowRun, WorkflowSourceSnapshot,
    };
    use serde_json::Value;

    use crate::{Error, SqliteWorkflowStore};

    pub(crate) fn run(id: &str) -> WorkflowRun {
        let now = Utc::now();
        WorkflowRun {
            id: id.into(),
            workflow_name: "wf".into(),
            workflow_api_version: 1,
            workflow_hash: "source-hash".into(),
            workflow_sources: Default::default(),
            original_request: "do it".into(),
            request_topic: Some("topic".into()),
            config_set: ConfigSetRef {
                name: "careful".into(),
            },
            status: RunStatus::Running,
            retries_used: 2,
            step_retries_used: [("start".into(), 1)].into_iter().collect(),
            current_step: "start".into(),
            head: None,
            resume: Value::Null,
            steps_executed: 1,
            step_visits: [("start".into(), 1)].into_iter().collect(),
            active_duration_ms: 42,
            created_at: now,
            updated_at: now,
        }
    }

    pub(crate) fn record(id: &str) -> StepRecord {
        let now = Utc::now();
        StepRecord {
            id: id.into(),
            prev: None,
            step: "start".into(),
            action: "status".into(),
            input: StepInput {
                prompt: None,
                context: Value::Null,
            },
            output: Some(StepOutput {
                status: "success".into(),
                fields: Value::Null,
                body: String::new(),
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
        }
    }

    #[tokio::test]
    async fn run_head_round_trip_and_deterministic_listing() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let run_b = run("run-b");
        let run_a = run("run-a");
        store.save_run(&run_b).await.unwrap();
        store.save_run(&run_a).await.unwrap();
        assert_eq!(store.load_run("run-a").await.unwrap(), run_a);
        assert_eq!(
            store.load_run_head("run-a").await.unwrap(),
            cowboy_workflow_core::RunHead::from_run(&run_a)
        );
        assert_eq!(
            store
                .list_runs()
                .await
                .unwrap()
                .into_iter()
                .map(|head| head.run_id)
                .collect::<Vec<_>>(),
            vec!["run-a", "run-b"]
        );
        assert_eq!(
            store.load_run("run-a").await.unwrap().config_set.name,
            "careful"
        );
        println!("EVIDENCE contract-run-head round_trip=true deterministic=true");
    }

    #[tokio::test]
    async fn source_step_hashes_and_reopen_durability() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let store = SqliteWorkflowStore::connect(&path).await.unwrap();
        let source = WorkflowSourceSnapshot {
            root: None,
            entry: "main.lua".into(),
            files: [("main.lua".into(), "return workflow('x', step('s'))".into())]
                .into_iter()
                .collect(),
        };
        let record = record("record-1");
        let source_hash = store.store_workflow_source_snapshot(&source).await.unwrap();
        let step_hash = store.store_step_record(&record).await.unwrap();
        assert_eq!(
            store.store_workflow_source_snapshot(&source).await.unwrap(),
            source_hash
        );
        store.close().await;
        let reopened = SqliteWorkflowStore::connect(&path).await.unwrap();
        assert_eq!(
            reopened
                .load_workflow_source_snapshot(&source_hash)
                .await
                .unwrap(),
            source
        );
        assert_eq!(reopened.load_step_record(&step_hash).await.unwrap(), record);
        println!("EVIDENCE contract-objects source_hash=true step_hash=true reopen=true");
    }

    #[tokio::test]
    async fn role_session_and_turn_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let session = RoleSession {
            run_id: "run-1".into(),
            role_id: "developer".into(),
            backend: "acp".into(),
            session_id: "session-1".into(),
            updated_at: Utc::now(),
            role_instructions_sent: true,
            last_sent_input_sequence: Some(3),
        };
        store.save_role_session(session.clone()).await.unwrap();
        assert_eq!(
            store.load_role_session("run-1", "developer").await.unwrap(),
            Some(session)
        );
        for index in 1..=2 {
            store
                .append_turn(
                    "run-1",
                    TurnRecord {
                        id: format!("turn-{index}"),
                        step_id: "record-1".into(),
                        role: "assistant".into(),
                        content: format!("content-{index}"),
                        timestamp: Utc::now(),
                        prev: None,
                    },
                )
                .await
                .unwrap();
        }
        assert_eq!(
            store
                .load_turns("run-1", "record-1")
                .await
                .unwrap()
                .into_iter()
                .map(|turn| turn.id)
                .collect::<Vec<_>>(),
            vec!["turn-1", "turn-2"]
        );
        store.delete_role_sessions("run-1").await.unwrap();
        assert_eq!(
            store.load_role_session("run-1", "developer").await.unwrap(),
            None
        );
        println!("EVIDENCE contract-agent session_crud=true turn_order=true");
    }

    #[tokio::test]
    async fn run_deletion_retains_objects_and_low_level_delete_removes_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteWorkflowStore::connect(dir.path().join("data.db"))
            .await
            .unwrap();
        let run = run("run-1");
        let record = record("record-1");
        store.save_run(&run).await.unwrap();
        let hash = store.store_step_record(&record).await.unwrap();
        store.delete_run(&run.id).await.unwrap();
        assert!(matches!(
            store.load_run(&run.id).await,
            Err(Error::RunNotFound(_))
        ));
        assert_eq!(store.load_step_record(&hash).await.unwrap(), record);
        store.delete_object(&hash).await.unwrap();
        assert!(matches!(
            store.load_step_record(&hash).await,
            Err(Error::ObjectNotFound(_))
        ));
        println!(
            "EVIDENCE contract-delete mutable_removed=true immutable_retained=true explicit_delete=true"
        );
    }
}
