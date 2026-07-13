use std::env;
use std::path::PathBuf;

use chrono::Utc;
use cowboy_workflow_core::{
    ObjectKind, RunHead, RunStatus, StepDetail, StepInput, StepOutput, StepRecord, TurnRecord,
    WorkflowRun, WorkflowSourceSnapshot,
};
use cowboy_workflow_store::RedbRunStore;
use serde_json::Value;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let Some(db) = args.next() else {
        usage();
    };
    let Some(command) = args.next() else {
        usage();
    };
    let rest = args.collect::<Vec<_>>();
    let store = open_store(db)?;

    match command.as_str() {
        "save-run" => {
            require_len(&rest, 4)?;
            let run = sample_run(&rest[0], &rest[1], &rest[2], &rest[3]);
            store.save_run(&run)?;
            println!("saved run {}", run.id);
        }
        "load-run" => {
            require_len(&rest, 1)?;
            let run = store.load_run(&rest[0])?;
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
        "list-runs" => {
            require_len(&rest, 0)?;
            for head in store.list_runs()? {
                println!("{}", serde_json::to_string_pretty(&head)?);
            }
        }
        "put-step" => {
            require_len(&rest, 3)?;
            let record = sample_step(&rest[0], &rest[1], &rest[2]);
            let hash = store.put_object(ObjectKind::StepRecord, &record)?;
            println!("{hash}");
        }
        "get-step" => {
            require_len(&rest, 1)?;
            let record: StepRecord = store.get_object(&rest[0])?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        "put-source" => {
            require_len(&rest, 2)?;
            let bundle = WorkflowSourceSnapshot {
                root: None,
                entry: rest[0].clone(),
                files: [(rest[0].clone(), rest[1].clone())].into_iter().collect(),
            };
            let hash = store.put_object(ObjectKind::WorkflowSourceSnapshot, &bundle)?;
            println!("{hash}");
        }
        "save-head" => {
            require_len(&rest, 4)?;
            let head = RunHead {
                run_id: rest[0].clone(),
                workflow_hash: rest[1].clone(),
                head_step: none_if_dash(&rest[2]),
                status: parse_status(&rest[3]),
                updated_at: Utc::now(),
            };
            store.update_run_head(&head.run_id, head.clone())?;
            println!("saved head {}", head.run_id);
        }
        "load-head" => {
            require_len(&rest, 1)?;
            let head = store.load_run_head(&rest[0])?;
            println!("{}", serde_json::to_string_pretty(&head)?);
        }
        "append-turn" => {
            require_len(&rest, 4)?;
            let turn = TurnRecord {
                id: rest[2].clone(),
                step_id: rest[1].clone(),
                role: "assistant".to_string(),
                content: rest[3].clone(),
                timestamp: Utc::now(),
                prev: None,
            };
            let hash = store.append_turn(&rest[0], turn)?;
            println!("{hash}");
        }
        "delete-run" => {
            require_len(&rest, 1)?;
            store.delete_run(&rest[0])?;
            println!("deleted run {}", rest[0]);
        }
        "delete-object" => {
            require_len(&rest, 1)?;
            store.delete_object(&rest[0])?;
            println!("deleted object {}", rest[0]);
        }
        _ => usage(),
    }
    Ok(())
}

fn open_store(path: String) -> Result<RedbRunStore, Box<dyn std::error::Error>> {
    let path = PathBuf::from(path);
    if path.exists() {
        Ok(RedbRunStore::open(path)?)
    } else {
        Ok(RedbRunStore::create(path)?)
    }
}

fn sample_run(id: &str, workflow: &str, workflow_hash: &str, current_step: &str) -> WorkflowRun {
    let now = Utc::now();
    WorkflowRun {
        id: id.to_string(),
        workflow_name: workflow.to_string(),
        workflow_api_version: 1,
        workflow_hash: workflow_hash.to_string(),
        workflow_sources: Default::default(),
        original_request: "manual store-cli run".to_string(),
        request_topic: None,
        config_set: Default::default(),
        status: RunStatus::Running,
        retries_used: 0,
        step_retries_used: Default::default(),
        current_step: current_step.to_string(),
        head: None,
        resume: Value::Null,
        steps_executed: 0,
        step_visits: Default::default(),
        active_duration_ms: 0,
        created_at: now,
        updated_at: now,
    }
}

fn sample_step(id: &str, step: &str, status: &str) -> StepRecord {
    let now = Utc::now();
    StepRecord {
        id: id.to_string(),
        prev: None,
        step: step.to_string(),
        action: "status".to_string(),
        input: StepInput {
            prompt: None,
            context: Value::Null,
        },
        output: Some(StepOutput {
            status: status.to_string(),
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

fn parse_status(status: &str) -> RunStatus {
    match status {
        "running" => RunStatus::Running,
        "completed" => RunStatus::Completed,
        "cancelled" => RunStatus::Cancelled,
        value if value.starts_with("failed:") => RunStatus::Failed {
            reason: value.trim_start_matches("failed:").to_string(),
        },
        other => RunStatus::Failed {
            reason: format!("unknown status argument: {other}"),
        },
    }
}

fn none_if_dash(value: &str) -> Option<String> {
    if value == "-" {
        None
    } else {
        Some(value.to_string())
    }
}

fn require_len(args: &[String], expected: usize) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != expected {
        usage();
    }
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  store-cli <db> save-run <run-id> <workflow> <workflow-hash> <current-step>\n  store-cli <db> load-run <run-id>\n  store-cli <db> list-runs\n  store-cli <db> put-step <record-id> <step-id> <status>\n  store-cli <db> get-step <hash>\n  store-cli <db> put-source <entry> <source>\n  store-cli <db> save-head <run-id> <workflow-hash> <head-hash|-> <running|completed|cancelled|failed:reason>\n  store-cli <db> load-head <run-id>\n  store-cli <db> append-turn <run-id> <step-record-id> <turn-id> <content>\n  store-cli <db> delete-run <run-id>\n  store-cli <db> delete-object <hash>"
    );
    std::process::exit(2)
}
