use std::env;
use std::path::PathBuf;

use chrono::Utc;
use cowboy_workflow_core::{
    RunStatus, StepDetail, StepInput, StepOutput, StepRecord, TurnRecord, WorkflowRun,
    WorkflowSourceSnapshot,
};
use cowboy_workflow_store::SqliteWorkflowStore;
use serde_json::Value;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let Some(db) = args.next() else {
        usage();
    };
    let Some(command) = args.next() else {
        usage();
    };
    let rest = args.collect::<Vec<_>>();
    let store = open_store(db).await?;

    match command.as_str() {
        "save-run" => {
            require_len(&rest, 4)?;
            let run = sample_run(&rest[0], &rest[1], &rest[2], &rest[3]);
            store.save_run(&run).await?;
            println!("saved run {}", run.id);
        }
        "load-run" => {
            require_len(&rest, 1)?;
            let run = store.load_run(&rest[0]).await?;
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
        "list-runs" => {
            require_len(&rest, 0)?;
            for head in store.list_runs().await? {
                println!("{}", serde_json::to_string_pretty(&head)?);
            }
        }
        "put-step" => {
            require_len(&rest, 3)?;
            let record = sample_step(&rest[0], &rest[1], &rest[2]);
            let hash = store.store_step_record(&record).await?;
            println!("{hash}");
        }
        "get-step" => {
            require_len(&rest, 1)?;
            let record = store.load_step_record(&rest[0]).await?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        "put-source" => {
            require_len(&rest, 2)?;
            let bundle = WorkflowSourceSnapshot {
                root: None,
                entry: rest[0].clone(),
                files: [(rest[0].clone(), rest[1].clone())].into_iter().collect(),
            };
            let hash = store.store_workflow_source_snapshot(&bundle).await?;
            println!("{hash}");
        }
        "load-head" => {
            require_len(&rest, 1)?;
            let head = store.load_run_head(&rest[0]).await?;
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
            let hash = store.append_turn(&rest[0], turn).await?;
            println!("{hash}");
        }
        "delete-run" => {
            require_len(&rest, 1)?;
            store.delete_run(&rest[0]).await?;
            println!("deleted run {}", rest[0]);
        }
        "delete-object" => {
            require_len(&rest, 1)?;
            store.delete_object(&rest[0]).await?;
            println!("deleted object {}", rest[0]);
        }
        _ => usage(),
    }
    Ok(())
}

async fn open_store(path: String) -> Result<SqliteWorkflowStore, Box<dyn std::error::Error>> {
    let path = PathBuf::from(path);
    Ok(SqliteWorkflowStore::connect(path).await?)
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

fn require_len(args: &[String], expected: usize) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != expected {
        usage();
    }
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  store-cli <db> save-run <run-id> <workflow> <workflow-hash> <current-step>\n  store-cli <db> load-run <run-id>\n  store-cli <db> list-runs\n  store-cli <db> put-step <record-id> <step-id> <status>\n  store-cli <db> get-step <hash>\n  store-cli <db> put-source <entry> <source>\n  store-cli <db> load-head <run-id>\n  store-cli <db> append-turn <run-id> <step-record-id> <turn-id> <content>\n  store-cli <db> delete-run <run-id>\n  store-cli <db> delete-object <hash>"
    );
    std::process::exit(2)
}
