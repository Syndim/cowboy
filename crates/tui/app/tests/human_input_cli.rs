use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cowboy_workflow_core::RunStatus;
use cowboy_workflow_engine::WorkflowRuntime;
use cowboy_workflow_store::SqliteWorkflowStore;
use serde_json::{Value, json};

struct Harness {
    dir: tempfile::TempDir,
    config: PathBuf,
    log: PathBuf,
    database: PathBuf,
    store: tokio::sync::OnceCell<SqliteWorkflowStore>,
}

impl Harness {
    fn new(scenario: &str, enabled: Option<bool>, retries: u32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let workflows = dir.path().join("workflows");
        fs::create_dir(&workflows).unwrap();
        fs::write(
            workflows.join("recover.lua"),
            r#"
            local developer = role("developer", "Work carefully on the current request")
            local implement = step("implement")
            implement.run = function(ctx)
              return action.agent {
                role = developer,
                prompt = "Implement the original request without repeating completed side effects: " .. ctx.request,
                output = { status = { "success", "blocked" } }
              }
            end
            return workflow("recover", implement)
            "#,
        )
        .unwrap();
        let log = dir.path().join("acp.jsonl");
        let database = dir.path().join("state/data.db");
        let config = dir.path().join("config.toml");
        let option = enabled
            .map(|value| format!("agent_human_input = {value}"))
            .unwrap_or_default();
        fs::write(
            &config,
            format!(
                "state_dir = {}\nworkflow_store = {}\nworkflow_dirs = [{}]\n\
                 [config_sets.default]\nmax_steps_per_run = 5\nmax_visits_per_step = 5\n\
                 max_retries_per_run = {retries}\nmax_retries_per_step = {retries}\n{option}\n\
                 [[agents]]\nname = \"default\"\ncommand = {}\nargs = [{}, {}]\n\
                 [agents.watchdog]\nresponse_timeout_seconds = 1\ncancel_timeout_seconds = 1\n\
                 recovery_operation_timeout_seconds = 3\n",
                toml_path(&dir.path().join("state")),
                toml_path(&database),
                toml_path(&workflows),
                toml_path(Path::new(env!("CARGO_BIN_EXE_fake-human-input-acp"))),
                toml_path(Path::new(scenario)),
                toml_path(&log),
            ),
        )
        .unwrap();
        Self {
            dir,
            config,
            log,
            database,
            store: tokio::sync::OnceCell::new(),
        }
    }

    fn raw_cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cowboy"))
            .arg("--config")
            .arg(&self.config)
            .args(args)
            .current_dir(self.dir.path())
            .output()
            .unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        let output = self.raw_cli(args);
        assert!(
            output.status.success(),
            "args={args:?}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    async fn run(&self) -> cowboy_workflow_core::Run {
        let output = self.cli(&["run", "--workflow", "recover", "perform one scoped task"]);
        let text = String::from_utf8_lossy(&output.stdout);
        let run_id = text
            .strip_prefix("run=")
            .and_then(|line| line.split_whitespace().next())
            .expect("CLI run id");
        self.load(run_id).await
    }

    async fn load(&self, run_id: &str) -> cowboy_workflow_core::Run {
        self.store
            .get_or_try_init(|| async { SqliteWorkflowStore::connect(&self.database).await })
            .await
            .unwrap()
            .load_run(run_id)
            .await
            .unwrap()
    }

    async fn only_run(&self) -> cowboy_workflow_core::Run {
        let store = self
            .store
            .get_or_try_init(|| async { SqliteWorkflowStore::connect(&self.database).await })
            .await
            .unwrap();
        let heads = store.list_runs().await.unwrap();
        assert_eq!(heads.len(), 1);
        store.load_run(&heads[0].run_id).await.unwrap()
    }

    fn entries(&self, method: &str) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|entry| entry["method"] == method)
            .collect()
    }
}

fn toml_path(path: &Path) -> String {
    json!(path.to_string_lossy()).to_string()
}

fn waiting_id(run: &cowboy_workflow_core::Run) -> String {
    waiting_id_on(run, "implement")
}

fn waiting_id_on(run: &cowboy_workflow_core::Run, expected_step: &str) -> String {
    let RunStatus::WaitingForInput {
        step,
        input_id,
        choices,
        resume_callback,
        ..
    } = &run.status
    else {
        panic!("expected durable human wait, got {:?}", run.status);
    };
    assert_eq!(step, expected_step);
    assert!(choices.is_empty());
    assert_eq!(resume_callback.kind(), "agent_human_input");
    assert!(input_id.starts_with("agent-"));
    input_id.clone()
}

#[tokio::test]
async fn declared_blocked_is_opt_in_and_continues_same_session_and_step() {
    for enabled in [None, Some(false)] {
        let h = Harness::new("declared", enabled, 0);
        let run = h.run().await;
        assert!(matches!(run.status, RunStatus::Completed), "{run:?}");
        assert_eq!(h.entries("session/prompt").len(), 1);
    }

    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let first_id = waiting_id(&first);
    assert!(first.step.head.is_none());
    assert_eq!(first.step.executed, 1);
    let failure_events = fs::read_to_string(
        h.dir
            .path()
            .join("state/events")
            .join(format!("{}.json", first.id)),
    )
    .unwrap();
    assert!(!failure_events.contains("Operator decision required"));
    assert!(!failure_events.contains("session-1"));

    let reloaded = h.load(&first.id).await;
    assert_eq!(waiting_id(&reloaded), first_id);

    h.cli(&["resume", &first.id]);
    assert_eq!(h.entries("session/prompt").len(), 1);
    assert_eq!(waiting_id(&h.load(&first.id).await), first_id);
    h.cli(&[
        "provide-input",
        &first.id,
        &first_id,
        "Continue only after checking prior work",
    ]);
    let finished = h.load(&first.id).await;
    assert!(
        matches!(finished.status, RunStatus::Completed),
        "{finished:?}"
    );
    assert_eq!(finished.step.executed, 1);
    assert_eq!(finished.step.visits["implement"], 1);
    assert_eq!(h.entries("session/prompt").len(), 2);
    assert_eq!(h.entries("session/load").len(), 1);
    assert_eq!(
        h.entries("session/load")[0]["params"]["sessionId"],
        "session-1"
    );
    assert_eq!(h.entries("session/new").len(), 1);
    let continuation = &h.entries("session/prompt")[1]["params"]["prompt"];
    assert!(
        continuation
            .to_string()
            .contains("Continue only after checking prior work"),
        "{continuation}"
    );
    assert!(
        !continuation
            .to_string()
            .contains("Implement the original request without repeating completed side effects")
    );
}

#[tokio::test]
async fn repeated_wait_rotates_identity_and_rejects_stale_and_duplicate_inputs() {
    let h = Harness::new("twice", Some(true), 0);
    let first = h.run().await;
    let first_id = waiting_id(&first);
    let wrong = h.raw_cli(&["provide-input", &first.id, "wrong", "no"]);
    assert!(!wrong.status.success());
    assert_eq!(h.entries("session/prompt").len(), 1);
    let delivered = h
        .store
        .get()
        .unwrap()
        .load_role_session(&first.id, "developer")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.delivered_actions.len(), 1);

    h.cli(&[
        "provide-input",
        &first.id,
        &first_id,
        "Check existing files first",
    ]);
    let second = h.load(&first.id).await;
    let second_id = waiting_id(&second);
    assert_ne!(first_id, second_id);
    assert_eq!(second.step.head, first.step.head);
    assert_eq!(second.step.visits, first.step.visits);
    let stale = h.raw_cli(&["provide-input", &first.id, &first_id, "repeat"]);
    assert!(!stale.status.success());
    assert_eq!(h.entries("session/prompt").len(), 2);
    let first_answer_prompt = &h.entries("session/prompt")[1]["params"]["prompt"];
    assert!(
        !first_answer_prompt
            .to_string()
            .contains("Implement the original request without repeating completed side effects")
    );
    h.cli(&[
        "provide-input",
        &first.id,
        &second_id,
        "Proceed only with verified context",
    ]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    let duplicate = h.raw_cli(&["provide-input", &first.id, &second_id, "again"]);
    assert!(!duplicate.status.success());
    assert_eq!(h.entries("session/prompt").len(), 3);
    let second_answer_prompt = &h.entries("session/prompt")[2]["params"]["prompt"];
    assert!(
        !second_answer_prompt
            .to_string()
            .contains("Implement the original request without repeating completed side effects")
    );
}

#[tokio::test]
async fn failed_before_first_session_can_wait_and_create_first_session_on_answer() {
    for scenario in ["initialize_error", "new_error"] {
        let h = Harness::new(scenario, Some(true), 0);
        let first = h.run().await;
        let id = waiting_id(&first);
        assert!(first.step.head.is_none());
        assert!(h.entries("session/prompt").is_empty());
        assert_eq!(
            ownership(&h, &first.id)["state"],
            "verified",
            "{scenario}: no first session, but owned process stopped"
        );
        assert_eq!(ownership(&h, &first.id)["attempt_count"], 1);
        h.cli(&[
            "provide-input",
            &first.id,
            &id,
            "Try again after operator checked setup",
        ]);
        assert!(matches!(
            h.load(&first.id).await.status,
            RunStatus::Completed
        ));
        assert!(h.entries("session/load").is_empty());
        assert_eq!(h.entries("session/prompt").len(), 1);
        assert_eq!(ownership(&h, &first.id)["state"], "verified");
        assert_eq!(ownership(&h, &first.id)["attempt_count"], 2);
    }
}

#[tokio::test]
async fn backend_failures_wait_only_when_enabled_and_never_persist_provider_errors() {
    for scenario in ["initialize_error", "new_error", "prompt_error"] {
        for enabled in [None, Some(false)] {
            let h = Harness::new(scenario, enabled, 0);
            assert!(
                !h.raw_cli(&["run", "--workflow", "recover", "task"])
                    .status
                    .success()
            );
            let run = h.only_run().await;
            assert!(
                matches!(run.status, RunStatus::Failed { .. }),
                "{scenario}: {run:?}"
            );
        }

        let h = Harness::new(scenario, Some(true), 0);
        let first = h.run().await;
        let id = waiting_id(&first);
        let persisted = serde_json::to_string(&first.status).unwrap();
        assert!(!persisted.contains("secret"), "{scenario}: {persisted}");
        assert!(!persisted.contains("session-1"), "{scenario}: {persisted}");
        let events = fs::read_to_string(
            h.dir
                .path()
                .join("state/events")
                .join(format!("{}.json", first.id)),
        )
        .unwrap();
        assert!(!events.contains("secret"), "{scenario}: {events}");
        assert!(!events.contains("session-1"), "{scenario}: {events}");
        h.cli(&[
            "provide-input",
            &first.id,
            &id,
            "Continue the original incomplete step",
        ]);
        assert!(matches!(
            h.load(&first.id).await.status,
            RunStatus::Completed
        ));
    }
}

#[tokio::test]
async fn missing_executable_waits_only_when_enabled_then_creates_a_real_first_session() {
    for enabled in [None, Some(false), Some(true)] {
        let h = Harness::new("success", enabled, 0);
        let original = fs::read_to_string(&h.config).unwrap();
        let executable = toml_path(Path::new(env!("CARGO_BIN_EXE_fake-human-input-acp")));
        let missing = toml_path(&h.dir.path().join("no-such-agent"));
        assert!(original.contains(&executable));
        fs::write(&h.config, original.replace(&executable, &missing)).unwrap();

        if enabled != Some(true) {
            assert!(
                !h.raw_cli(&["run", "--workflow", "recover", "task"])
                    .status
                    .success()
            );
            assert!(matches!(
                h.only_run().await.status,
                RunStatus::Failed { .. }
            ));
            continue;
        }

        let first = h.run().await;
        let id = waiting_id(&first);
        assert!(h.entries("session/new").is_empty());
        assert_eq!(ownership(&h, &first.id)["state"], "verified");
        assert_eq!(ownership(&h, &first.id)["attempt_count"], 1);
        assert_eq!(ownership(&h, &first.id)["process_count"], 0);
        fs::write(&h.config, original).unwrap();
        h.cli(&[
            "provide-input",
            &first.id,
            &id,
            "Operator repaired the agent launch path",
        ]);
        assert!(matches!(
            h.load(&first.id).await.status,
            RunStatus::Completed
        ));
        assert_eq!(ownership(&h, &first.id)["state"], "verified");
        assert_eq!(ownership(&h, &first.id)["attempt_count"], 2);
        assert_eq!(ownership(&h, &first.id)["process_count"], 1);
        assert_eq!(h.entries("session/new").len(), 1);
        assert!(h.entries("session/load").is_empty());
    }
}

#[tokio::test]
async fn failed_native_session_load_reparks_without_forging_new_session() {
    let h = Harness::new("load_error", Some(true), 0);
    let first = h.run().await;
    let first_id = waiting_id(&first);
    h.cli(&[
        "provide-input",
        &first.id,
        &first_id,
        "Review the existing session",
    ]);
    let second = h.load(&first.id).await;
    let second_id = waiting_id(&second);
    assert_ne!(first_id, second_id);
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/load").len(), 1);
    assert_eq!(h.entries("session/prompt").len(), 1);
    h.cli(&[
        "provide-input",
        &first.id,
        &second_id,
        "Try to load that same session again",
    ]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/load").len(), 2);
    assert_eq!(h.entries("session/prompt").len(), 2);
}

#[tokio::test]
async fn disabling_a_live_wait_does_not_reclassify_or_replay_it() {
    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let enabled = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        enabled.replace("agent_human_input = true", "agent_human_input = false"),
    )
    .unwrap();
    h.cli(&["resume", &first.id]);
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &id, "continue"])
            .status
            .success()
    );
    assert_eq!(waiting_id(&h.load(&first.id).await), id);
    assert_eq!(h.entries("session/prompt").len(), 1);
    fs::write(&h.config, enabled).unwrap();
    h.cli(&[
        "provide-input",
        &first.id,
        &id,
        "Continue after configuration restored",
    ]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
}

#[tokio::test]
async fn restricted_policy_refusal_is_terminal_not_a_human_approval() {
    let h = Harness::new("permission", Some(true), 0);
    let config = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        config.replace(
            "name = \"default\"",
            "name = \"default\"\naccess = \"deny_all\"",
        ),
    )
    .unwrap();
    assert!(
        !h.raw_cli(&["run", "--workflow", "recover", "task"])
            .status
            .success()
    );
    let run = h.only_run().await;
    assert!(matches!(run.status, RunStatus::Failed { .. }), "{run:?}");
    assert!(run.agent_recovery_denied);
    let failure_events = fs::read_to_string(
        h.dir
            .path()
            .join("state/events")
            .join(format!("{}.json", run.id)),
    )
    .unwrap();
    assert!(!failure_events.contains("session-1"));
    assert!(!failure_events.contains("unsafe-tool"));
    assert!(!h.raw_cli(&["resolve", &run.id, "success"]).status.success());
    assert!(!h.raw_cli(&["resume", &run.id]).status.success());
    assert!(!h.raw_cli(&["step", &run.id]).status.success());
    let runtime = WorkflowRuntime::new(
        cowboy::load_config(&h.config)
            .unwrap()
            .runtime_config(h.dir.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert!(runtime.restart_run(&run.id, "new request").await.is_err());
    runtime.shutdown(std::time::Duration::from_secs(5)).await;
    assert!(run.step.head.is_none());
    assert_eq!(h.entries("session/prompt").len(), 1);
}

#[tokio::test]
async fn crashed_or_timed_out_acp_parks_after_verified_cleanup_then_continues() {
    for scenario in ["prompt_crash", "prompt_timeout"] {
        let h = Harness::new(scenario, Some(true), 0);
        let first = h.run().await;
        let id = waiting_id(&first);
        let RunStatus::WaitingForInput { message, .. } = &first.status else {
            unreachable!()
        };
        assert!(message.contains("prompt failed or timed out"), "{message}");
        assert!(first.step.head.is_none());
        assert!(!h.entries("session/prompt").is_empty());

        if scenario == "prompt_crash" {
            assert_eq!(h.entries("descendant").len(), 1);
            let heartbeat = h.log.with_extension("heartbeat");
            let before = fs::metadata(&heartbeat).unwrap().len();
            std::thread::sleep(std::time::Duration::from_millis(120));
            assert_eq!(
                fs::metadata(heartbeat).unwrap().len(),
                before,
                "unreaped ACP descendant kept writing after durable wait"
            );
        }

        h.cli(&[
            "provide-input",
            &first.id,
            &id,
            "Review completed effects, then continue safely",
        ]);
        assert!(matches!(
            h.load(&first.id).await.status,
            RunStatus::Completed
        ));
    }
}

#[tokio::test]
async fn enabled_agent_failure_asks_before_any_workflow_retry() {
    let h = Harness::new("prompt_error", Some(true), 1);
    let first = h.run().await;
    let id = waiting_id(&first);
    assert_eq!(h.entries("session/prompt").len(), 1);
    assert_eq!(first.retries_used, 0);
    assert_eq!(first.step.retries_used.get("implement"), None);
    assert_eq!(first.step.visits["implement"], 1);
    h.cli(&[
        "provide-input",
        &first.id,
        &id,
        "Acknowledge earlier work and finish the step",
    ]);
    let finished = h.load(&first.id).await;
    assert!(matches!(finished.status, RunStatus::Completed));
    assert_eq!(finished.step.visits["implement"], 1);
    assert_eq!(finished.retries_used, 0);
}

#[tokio::test]
async fn enabled_initialization_error_does_not_retry_and_complete_without_a_decision() {
    let h = Harness::new("initialize_error", Some(true), 2);
    let first = h.run().await;
    waiting_id(&first);
    assert_eq!(h.entries("initialize").len(), 1);
    assert_eq!(first.retries_used, 0);

    let legacy = Harness::new("initialize_error", Some(false), 2);
    assert!(matches!(legacy.run().await.status, RunStatus::Completed));
    assert_eq!(legacy.entries("initialize").len(), 2);
}

#[tokio::test]
async fn oversized_context_and_concurrent_duplicate_cannot_dispatch_agent_twice() {
    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let huge = "x".repeat(4097);
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &id, &huge])
            .status
            .success()
    );
    assert_eq!(h.entries("session/prompt").len(), 1);

    let mut calls = (0..2)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_cowboy"))
                .arg("--config")
                .arg(&h.config)
                .args([
                    "provide-input",
                    &first.id,
                    &id,
                    "Proceed after inspecting existing work",
                ])
                .current_dir(h.dir.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let statuses = calls
        .iter_mut()
        .map(|call| call.wait().unwrap().success())
        .collect::<Vec<_>>();
    assert_eq!(
        statuses.iter().filter(|success| **success).count(),
        1,
        "{statuses:?}"
    );
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("session/prompt").len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_durably_fences_unverified_agent_continuation() {
    let h = Harness::new("hold_human", Some(true), 0);
    let first = h.run().await;
    let original_id = waiting_id(&first);
    let runtime = WorkflowRuntime::new(
        cowboy::load_config(&h.config)
            .unwrap()
            .runtime_config(h.dir.path().to_path_buf()),
    )
    .await
    .unwrap();
    let worker = runtime.clone();
    let run_id = first.id.clone();
    let supplied_id = original_id.clone();
    let call = tokio::spawn(async move {
        worker
            .provide_input_run(&run_id, &supplied_id, "Review completed work")
            .await
    });
    let heartbeat = h.log.with_extension("heartbeat");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while h.entries("session/prompt").len() < 2 || !heartbeat.is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    assert!(
        !h.raw_cli(&["resolve", &first.id, "success"])
            .status
            .success()
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        runtime.resume_run(&first.id),
    )
    .await
    .expect("native cancellation released the run lock");
    assert!(
        result.is_err(),
        "no cleanup proof may turn the checkpoint into an actionable wait"
    );
    let fenced = runtime.load_run(&first.id).await.unwrap();
    assert!(fenced.agent_recovery_denied);
    assert!(matches!(fenced.status, RunStatus::Failed { .. }));
    assert!(fenced.agent_input_checkpoint.is_some());
    assert_eq!(
        runtime
            .native_shutdown_evidence(&first.id)
            .await
            .unwrap()
            .state,
        "unknown"
    );
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &original_id, "try again"])
            .status
            .success()
    );
    assert_eq!(h.entries("session/prompt").len(), 2);
    let before = fs::metadata(&heartbeat).unwrap().len();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(
        fs::metadata(&heartbeat).unwrap().len(),
        before,
        "cancelled agent kept writing"
    );
    runtime.shutdown(std::time::Duration::from_secs(5)).await;
}

#[tokio::test]
async fn selected_named_config_set_overrides_default_agent_human_input_policy() {
    for (default_enabled, careful_enabled) in [(false, true), (true, false)] {
        let h = Harness::new("declared", Some(default_enabled), 0);
        let config = fs::read_to_string(&h.config).unwrap();
        fs::write(
            &h.config,
            format!("{config}\n[config_sets.careful]\nagent_human_input = {careful_enabled}\n"),
        )
        .unwrap();
        let workflow = h.dir.path().join("workflows/recover.lua");
        let source = fs::read_to_string(&workflow).unwrap();
        fs::write(
            workflow,
            source.replace(
                "return workflow(\"recover\", implement)",
                "return workflow(\"recover\", implement, { config_set = \"careful\" })",
            ),
        )
        .unwrap();

        let run = h.run().await;
        assert_eq!(run.config_set.name, "careful");
        if careful_enabled {
            waiting_id(&run);
        } else {
            assert!(matches!(run.status, RunStatus::Completed));
        }
    }
}

#[tokio::test]
async fn non_agent_command_failure_keeps_existing_routing_when_recovery_enabled() {
    for enabled in [None, Some(false), Some(true)] {
        let h = Harness::new("success", enabled, 0);
        fs::write(
            h.dir.path().join("workflows/recover.lua"),
            r#"
            local task = step("task")
            task.run = function()
              return action.command { program = "no-such-command-in-test" }
            end
            return workflow("recover", task)
        "#,
        )
        .unwrap();
        let run = h.run().await;
        assert!(matches!(run.status, RunStatus::Completed));
        let store = h.store.get().unwrap();
        let record = store
            .load_step_record(run.step.head.as_ref().unwrap())
            .await
            .unwrap();
        assert_eq!(record.action, "command");
        assert_eq!(record.output.unwrap().status, "failed");
        assert!(h.entries("session/prompt").is_empty());
    }
}

#[tokio::test]
async fn required_mode_provenance_failure_remains_terminal_with_opt_in() {
    let h = Harness::new("success", Some(true), 0);
    let config = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        config.replace(
            "name = \"default\"",
            "name = \"default\"\naccess = { mode = \"deny_escalation\", acp_mode = \"read-only\" }",
        ),
    )
    .unwrap();
    assert!(
        !h.raw_cli(&["run", "--workflow", "recover", "task"])
            .status
            .success()
    );
    let run = h.only_run().await;
    assert!(matches!(run.status, RunStatus::Failed { .. }), "{run:?}");
    assert!(run.agent_recovery_denied);
    assert!(!h.raw_cli(&["resolve", &run.id, "success"]).status.success());
    assert!(!h.raw_cli(&["resume", &run.id]).status.success());
    assert_eq!(h.entries("session/new").len(), 1);
    assert!(h.entries("session/prompt").is_empty());
}

#[tokio::test]
async fn declared_blocked_does_not_require_success_only_output_fields() {
    for enabled in [None, Some(false), Some(true)] {
        let h = Harness::new("declared", enabled, 0);
        let workflow = h.dir.path().join("workflows/recover.lua");
        let source = fs::read_to_string(&workflow).unwrap();
        fs::write(workflow, source.replace(
            "output = { status = { \"success\", \"blocked\" } }",
            "output = { status = { \"success\", \"blocked\" }, fields = { summary = \"string\" }, required_fields = { \"summary\" } }",
        )).unwrap();

        if enabled != Some(true) {
            assert!(
                !h.raw_cli(&["run", "--workflow", "recover", "task"])
                    .status
                    .success()
            );
            assert!(matches!(
                h.only_run().await.status,
                RunStatus::Failed { .. }
            ));
            continue;
        }

        let run = h.run().await;
        let RunStatus::WaitingForInput {
            resume_callback,
            message,
            ..
        } = run.status
        else {
            panic!("declared incomplete result must wait")
        };
        assert_eq!(resume_callback.payload()["category"], "declared_blocked");
        assert!(message.contains("agent declared"));
    }
}

#[tokio::test]
async fn stale_input_for_previous_role_cannot_wake_new_role() {
    let h = Harness::new("role_switch", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/recover.lua"),
        r#"
        local alpha = role("alpha", "Role alpha owns the first step")
        local beta = role("beta", "Role beta owns the second step")
        local first = step("first")
        first.run = function(ctx)
          return action.agent {
            role = alpha, prompt = "Perform only the first task: " .. ctx.request,
            output = { status = { "success", "blocked" } }
          }
        end
        local second = step("second")
        second.run = function()
          return action.agent {
            role = beta, prompt = "Perform only the second task",
            output = { status = { "success", "blocked" } }
          }
        end
        first:on("success", second)
        return workflow("recover", first)
    "#,
    )
    .unwrap();

    let first = h.run().await;
    let first_id = waiting_id_on(&first, "first");
    assert_eq!(ownership(&h, &first.id)["state"], "verified");
    assert_eq!(ownership(&h, &first.id)["process_count"], 1);
    h.cli(&[
        "provide-input",
        &first.id,
        &first_id,
        "Finish only alpha's task",
    ]);
    let second = h.load(&first.id).await;
    let second_id = waiting_id_on(&second, "second");
    assert_ne!(first_id, second_id);
    assert!(
        second.step.head.is_some(),
        "alpha step must have a real completed record"
    );
    let RunStatus::WaitingForInput {
        resume_callback, ..
    } = &second.status
    else {
        unreachable!()
    };
    assert_eq!(resume_callback.payload()["role"], "beta");
    assert_eq!(ownership(&h, &second.id)["state"], "verified");
    assert_eq!(ownership(&h, &second.id)["attempt_count"], 3);
    assert_eq!(ownership(&h, &second.id)["process_count"], 3);
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &first_id, "wrong role"])
            .status
            .success()
    );
    assert_eq!(h.entries("session/prompt").len(), 3);
    h.cli(&[
        "provide-input",
        &first.id,
        &second_id,
        "Finish only beta's task",
    ]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(ownership(&h, &first.id)["state"], "verified");
    assert_eq!(ownership(&h, &first.id)["attempt_count"], 4);
    assert_eq!(h.entries("session/prompt").len(), 4);
    assert_eq!(h.entries("session/new").len(), 2);
    assert_eq!(h.entries("session/load").len(), 2);
    assert!(
        h.entries("session/prompt")[0]["params"]["prompt"]
            .to_string()
            .contains("Role alpha")
    );
    assert!(
        h.entries("session/prompt")[2]["params"]["prompt"]
            .to_string()
            .contains("Role beta")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_resume_does_not_replay_interrupted_child_agent() {
    let h = Harness::new("hold_human", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/parent.lua"),
        r#"
        local child = step("child")
        child.run = function(ctx)
            return action.workflow { workflow = "recover", request = ctx.request }
        end
        return workflow("parent", child)
    "#,
    )
    .unwrap();
    let start = h.cli(&["run", "--workflow", "parent", "one task"]);
    let stdout = String::from_utf8_lossy(&start.stdout);
    let parent_id = stdout
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let parent = h.load(parent_id).await;
    let old_id = match &parent.status {
        RunStatus::WaitingForInput { input_id, .. } => input_id.clone(),
        other => panic!("expected child proxy wait, got {other:?}"),
    };
    let store = h.store.get().unwrap();
    let child_id = store
        .list_runs()
        .await
        .unwrap()
        .into_iter()
        .find(|head| head.run_id != parent_id)
        .unwrap()
        .run_id;
    let runtime = WorkflowRuntime::new(
        cowboy::load_config(&h.config)
            .unwrap()
            .runtime_config(h.dir.path().to_path_buf()),
    )
    .await
    .unwrap();
    let worker = runtime.clone();
    let parent_id_owned = parent_id.to_string();
    let old_id_owned = old_id.clone();
    let task = tokio::spawn(async move {
        worker
            .provide_input_run(&parent_id_owned, &old_id_owned, "Inspect before continuing")
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while h.entries("session/prompt").len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let child = h.load(&child_id).await;
    assert!(child.agent_input_checkpoint.is_some());
    let before = h.entries("session/prompt").len();
    h.store.get().unwrap().close().await;
    let _resumed = h.raw_cli(&["resume", parent_id]);
    let after = SqliteWorkflowStore::connect(&h.database)
        .await
        .unwrap()
        .load_run(&child_id)
        .await
        .unwrap();
    assert_eq!(
        h.entries("session/prompt").len(),
        before,
        "resuming parent replayed incomplete child action"
    );
    assert_eq!(
        after.step.visits["implement"], 1,
        "child consumed another visit"
    );
    assert!(after.step.head.is_none(), "child forged a completed record");
    runtime.shutdown(std::time::Duration::from_secs(5)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_child_reconciliation_also_fences_idle_parent_proxy() {
    let h = Harness::new("hold_human", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/parent.lua"),
        r#"
        local child = step("child")
        child.run = function(ctx)
            return action.workflow { workflow = "recover", request = ctx.request }
        end
        return workflow("parent", child)
    "#,
    )
    .unwrap();
    let start = h.cli(&["run", "--workflow", "parent", "one task"]);
    let text = String::from_utf8_lossy(&start.stdout);
    let parent_id = text
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let parent = h.load(&parent_id).await;
    let old_id = match parent.status {
        RunStatus::WaitingForInput { input_id, .. } => input_id,
        other => panic!("expected proxy wait: {other:?}"),
    };
    let child_id = h
        .store
        .get()
        .unwrap()
        .list_runs()
        .await
        .unwrap()
        .into_iter()
        .find(|head| head.run_id != parent_id)
        .unwrap()
        .run_id;
    let runtime = WorkflowRuntime::new(
        cowboy::load_config(&h.config)
            .unwrap()
            .runtime_config(h.dir.path().to_path_buf()),
    )
    .await
    .unwrap();
    let worker = runtime.clone();
    let parent_for_task = parent_id.clone();
    let task = tokio::spawn(async move {
        worker
            .provide_input_run(&parent_for_task, &old_id, "Inspect before continuing")
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while h.entries("session/prompt").len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(runtime.resume_run(&child_id).await.is_err());
    let parent = runtime.load_run(&parent_id).await.unwrap();
    assert!(
        parent.agent_recovery_denied,
        "idle parent still offers the obsolete child wait"
    );
    assert!(matches!(parent.status, RunStatus::Failed { .. }));
    assert_eq!(h.entries("session/prompt").len(), 2);
    runtime.shutdown(std::time::Duration::from_secs(5)).await;
}

#[tokio::test]
async fn enabled_wait_preserves_session_when_backend_drops_load_capability() {
    let h = Harness::new("load_unsupported_after_wait", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let before = h.entries("session/new").len();
    h.cli(&[
        "provide-input",
        &first.id,
        &id,
        "Use the original session only",
    ]);
    let next = h.load(&first.id).await;
    let next_id = waiting_id(&next);
    assert_ne!(next_id, id);
    assert_eq!(
        h.entries("session/new").len(),
        before,
        "session was silently replaced"
    );
    assert_eq!(
        h.entries("session/prompt").len(),
        1,
        "original task was replayed"
    );
    assert_eq!(h.entries("session/load").len(), 0);
}

#[tokio::test]
async fn supplied_session_rpc_failure_reparks_without_creating_replacement() {
    let h = Harness::new("provided_load_error", Some(true), 0);
    let start = h.cli(&[
        "run",
        "--workflow",
        "recover",
        "--session-id",
        "developer=session-1",
        "task",
    ]);
    let text = String::from_utf8_lossy(&start.stdout);
    let run_id = text
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let first = h.load(run_id).await;
    let id = waiting_id(&first);
    let RunStatus::WaitingForInput {
        resume_callback, ..
    } = &first.status
    else {
        unreachable!()
    };
    assert_eq!(resume_callback.payload()["category"], "session");
    assert!(!first.agent_recovery_denied);
    assert!(h.entries("session/new").is_empty());
    h.cli(&[
        "provide-input",
        run_id,
        &id,
        "Retry loading exactly the supplied session",
    ]);
    assert!(matches!(h.load(run_id).await.status, RunStatus::Completed));
    assert_eq!(h.entries("session/load").len(), 2);
    assert!(h.entries("session/new").is_empty());
}

#[tokio::test]
async fn supplied_session_failure_cannot_retry_on_different_backend() {
    let h = Harness::new("provided_load_error", Some(true), 0);
    let start = h.cli(&[
        "run",
        "--workflow",
        "recover",
        "--session-id",
        "developer=session-1",
        "task",
    ]);
    let text = String::from_utf8_lossy(&start.stdout);
    let run_id = text
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let first = h.load(run_id).await;
    let id = waiting_id(&first);
    let saved = h
        .store
        .get()
        .unwrap()
        .load_role_session(run_id, "developer")
        .await
        .unwrap()
        .unwrap();
    assert!(saved.backend_identity.is_some());
    let config = fs::read_to_string(&h.config).unwrap();
    h.store.get().unwrap().close().await;
    fs::write(
        &h.config,
        config.replacen("args = [\"provided_load_error\"", "args = [\"success\"", 1),
    )
    .unwrap();
    assert!(
        !h.raw_cli(&[
            "provide-input",
            run_id,
            &id,
            "load supplied session on wrong backend"
        ])
        .status
        .success()
    );
    assert_eq!(h.entries("session/load").len(), 1);
    assert!(h.entries("session/new").is_empty());
    assert!(h.entries("session/prompt").is_empty());
    assert!(
        SqliteWorkflowStore::connect(&h.database)
            .await
            .unwrap()
            .load_run(run_id)
            .await
            .unwrap()
            .agent_recovery_denied
    );
}

#[tokio::test]
async fn changed_backend_config_cannot_load_original_role_session() {
    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let original = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        original.replacen("args = [\"declared\"", "args = [\"success\"", 1),
    )
    .unwrap();
    let reply = h.raw_cli(&[
        "provide-input",
        &first.id,
        &id,
        "Continue only in original backend",
    ]);
    assert!(
        !reply.status.success(),
        "backend drift must not continue the original session"
    );
    assert_eq!(
        h.entries("session/load").len(),
        0,
        "foreign backend received original session id"
    );
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/prompt").len(), 1);
}

#[tokio::test]
async fn changed_working_directory_cannot_load_original_role_session() {
    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let other = h.dir.path().join("different-worktree");
    fs::create_dir(&other).unwrap();
    let response = Command::new(env!("CARGO_BIN_EXE_cowboy"))
        .arg("--config")
        .arg(&h.config)
        .args(["provide-input", &first.id, &id, "wrong checkout"])
        .current_dir(other)
        .output()
        .unwrap();
    assert!(!response.status.success());
    assert_eq!(h.entries("session/load").len(), 0);
    assert_eq!(h.entries("session/prompt").len(), 1);
}

struct StopFakeWriter(PathBuf);

impl Drop for StopFakeWriter {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, "stop");
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_kill_during_agent_turn_cannot_authorize_a_second_writer() {
    use std::os::unix::process::ExitStatusExt;

    let h = Harness::new("hold_human", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let _stop = StopFakeWriter(h.log.with_extension("stop"));
    let mut cowboy = Command::new(env!("CARGO_BIN_EXE_cowboy"))
        .arg("--config")
        .arg(&h.config)
        .args([
            "provide-input",
            &first.id,
            &id,
            "Continue after inspecting work",
        ])
        .current_dir(h.dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let heartbeat = h.log.with_extension("heartbeat");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while h.entries("session/prompt").len() < 2 || !heartbeat.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cowboy.kill().unwrap(); // SIGKILL: Cowboy's Drop cannot terminate ACP.
    assert_eq!(cowboy.wait().unwrap().signal(), Some(9));
    let written = fs::metadata(&heartbeat).unwrap().len();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert!(
        fs::metadata(&heartbeat).unwrap().len() > written,
        "old fakeACP writer died before ownership check"
    );
    assert_eq!(
        ownership(&h, &first.id)["state"],
        "unknown",
        "active old writer cannot be released"
    );
    let checkpoint = h.load(&first.id).await.agent_input_checkpoint.unwrap();
    let RunStatus::WaitingForInput {
        input_id: interrupted_id,
        ..
    } = *checkpoint
    else {
        panic!("missing interrupted wait")
    };
    let resumed = h.raw_cli(&["resume", &first.id]);
    assert!(
        !resumed.status.success(),
        "unverified hard-kill cleanup must fail closed"
    );
    let rejected = h.raw_cli(&[
        "provide-input",
        &first.id,
        &interrupted_id,
        "authorize another writer",
    ]);
    assert!(!rejected.status.success());
    assert_eq!(
        h.entries("session/prompt").len(),
        2,
        "new writer dispatched while old ACP still writes"
    );
    assert_eq!(h.entries("spawn").len(), 2);
    assert!(h.load(&first.id).await.agent_recovery_denied);
    assert_eq!(ownership(&h, &first.id)["state"], "unknown");
}

#[tokio::test]
async fn enabled_empty_turn_asks_before_acp_automatic_continue() {
    let enabled = Harness::new("empty_then_success", Some(true), 2);
    let first = enabled.run().await;
    waiting_id(&first);
    assert_eq!(
        enabled.entries("session/prompt").len(),
        1,
        "no automatic Continue on enabled policy"
    );
    assert_eq!(first.retries_used, 0);
    let legacy = Harness::new("empty_then_success", Some(false), 2);
    assert!(matches!(legacy.run().await.status, RunStatus::Completed));
    assert_eq!(
        legacy.entries("session/prompt").len(),
        2,
        "disabled retains ACP Continue"
    );
}

#[tokio::test]
async fn enabled_crash_asks_without_acp_replacement_or_continue() {
    let h = Harness::new("prompt_crash", Some(true), 2);
    let run = h.run().await;
    waiting_id(&run);
    assert_eq!(h.entries("spawn").len(), 1);
    assert_eq!(h.entries("session/prompt").len(), 1);
    assert_eq!(run.retries_used, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabled_inflight_tool_wait_is_bounded_but_disabled_keeps_watchdog() {
    let enabled = Harness::new("tool_stall", Some(true), 0);
    let _stop = StopFakeWriter(enabled.log.with_extension("stop"));
    let mut call = tokio::process::Command::new(env!("CARGO_BIN_EXE_cowboy"));
    call.arg("--config")
        .arg(&enabled.config)
        .args(["run", "--workflow", "recover", "one task"])
        .current_dir(enabled.dir.path())
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(6), call.output())
        .await
        .expect("opted-in tool wait must reach human input")
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    waiting_id(&enabled.only_run().await);
    assert_eq!(enabled.entries("session/prompt").len(), 1);
    assert_eq!(enabled.entries("session/cancel").len(), 1);
    let heartbeat = enabled.log.with_extension("heartbeat");
    let before = fs::metadata(&heartbeat).unwrap().len();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(
        fs::metadata(&heartbeat).unwrap().len(),
        before,
        "unverified tool writer remained active"
    );

    let legacy = Harness::new("tool_stall", Some(false), 0);
    let _stop_legacy = StopFakeWriter(legacy.log.with_extension("stop"));
    let mut old = Command::new(env!("CARGO_BIN_EXE_cowboy"))
        .arg("--config")
        .arg(&legacy.config)
        .args(["run", "--workflow", "recover", "one task"])
        .current_dir(legacy.dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while legacy.entries("session/prompt").is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2400)).await;
    let still_running = old.try_wait().unwrap().is_none();
    let legacy_cancelled = legacy.entries("session/cancel").len();
    old.kill().unwrap();
    let _ = old.wait();
    assert!(
        still_running,
        "disabled tool-call watchdog changed its legacy behavior"
    );
    assert_eq!(
        legacy_cancelled, 0,
        "disabled in-flight tool watchdog sent a cancel"
    );
}

#[tokio::test]
async fn first_prompt_send_failure_preserves_undelivered_task_on_continuation() {
    let h = Harness::new("prompt_send_failure", Some(true), 0);
    let workflow = h.dir.path().join("workflows/recover.lua");
    let source = fs::read_to_string(&workflow).unwrap();
    fs::write(&workflow, source.replace(
        "output = { status = { \"success\", \"blocked\" } }",
        "task = { key = \"scope\", instructions = \"STATIC CONTRACT MUST ARRIVE\", turn = \"MUTATING TURN MUST BE REVIEWED\" }, output = { status = { \"success\", \"blocked\" } }",
    )).unwrap();
    let first = h.run().await;
    let id = waiting_id(&first);
    let saved = h
        .store
        .get()
        .unwrap()
        .load_role_session(&first.id, "developer")
        .await
        .unwrap()
        .unwrap();
    assert!(
        saved.delivered_task_contracts.is_empty(),
        "failed first prompt was falsely marked delivered"
    );
    assert!(!saved.role_instructions_sent);
    h.cli(&[
        "provide-input",
        &first.id,
        &id,
        "Check old work and follow the original contract",
    ]);
    let second = &h.entries("session/prompt")[0]["params"]["prompt"];
    assert!(
        second.to_string().contains("STATIC CONTRACT MUST ARRIVE"),
        "{second}"
    );
    assert!(
        second
            .to_string()
            .contains("MUTATING TURN MUST BE REVIEWED"),
        "{second}"
    );
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/load").len(), 1);
}

#[tokio::test]
async fn sqlite_record_commit_abort_retains_same_action_checkpoint_and_head() {
    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let store = h.store.get().unwrap();
    sqlx::query("CREATE TRIGGER reject_agent_record BEFORE INSERT ON objects WHEN json_extract(NEW.kind, '$') = 'step_record' BEGIN SELECT RAISE(ABORT, 'injected record commit rollback'); END")
        .execute(store.pool()).await.unwrap();

    let output = h.raw_cli(&[
        "provide-input",
        &first.id,
        &id,
        "Check work and complete the original task",
    ]);
    assert!(!output.status.success());
    let persisted = SqliteWorkflowStore::connect(&h.database).await.unwrap();
    let failed = persisted.load_run(&first.id).await.unwrap();
    assert!(matches!(failed.status, RunStatus::Running));
    assert!(
        failed.agent_input_checkpoint.is_some(),
        "commit error discarded original checkpoint"
    );
    assert_eq!(failed.step.next, first.step.next);
    assert_eq!(failed.step.head, first.step.head);
    assert_eq!(failed.step.visits, first.step.visits);
    let head = persisted.load_run_head(&first.id).await.unwrap();
    assert_eq!(
        ownership(&h, &first.id)["state"],
        "unknown",
        "uncommitted agent result cannot attest idle"
    );
    assert_eq!(head.head_step, first.step.head);
    assert_eq!(head.status, failed.status);
    let records: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objects WHERE json_extract(kind, '$') = 'step_record'",
    )
    .fetch_one(persisted.pool())
    .await
    .unwrap();
    assert_eq!(records, 0, "rolled back record must not exist");
    let events = fs::read_to_string(
        h.dir
            .path()
            .join("state/events")
            .join(format!("{}.json", first.id)),
    )
    .unwrap();
    assert!(
        !events.contains("Continued the incomplete step"),
        "uncommitted reply leaked"
    );
    assert!(
        !events.contains("session-1"),
        "uncommitted session id leaked"
    );
    sqlx::query("DROP TRIGGER reject_agent_record")
        .execute(persisted.pool())
        .await
        .unwrap();
    assert!(
        !h.raw_cli(&["resolve", &first.id, "success"])
            .status
            .success()
    );
    assert!(!h.raw_cli(&["resume", &first.id]).status.success());
    assert_eq!(
        h.entries("session/prompt").len(),
        2,
        "must not replay uncommitted agent action"
    );
}

#[tokio::test]
async fn enabled_and_disabled_runs_in_one_runtime_keep_separate_acp_policies() {
    let h = Harness::new("empty_then_success", Some(true), 2);
    let config = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        config.replace(
            "[[agents]]",
            "[config_sets.legacy]\nagent_human_input = false\n\n[[agents]]",
        ),
    )
    .unwrap();
    let source = fs::read_to_string(h.dir.path().join("workflows/recover.lua")).unwrap();
    fs::write(
        h.dir.path().join("workflows/legacy.lua"),
        source.replace(
            "return workflow(\"recover\", implement)",
            "return workflow(\"legacy\", implement, { config_set = \"legacy\" })",
        ),
    )
    .unwrap();
    let runtime = WorkflowRuntime::new(
        cowboy::load_config(&h.config)
            .unwrap()
            .runtime_config(h.dir.path().to_path_buf()),
    )
    .await
    .unwrap();
    let parked = runtime
        .start_run_with_workflow("recover", "first")
        .await
        .unwrap();
    waiting_id(&parked.run);
    assert_eq!(parked.run.retries_used, 0);
    let legacy = runtime
        .start_run_with_workflow("legacy", "second")
        .await
        .unwrap();
    assert!(
        matches!(legacy.run.status, RunStatus::Completed),
        "{:?}",
        legacy.run.status
    );
    assert_eq!(
        h.entries("session/prompt").len(),
        2,
        "enabled run parks while disabled run completes in the same runtime"
    );
    assert_eq!(h.entries("spawn").len(), 2);
    runtime.shutdown(std::time::Duration::from_secs(5)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postcommit_event_save_failure_does_not_replay_completed_agent() {
    let h = Harness::new("delayed_success", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_cowboy"));
    command
        .arg("--config")
        .arg(&h.config)
        .args([
            "provide-input",
            &first.id,
            &id,
            "Finish original action after checking work",
        ])
        .current_dir(h.dir.path())
        .kill_on_drop(true);
    let child = command.spawn().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while h.entries("session/prompt").len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let events = h.dir.path().join("state/events");
    let parked_events = h.dir.path().join("state/events-parked");
    fs::rename(&events, &parked_events).unwrap();
    fs::write(&events, "block event file persistence after real commit").unwrap();
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !output.status.success(),
        "event save unexpectedly succeeded"
    );
    fs::remove_file(&events).unwrap();
    fs::rename(parked_events, events).unwrap();

    let store = SqliteWorkflowStore::connect(&h.database).await.unwrap();
    let run = store.load_run(&first.id).await.unwrap();
    assert!(
        matches!(run.status, RunStatus::Completed),
        "committed output lost: {:?}",
        run.status
    );
    assert!(run.agent_input_checkpoint.is_none());
    let head = store.load_run_head(&first.id).await.unwrap();
    assert_eq!(head.head_step, run.step.head);
    assert_eq!(head.status, run.status);
    let record = store
        .load_step_record(run.step.head.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(record.output.unwrap().status, "success");
    let resumed = h.cli(&["resume", &first.id]);
    assert!(String::from_utf8_lossy(&resumed.stdout).contains("status=Completed"));
    assert_eq!(
        h.entries("session/prompt").len(),
        2,
        "committed action was repeated"
    );
}

#[tokio::test]
async fn parent_proxy_rotates_child_wait_and_rejects_old_answer() {
    let h = Harness::new("twice", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/parent.lua"),
        r#"
        local child = step("child")
        child.run = function(ctx)
            return action.workflow { workflow = "recover", request = ctx.request }
        end
        return workflow("parent", child)
    "#,
    )
    .unwrap();
    let start = h.cli(&["run", "--workflow", "parent", "task"]);
    let text = String::from_utf8_lossy(&start.stdout);
    let parent_id = text
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let initial = h.load(parent_id).await;
    let first_id = match initial.status {
        RunStatus::WaitingForInput { input_id, .. } => input_id,
        other => panic!("expected parent child wait: {other:?}"),
    };
    h.store.get().unwrap().close().await;
    h.cli(&[
        "provide-input",
        parent_id,
        &first_id,
        "Inspect original work first",
    ]);
    let store = SqliteWorkflowStore::connect(&h.database).await.unwrap();
    let second = store.load_run(parent_id).await.unwrap();
    let second_id = match second.status {
        RunStatus::WaitingForInput { input_id, .. } => input_id,
        other => panic!("expected new parent proxy wait: {other:?}"),
    };
    assert_ne!(first_id, second_id);
    assert!(
        !h.raw_cli(&["provide-input", parent_id, &first_id, "stale replay"])
            .status
            .success()
    );
    assert_eq!(h.entries("session/prompt").len(), 2);
    store.close().await;
    h.cli(&[
        "provide-input",
        parent_id,
        &second_id,
        "Finish only the original child action",
    ]);
    let store = SqliteWorkflowStore::connect(&h.database).await.unwrap();
    let parent = store.load_run(parent_id).await.unwrap();
    assert!(matches!(parent.status, RunStatus::Completed));
    let child_id = store
        .list_runs()
        .await
        .unwrap()
        .into_iter()
        .find(|head| head.run_id != parent_id)
        .unwrap()
        .run_id;
    let child = store.load_run(&child_id).await.unwrap();
    assert!(matches!(child.status, RunStatus::Completed));
    assert_eq!(child.step.visits["implement"], 1);
    assert_eq!(h.entries("session/prompt").len(), 3);
}

#[tokio::test]
async fn supplied_session_load_error_preserves_unseen_raw_action() {
    let h = Harness::new("provided_load_error", Some(true), 0);
    let workflow = h.dir.path().join("workflows/recover.lua");
    let source = fs::read_to_string(&workflow).unwrap();
    fs::write(
        workflow,
        source.replace(
            "Implement the original request without repeating completed side effects: ",
            "PRIVATE_TASK_SENTINEL load-failure: ",
        ),
    )
    .unwrap();
    let start = h.cli(&[
        "run",
        "--workflow",
        "recover",
        "--session-id",
        "developer=session-1",
        "task",
    ]);
    let text = String::from_utf8_lossy(&start.stdout);
    let id = text
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let first = h.load(id).await;
    let answer = waiting_id(&first);
    assert_eq!(h.entries("session/prompt").len(), 0);
    let session = h
        .store
        .get()
        .unwrap()
        .load_role_session(id, "developer")
        .await
        .unwrap()
        .unwrap();
    assert!(session.role_instructions_sent);
    assert!(
        session.delivered_actions.is_empty(),
        "supplied session must not claim this action was sent"
    );
    h.cli(&[
        "provide-input",
        id,
        &answer,
        "Verify existing work, then follow original task",
    ]);
    let sent = &h.entries("session/prompt")[0]["params"]["prompt"];
    assert!(
        sent.to_string()
            .contains("PRIVATE_TASK_SENTINEL load-failure"),
        "{sent}"
    );
    assert!(
        sent.to_string()
            .contains("Verify existing work before acting")
    );
    assert_eq!(h.entries("session/new").len(), 0);
    assert_eq!(h.entries("session/load").len(), 2);
    assert!(matches!(h.load(id).await.status, RunStatus::Completed));
}

#[tokio::test]
async fn supplied_session_missing_load_capability_preserves_unseen_raw_action() {
    let h = Harness::new("provided_load_unsupported", Some(true), 0);
    let workflow = h.dir.path().join("workflows/recover.lua");
    let source = fs::read_to_string(&workflow).unwrap();
    fs::write(
        workflow,
        source.replace(
            "Implement the original request without repeating completed side effects: ",
            "PRIVATE_TASK_SENTINEL capability-loss: ",
        ),
    )
    .unwrap();
    let start = h.cli(&[
        "run",
        "--workflow",
        "recover",
        "--session-id",
        "developer=session-1",
        "task",
    ]);
    let text = String::from_utf8_lossy(&start.stdout);
    let id = text
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let answer = waiting_id(&h.load(id).await);
    assert!(h.entries("session/prompt").is_empty());
    assert!(h.entries("session/load").is_empty());
    h.cli(&[
        "provide-input",
        id,
        &answer,
        "Verify existing work, then use original instructions",
    ]);
    let sent = &h.entries("session/prompt")[0]["params"]["prompt"];
    assert!(
        sent.to_string()
            .contains("PRIVATE_TASK_SENTINEL capability-loss"),
        "{sent}"
    );
    assert!(
        sent.to_string()
            .contains("Verify existing work before acting")
    );
    assert_eq!(h.entries("session/new").len(), 0);
    assert_eq!(h.entries("session/load").len(), 1);
    assert!(matches!(h.load(id).await.status, RunStatus::Completed));
}

fn two_step_delivery_source(structured: bool) -> String {
    let (first_task, second_task) = if structured {
        (
            "task = { key = 'stable-task', instructions = 'SAME STATIC TASK', turn = 'FIRST TURN' },",
            "task = { key = 'stable-task', instructions = 'SAME STATIC TASK', turn = 'ORIGINAL_SECOND_STEP_SENTINEL new turn' },",
        )
    } else {
        ("", "")
    };
    format!(
        r#"
        local developer = role("developer", "Perform only the requested step")
        local first = step("first")
        first.run = function()
            return action.agent {{ role = developer, prompt = "PREVIOUS STEP", {first_task}
                output = {{ status = {{ "success", "blocked" }} }} }}
        end
        local second = step("second")
        second.run = function()
            return action.agent {{ role = developer, prompt = "ORIGINAL_SECOND_STEP_SENTINEL required new instructions", {second_task}
                output = {{ status = {{ "success", "blocked" }} }} }}
        end
        first:on("success", second)
        return workflow("recover", first)
    "#
    )
}

#[tokio::test]
async fn second_raw_step_failure_preserves_undelivered_current_action() {
    let h = Harness::new("second_step_error", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/recover.lua"),
        two_step_delivery_source(false),
    )
    .unwrap();
    let first = h.run().await;
    let id = waiting_id_on(&first, "second");
    let saved = h
        .store
        .get()
        .unwrap()
        .load_role_session(&first.id, "developer")
        .await
        .unwrap()
        .unwrap();
    assert!(saved.role_instructions_sent);
    assert_eq!(
        saved.delivered_actions.len(),
        1,
        "previous completed step alone is confirmed"
    );
    assert!(!saved.delivered_actions.contains_key(&format!(
        "{}-{}",
        first.id,
        first.step.executed + 1
    )));
    assert_eq!(h.entries("session/prompt").len(), 2);
    h.cli(&[
        "provide-input",
        &first.id,
        &id,
        "Verify prior work then complete the second step",
    ]);
    let sent = &h.entries("session/prompt")[2]["params"]["prompt"];
    assert!(
        sent.to_string()
            .contains("ORIGINAL_SECOND_STEP_SENTINEL required new instructions"),
        "{sent}"
    );
    assert!(
        sent.to_string()
            .contains("Verify existing work before acting")
    );
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/load").len(), 1);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
}

#[tokio::test]
async fn second_turn_with_same_stable_task_key_retains_undelivered_turn() {
    let h = Harness::new("second_step_error", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/recover.lua"),
        two_step_delivery_source(true),
    )
    .unwrap();
    let first = h.run().await;
    let id = waiting_id_on(&first, "second");
    let saved = h
        .store
        .get()
        .unwrap()
        .load_role_session(&first.id, "developer")
        .await
        .unwrap()
        .unwrap();
    assert!(saved.delivered_task_contracts.contains_key("stable-task"));
    assert_eq!(
        saved.delivered_actions.len(),
        1,
        "stable contract is not current turn proof"
    );
    assert!(!saved.delivered_actions.contains_key(&format!(
        "{}-{}",
        first.id,
        first.step.executed + 1
    )));
    assert_eq!(h.entries("session/prompt").len(), 2);
    h.cli(&[
        "provide-input",
        &first.id,
        &id,
        "Verify prior work then finish only the new turn",
    ]);
    let sent = &h.entries("session/prompt")[2]["params"]["prompt"];
    assert!(
        sent.to_string()
            .contains("ORIGINAL_SECOND_STEP_SENTINEL new turn"),
        "{sent}"
    );
    assert!(
        sent.to_string()
            .contains("Verify existing work before acting")
    );
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/load").len(), 1);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
}

#[tokio::test]
async fn confirmed_current_structured_turn_is_not_resent_on_two_human_answers() {
    let h = Harness::new("twice", Some(true), 0);
    let workflow = h.dir.path().join("workflows/recover.lua");
    let source = fs::read_to_string(&workflow).unwrap();
    fs::write(workflow, source.replace(
        "output = { status = { \"success\", \"blocked\" } }",
        "task = { key = 'current-step', instructions = 'CURRENT STATIC CONTRACT', turn = 'CONFIRMED_CURRENT_TURN' }, output = { status = { \"success\", \"blocked\" } }",
    )).unwrap();
    let first = h.run().await;
    let first_id = waiting_id(&first);
    assert!(
        h.entries("session/prompt")[0]["params"]["prompt"]
            .to_string()
            .contains("CONFIRMED_CURRENT_TURN")
    );
    let saved = h
        .store
        .get()
        .unwrap()
        .load_role_session(&first.id, "developer")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.delivered_actions.len(), 1);
    assert!(saved.delivered_task_contracts.contains_key("current-step"));
    h.cli(&[
        "provide-input",
        &first.id,
        &first_id,
        "Inspect prior work before another attempt",
    ]);
    let second = h.load(&first.id).await;
    let second_id = waiting_id(&second);
    assert_ne!(first_id, second_id);
    let second_prompt = &h.entries("session/prompt")[1]["params"]["prompt"];
    assert!(
        second_prompt
            .to_string()
            .contains("Inspect prior work before another attempt")
    );
    assert!(!second_prompt.to_string().contains("CONFIRMED_CURRENT_TURN"));
    h.cli(&[
        "provide-input",
        &first.id,
        &second_id,
        "Finish only the verified incomplete action",
    ]);
    let third_prompt = &h.entries("session/prompt")[2]["params"]["prompt"];
    assert!(
        third_prompt
            .to_string()
            .contains("Finish only the verified incomplete action")
    );
    assert!(!third_prompt.to_string().contains("CONFIRMED_CURRENT_TURN"));
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/load").len(), 2);
}

fn install_preflight(h: &Harness) -> PathBuf {
    let state = h.dir.path().join("authority.json");
    fs::write(
        &state,
        r#"{"revision":"r1","context":"authoritative context r1"}"#,
    )
    .unwrap();
    let workflow = h.dir.path().join("workflows/recover.lua");
    let source = fs::read_to_string(&workflow).unwrap();
    let source = source
        .replace(
            "local implement = step(\"implement\")",
            &format!(
                r#"
            local guard = step("preflight")
            guard.run = function(ctx)
                return action.command {{ program = {}, args = {{ {}, {} }}, timeout_ms = 1000 }}
            end
            guard.verify = function(ctx)
                local parsed = cowboy.json.decode(ctx.authority.fields.stdout)
                if parsed.revision ~= ctx.input then
                    return action.status {{ status = "reject" }}
                end
                return action.status {{ status = "ready", fields = {{ context = parsed.context }} }}
            end
            local implement = step("implement")
        "#,
                toml_path(Path::new(env!("CARGO_BIN_EXE_fake-preflight-authority"))),
                toml_path(&state),
                toml_path(&h.log),
            ),
        )
        .replace(
            "role = developer,",
            "role = developer, pre_input = 'preflight',",
        );
    fs::write(workflow, source).unwrap();
    state
}

#[tokio::test]
async fn preflight_rejects_stale_revision_before_agent_and_appends_current_proof() {
    let h = Harness::new("twice", Some(true), 0);
    let state = install_preflight(&h);
    let first = h.run().await;
    let first_id = waiting_id(&first);
    fs::write(
        &state,
        r#"{"revision":"r2","context":"authoritative context r2"}"#,
    )
    .unwrap();
    let refused = h.raw_cli(&["provide-input", &first.id, &first_id, "r1"]);
    assert!(
        !refused.status.success(),
        "stale human decision bypassed preflight"
    );
    assert_eq!(waiting_id(&h.load(&first.id).await), first_id);
    assert_eq!(h.entries("session/prompt").len(), 1);
    assert_eq!(h.entries("authority").len(), 1);
    h.cli(&["provide-input", &first.id, &first_id, "r2"]);
    let second = h.load(&first.id).await;
    let second_id = waiting_id(&second);
    assert_ne!(first_id, second_id);
    let prompt = &h.entries("session/prompt")[1]["params"]["prompt"];
    assert!(
        prompt.to_string().contains("authoritative context r2"),
        "{prompt}"
    );
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/load").len(), 1);
    let authority_reads = h.entries("authority").len();
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &first_id, "r2"])
            .status
            .success()
    );
    assert_eq!(h.entries("authority").len(), authority_reads);
    assert_eq!(waiting_id(&h.load(&first.id).await), second_id);
    fs::write(
        &state,
        r#"{"revision":"r3","context":"authoritative context r3"}"#,
    )
    .unwrap();
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &second_id, "r2"])
            .status
            .success()
    );
    assert_eq!(h.entries("session/prompt").len(), 2);
    h.cli(&["provide-input", &first.id, &second_id, "r3"]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    let prompt = &h.entries("session/prompt")[2]["params"]["prompt"];
    assert!(
        prompt.to_string().contains("authoritative context r3"),
        "{prompt}"
    );
    assert!(!prompt.to_string().contains("authoritative context r2"));
    let entries = fs::read_to_string(&h.log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let last_authority = entries
        .iter()
        .rposition(|item| item["method"] == "authority")
        .unwrap();
    let last_prompt = entries
        .iter()
        .rposition(|item| item["method"] == "session/prompt")
        .unwrap();
    assert!(
        last_authority < last_prompt,
        "authority read must precede ACP prompt"
    );
    assert_eq!(h.entries("authority").len(), 4);
}

#[tokio::test]
async fn preflight_uses_frozen_source_and_never_launches_acp_on_rejected_answer() {
    let h = Harness::new("declared", Some(true), 0);
    let state = install_preflight(&h);
    let first = h.run().await;
    let id = waiting_id(&first);
    let source_path = h.dir.path().join("workflows/recover.lua");
    fs::write(
        &source_path,
        fs::read_to_string(&source_path)
            .unwrap()
            .replace("if parsed.revision ~= ctx.input then", "if false then"),
    )
    .unwrap();
    fs::write(&state, r#"{"revision":"r2","context":"current r2"}"#).unwrap();
    let denied = h.raw_cli(&["provide-input", &first.id, &id, "r1"]);
    assert!(
        !denied.status.success(),
        "modified workflow source bypassed frozen verifier"
    );
    assert_eq!(waiting_id(&h.load(&first.id).await), id);
    assert_eq!(h.entries("session/new").len(), 1);
    assert!(h.entries("session/load").is_empty());
    assert_eq!(h.entries("session/prompt").len(), 1);
    h.cli(&["provide-input", &first.id, &id, "r2"]);
    let entries = fs::read_to_string(&h.log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let authority = entries
        .iter()
        .rposition(|item| item["method"] == "authority")
        .unwrap();
    let load = entries
        .iter()
        .rposition(|item| item["method"] == "session/load")
        .unwrap();
    assert!(
        authority < load,
        "guard must finish before loading the original ACP session"
    );
    assert_eq!(h.entries("session/new").len(), 1);
    assert_eq!(h.entries("session/prompt").len(), 2);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
}

#[tokio::test]
async fn preflight_before_first_session_rejects_stale_then_creates_original_session() {
    let h = Harness::new("initialize_error", Some(true), 0);
    let state = install_preflight(&h);
    let first = h.run().await;
    let id = waiting_id(&first);
    assert!(h.entries("session/new").is_empty());
    fs::write(
        &state,
        r#"{"revision":"r2","context":"current first-session context"}"#,
    )
    .unwrap();
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &id, "r1"])
            .status
            .success()
    );
    assert_eq!(waiting_id(&h.load(&first.id).await), id);
    assert!(h.entries("session/new").is_empty());
    assert_eq!(h.entries("initialize").len(), 1);
    h.cli(&["provide-input", &first.id, &id, "r2"]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("initialize").len(), 2);
    assert_eq!(h.entries("session/new").len(), 1);
    assert!(h.entries("session/load").is_empty());
    assert_eq!(h.entries("session/prompt").len(), 1);
    assert!(
        h.entries("session/prompt")[0]["params"]["prompt"]
            .to_string()
            .contains("current first-session context")
    );
}

#[tokio::test]
async fn preflight_deleted_failed_malformed_and_overlong_proof_keep_exact_wait() {
    let h = Harness::new("declared", Some(true), 0);
    let state = install_preflight(&h);
    let first = h.run().await;
    let id = waiting_id(&first);
    for contents in [
        None,
        Some(r#"{"revision":"r1","context":"ignored","fixture_fail":true}"#.to_string()),
        Some("not JSON".to_string()),
        Some(format!(
            r#"{{"revision":"r1","context":"{}"}}"#,
            "x".repeat(4097)
        )),
        Some(r#"{"revision":"r1","context":"bad\ncontrol"}"#.to_string()),
    ] {
        match contents {
            Some(content) => fs::write(&state, content).unwrap(),
            None => fs::remove_file(&state).unwrap(),
        }
        let failed = h.raw_cli(&["provide-input", &first.id, &id, "r1"]);
        assert!(!failed.status.success());
        assert!(!String::from_utf8_lossy(&failed.stderr).contains("fixture_fail"));
        assert_eq!(waiting_id(&h.load(&first.id).await), id);
        assert!(h.load(&first.id).await.agent_input_checkpoint.is_none());
        assert_eq!(h.entries("session/prompt").len(), 1);
        assert!(h.entries("session/load").is_empty());
    }
    let seen = h.entries("authority").len();
    assert!(
        !h.raw_cli(&["provide-input", &first.id, "agent-wrong", "r1"])
            .status
            .success()
    );
    assert_eq!(
        h.entries("authority").len(),
        seen,
        "wrong generation invoked authority"
    );
    fs::write(
        &state,
        r#"{"revision":"r1","context":"recovered trustworthy context"}"#,
    )
    .unwrap();
    h.cli(&["provide-input", &first.id, &id, "r1"]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("session/prompt").len(), 2);
}

#[tokio::test]
async fn preflight_timeout_and_bad_output_do_not_consume_wait() {
    let h = Harness::new("declared", Some(true), 0);
    let state = install_preflight(&h);
    let first = h.run().await;
    let id = waiting_id(&first);
    fs::write(
        &state,
        r#"{"revision":"r1","context":"never accepted","fixture_slow":true}"#,
    )
    .unwrap();
    let start = std::time::Instant::now();
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &id, "r1"])
            .status
            .success()
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(waiting_id(&h.load(&first.id).await), id);
    assert_eq!(h.entries("session/prompt").len(), 1);
    fs::write(&state, r#"{"revision":"r1","context":"safe","program":"do-not-run","role":"foreign","status":"success"}"#).unwrap();
    h.cli(&["provide-input", &first.id, &id, "r1"]);
    assert_eq!(h.entries("session/prompt").len(), 2);
    assert!(
        h.entries("session/prompt")[1]["params"]["prompt"]
            .to_string()
            .contains("safe")
    );
    assert!(
        !h.entries("session/prompt")[1]["params"]["prompt"]
            .to_string()
            .contains("do-not-run")
    );
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !h.log.with_extension("survived").exists(),
        "authority subprocess survived the guard timeout"
    );
}

#[tokio::test]
async fn preflight_is_optional_and_disabled_mode_never_runs_it() {
    for enabled in [None, Some(false)] {
        let h = Harness::new("declared", enabled, 0);
        let _state = install_preflight(&h);
        let run = h.run().await;
        assert!(matches!(run.status, RunStatus::Completed));
        assert!(h.entries("authority").is_empty());
        assert_eq!(h.entries("session/prompt").len(), 1);
    }
    let h = Harness::new("declared", Some(true), 0);
    let run = h.run().await;
    let id = waiting_id(&run);
    h.cli(&["provide-input", &run.id, &id, "without hook"]);
    assert!(h.entries("authority").is_empty());
    assert!(matches!(h.load(&run.id).await.status, RunStatus::Completed));
}

#[tokio::test]
async fn restricted_agent_still_receives_context_from_workflow_preflight() {
    let h = Harness::new("declared", Some(true), 0);
    let _state = install_preflight(&h);
    let config = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        config.replace(
            "name = \"default\"",
            "name = \"default\"\naccess = \"deny_all\"",
        ),
    )
    .unwrap();
    let first = h.run().await;
    let id = waiting_id(&first);
    h.cli(&["provide-input", &first.id, &id, "r1"]);
    assert!(matches!(
        h.load(&first.id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("authority").len(), 1);
    assert!(
        h.entries("session/prompt")[1]["params"]["prompt"]
            .to_string()
            .contains("authoritative context r1")
    );
}

#[tokio::test]
async fn nested_preflight_rejection_does_not_consume_parent_proxy_or_child() {
    let h = Harness::new("declared", Some(true), 0);
    let state = install_preflight(&h);
    fs::write(
        h.dir.path().join("workflows/parent.lua"),
        r#"
        local child = step("child")
        child.run = function(ctx)
            return action.workflow { workflow = "recover", request = ctx.request }
        end
        return workflow("parent", child)
    "#,
    )
    .unwrap();
    let started = h.cli(&["run", "--workflow", "parent", "parent task"]);
    let stdout = String::from_utf8_lossy(&started.stdout);
    let parent_id = stdout
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let parent = h.load(parent_id).await;
    let RunStatus::WaitingForInput { input_id, .. } = &parent.status else {
        panic!("expected proxy")
    };
    let old_id = input_id.clone();
    fs::write(
        &state,
        r#"{"revision":"r2","context":"fresh child context"}"#,
    )
    .unwrap();
    let rejected = h.raw_cli(&["provide-input", parent_id, &old_id, "r1"]);
    assert!(!rejected.status.success());
    assert_eq!(h.entries("session/prompt").len(), 1);
    assert!(matches!(
        h.load(parent_id).await.status,
        RunStatus::WaitingForInput { .. }
    ));
    let store = h.store.get().unwrap();
    let child_id = store
        .list_runs()
        .await
        .unwrap()
        .into_iter()
        .find(|head| head.run_id != parent_id)
        .unwrap()
        .run_id;
    assert_eq!(waiting_id(&h.load(&child_id).await), old_id);
    store.close().await;
    h.cli(&["provide-input", parent_id, &old_id, "r2"]);
    let reopened = SqliteWorkflowStore::connect(&h.database).await.unwrap();
    assert!(matches!(
        reopened.load_run(parent_id).await.unwrap().status,
        RunStatus::Completed
    ));
    assert!(matches!(
        reopened.load_run(&child_id).await.unwrap().status,
        RunStatus::Completed
    ));
    assert_eq!(h.entries("session/prompt").len(), 2);
    assert_eq!(h.entries("authority").len(), 2);
}

fn ownership(h: &Harness, run_id: &str) -> Value {
    let output = h.cli(&["ownership", run_id]);
    serde_json::from_slice(&output.stdout).expect("safe ownership projection JSON")
}

#[tokio::test]
async fn normal_completion_and_declared_wait_only_prove_owned_writer_after_stop() {
    let h = Harness::new("success_child", Some(true), 0);
    let run = h.run().await;
    assert!(matches!(run.status, RunStatus::Completed));
    let proof = ownership(&h, &run.id);
    assert_eq!(proof["version"], 1);
    assert_eq!(proof["state"], "verified");
    assert_eq!(proof["run_id"], run.id);
    assert_eq!(proof["head"], run.step.head.as_ref().unwrap().as_str());
    assert_eq!(proof["attempt_count"], 1);
    assert_eq!(proof["process_count"], 1);
    let heartbeat = h.log.with_extension("heartbeat");
    let before = fs::metadata(&heartbeat).unwrap().len();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(fs::metadata(&heartbeat).unwrap().len(), before);
    assert!(!output_exposes_private_ownership(&proof));

    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    assert_eq!(ownership(&h, &first.id)["state"], "verified");
    h.cli(&["provide-input", &first.id, &id, "Inspect current work"]);
    let completed = h.load(&first.id).await;
    let proof = ownership(&h, &first.id);
    assert_eq!(proof["state"], "verified");
    assert_eq!(proof["attempt_count"], 2);
    assert_eq!(proof["process_count"], 2);
    assert_eq!(
        proof["head"],
        completed.step.head.as_ref().unwrap().as_str()
    );
}

fn output_exposes_private_ownership(value: &Value) -> bool {
    let text = value.to_string();
    [
        "session-1",
        "backend_identity",
        "owner_token",
        "process_id",
        "prompt",
    ]
    .iter()
    .any(|private| text.contains(private))
}

#[tokio::test]
async fn legacy_missing_or_disabled_human_recovery_never_gains_native_shutdown_proof() {
    for enabled in [None, Some(false)] {
        let h = Harness::new("success", enabled, 0);
        let run = h.run().await;
        assert!(matches!(run.status, RunStatus::Completed));
        let proof = ownership(&h, &run.id);
        assert_eq!(proof["state"], "unknown");
        assert_eq!(proof["attempt_count"], 0);
    }
}

#[tokio::test]
async fn distinct_role_writers_and_nested_child_proofs_must_all_be_quiescent() {
    let h = Harness::new("success", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/recover.lua"),
        r#"
        local alpha = role("alpha", "First task")
        local beta = role("beta", "Second task")
        local first = step("first")
        first.run = function()
          return action.agent { role = alpha, prompt = "First", output = { status = "success" } }
        end
        local second = step("second")
        second.run = function()
          return action.agent { role = beta, prompt = "Second", output = { status = "success" } }
        end
        first:on("success", second)
        return workflow("recover", first)
    "#,
    )
    .unwrap();
    let result = h.run().await;
    assert!(matches!(result.status, RunStatus::Completed));
    let proof = ownership(&h, &result.id);
    assert_eq!(proof["state"], "verified");
    assert_eq!(proof["attempt_count"], 2);
    assert_eq!(proof["process_count"], 2);
    assert_eq!(h.entries("spawn").len(), 2);

    let h = Harness::new("success", Some(true), 0);
    fs::write(
        h.dir.path().join("workflows/parent.lua"),
        r#"
        local child = step("child")
        child.run = function(ctx)
          return action.workflow { workflow = "recover", request = ctx.request }
        end
        return workflow("parent", child)
    "#,
    )
    .unwrap();
    let output = h.cli(&["run", "--workflow", "parent", "nested task"]);
    let parent_id = String::from_utf8_lossy(&output.stdout)
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let parent = h.load(&parent_id).await;
    assert!(matches!(parent.status, RunStatus::Completed));
    assert_eq!(ownership(&h, &parent_id)["state"], "verified");
    assert_eq!(ownership(&h, &parent_id)["attempt_count"], 1);
    let store = h.store.get().unwrap();
    let child_id = store
        .list_runs()
        .await
        .unwrap()
        .into_iter()
        .find(|head| head.run_id != parent_id)
        .unwrap()
        .run_id;
    store.delete_run(&child_id).await.unwrap();
    assert_eq!(
        ownership(&h, &parent_id)["state"],
        "unknown",
        "parent record still references deleted child's owned writer"
    );
}

#[tokio::test]
async fn failed_or_unverified_writer_cannot_become_a_completed_idle_claim() {
    let h = Harness::new("prompt_crash", Some(true), 0);
    let first = h.run().await;
    waiting_id(&first);
    let state = ownership(&h, &first.id);
    assert_eq!(
        state["state"], "verified",
        "verified stopped crashed group is quiescent"
    );

    let h = Harness::new("permission", Some(true), 0);
    let config = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        config.replace(
            "name = \"default\"",
            "name = \"default\"\naccess = \"deny_all\"",
        ),
    )
    .unwrap();
    assert!(
        !h.raw_cli(&["run", "--workflow", "recover", "task"])
            .status
            .success()
    );
    let unsafe_run = h.only_run().await;
    assert!(unsafe_run.agent_recovery_denied);
    assert_eq!(ownership(&h, &unsafe_run.id)["state"], "unknown");
    assert!(!output_exposes_private_ownership(&ownership(
        &h,
        &unsafe_run.id
    )));
}

#[tokio::test]
async fn stale_or_foreign_receipt_cannot_attest_another_run_or_role() {
    use sqlx::Row;

    let h = Harness::new("success", Some(true), 0);
    let first = h.run().await;
    let second = h.run().await;
    let store = h.store.get().unwrap();
    let first_row = sqlx::query(
        "SELECT generation, data, receipt FROM native_writer_attempts WHERE run_id = ?",
    )
    .bind(&first.id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    let second_row = sqlx::query(
        "SELECT generation, data, receipt FROM native_writer_attempts WHERE run_id = ?",
    )
    .bind(&second.id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    let first_data: Value = serde_json::from_slice(first_row.get("data")).unwrap();
    let mut second_data: Value = serde_json::from_slice(second_row.get("data")).unwrap();
    assert_ne!(
        first_data["start"]["owner_token"],
        second_data["start"]["owner_token"]
    );
    assert_ne!(
        first_data["start"]["generation"],
        second_data["start"]["generation"]
    );
    assert_eq!(ownership(&h, &second.id)["state"], "verified");
    let foreign_receipt: Vec<u8> = first_row.get("receipt");
    sqlx::query("UPDATE native_writer_attempts SET receipt = ? WHERE generation = ?")
        .bind(foreign_receipt)
        .bind(second_row.get::<String, _>("generation"))
        .execute(store.pool())
        .await
        .unwrap();
    assert_eq!(ownership(&h, &second.id)["state"], "unknown");
    assert_eq!(ownership(&h, &first.id)["state"], "verified");

    let original_receipt: Vec<u8> = second_row.get("receipt");
    sqlx::query("UPDATE native_writer_attempts SET receipt = ? WHERE generation = ?")
        .bind(original_receipt)
        .bind(second_row.get::<String, _>("generation"))
        .execute(store.pool())
        .await
        .unwrap();
    second_data["start"]["role_id"] = json!("foreign-role");
    sqlx::query("UPDATE native_writer_attempts SET data = ? WHERE generation = ?")
        .bind(serde_json::to_vec(&second_data).unwrap())
        .bind(second_row.get::<String, _>("generation"))
        .execute(store.pool())
        .await
        .unwrap();
    assert_eq!(ownership(&h, &second.id)["state"], "unknown");
}

#[tokio::test]
async fn child_completion_cannot_override_untracked_parent_lifecycle() {
    let h = Harness::new("success", Some(true), 0);
    let config = fs::read_to_string(&h.config).unwrap();
    fs::write(
        &h.config,
        format!("{config}\n[config_sets.untracked]\nagent_human_input = false\n"),
    )
    .unwrap();
    fs::write(
        h.dir.path().join("workflows/parent.lua"),
        r#"
        local child = step("child")
        child.run = function(ctx)
          return action.workflow { workflow = "recover", request = ctx.request }
        end
        return workflow("parent", child, { config_set = "untracked" })
    "#,
    )
    .unwrap();
    let output = h.cli(&["run", "--workflow", "parent", "nested task"]);
    let parent_id = String::from_utf8_lossy(&output.stdout)
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let child_id = h
        .store
        .get_or_try_init(|| async { SqliteWorkflowStore::connect(&h.database).await })
        .await
        .unwrap()
        .list_runs()
        .await
        .unwrap()
        .into_iter()
        .find(|run| run.run_id != parent_id)
        .unwrap()
        .run_id;
    assert!(matches!(
        h.load(&child_id).await.status,
        RunStatus::Completed
    ));
    assert_eq!(ownership(&h, &child_id)["state"], "unknown");
    assert_eq!(ownership(&h, &parent_id)["state"], "unknown");
}

#[tokio::test]
async fn enabled_run_does_not_spawn_unregistered_presentation_agent() {
    let h = Harness::new("success", Some(true), 0);
    let runtime = WorkflowRuntime::new(
        cowboy::load_config(&h.config)
            .unwrap()
            .runtime_config(h.dir.path().to_path_buf()),
    )
    .await
    .unwrap()
    .with_request_topic_generation();
    let report = runtime
        .start_run_with_workflow("recover", "one scoped task")
        .await
        .unwrap();
    assert!(matches!(report.run.status, RunStatus::Completed));
    assert!(report.run.request_topic.is_none());
    assert_eq!(
        h.entries("spawn").len(),
        1,
        "unregistered ACP topic agent started"
    );
    assert_eq!(
        runtime
            .native_shutdown_evidence(&report.run.id)
            .await
            .unwrap()
            .state,
        "verified"
    );
    runtime.shutdown(std::time::Duration::from_secs(5)).await;
}

#[tokio::test]
async fn stepwise_run_tracks_and_stops_each_distinct_agent_process() {
    let h = Harness::new("success", Some(true), 0);
    fs::write(h.dir.path().join("workflows/recover.lua"), r#"
        local developer = role("developer", "Stepwise tasks")
        local first = step("first")
        first.run = function()
          return action.agent { role = developer, prompt = "First", output = { status = "success" } }
        end
        local second = step("second")
        second.run = function()
          return action.agent { role = developer, prompt = "Second", output = { status = "success" } }
        end
        first:on("success", second)
        return workflow("recover", first)
    "#).unwrap();
    let output = h.cli(&["run", "--step", "--workflow", "recover", "two scoped tasks"]);
    let id = String::from_utf8_lossy(&output.stdout)
        .strip_prefix("run=")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    assert!(matches!(h.load(&id).await.status, RunStatus::Running));
    assert_eq!(ownership(&h, &id)["state"], "unknown");
    assert_eq!(ownership(&h, &id)["attempt_count"], 1);
    h.cli(&["step", &id]);
    let finished = h.load(&id).await;
    assert!(matches!(finished.status, RunStatus::Completed));
    let proof = ownership(&h, &id);
    assert_eq!(proof["state"], "verified");
    assert_eq!(proof["attempt_count"], 2);
    assert_eq!(proof["process_count"], 2);
    assert_eq!(h.entries("session/load").len(), 1);
}

#[tokio::test]
async fn lost_native_receipt_fences_answer_and_cannot_be_resolved_as_success() {
    let h = Harness::new("declared", Some(true), 0);
    let first = h.run().await;
    let id = waiting_id(&first);
    let store = h.store.get().unwrap();
    sqlx::query("UPDATE native_writer_attempts SET receipt = NULL WHERE run_id = ?")
        .bind(&first.id)
        .execute(store.pool())
        .await
        .unwrap();
    assert_eq!(ownership(&h, &first.id)["state"], "unknown");
    assert!(
        !h.raw_cli(&["provide-input", &first.id, &id, "another agent writer"])
            .status
            .success()
    );
    let fenced = h.load(&first.id).await;
    assert!(fenced.agent_recovery_denied);
    assert!(matches!(fenced.status, RunStatus::Failed { .. }));
    assert_eq!(h.entries("session/prompt").len(), 1);
    assert_eq!(h.entries("spawn").len(), 1);
    assert_eq!(ownership(&h, &first.id)["state"], "unknown");
    assert!(
        !h.raw_cli(&["resolve", &first.id, "success"])
            .status
            .success()
    );
    assert!(!h.raw_cli(&["resume", &first.id]).status.success());
}

fn public_acp_transport(log: &Path) -> cowboy_agent_acp::TransportConfig {
    cowboy_agent_acp::TransportConfig::Stdio(cowboy_agent_acp::transport::StdioConfig {
        command: env!("CARGO_BIN_EXE_fake-human-input-acp").to_string(),
        args: vec!["success".to_string(), log.to_string_lossy().into_owned()],
        clear_env: false,
        allowed_env: Vec::new(),
        env: Vec::new(),
    })
}

#[tokio::test]
async fn owned_public_acp_clones_cannot_spawn_or_claim_the_original_scope() {
    use cowboy_agent_acp::{AgentAccess, AgentWatchdogOptions, Client};

    let h = Harness::new("success", Some(true), 0);
    let cwd = h.dir.path().to_str().unwrap();
    let owner = "registered-owner";
    let mut original = Client::connect_owned(
        public_acp_transport(&h.log),
        AgentWatchdogOptions::default(),
        AgentAccess::Default,
        owner.to_string(),
    )
    .await
    .unwrap();
    let mut before_close = original.clone();
    let mut after_close = original.clone();
    let mut second_generation_clone = after_close.clone();
    drop(second_generation_clone.clone());
    assert_eq!(
        original.new_session(cwd, &[], None).await.unwrap(),
        "session-1"
    );

    let before_result = before_close.new_session(cwd, &[], None).await;
    if before_result.is_ok() {
        before_close.close().await.unwrap();
    }

    let proof = original.close_verified(owner).await.unwrap();
    assert_eq!(proof.owner_token, owner);
    assert_eq!(proof.scopes.len(), 1);
    assert_eq!(proof.scopes[0].scope_id, owner);
    assert_eq!(proof.session_id.as_deref(), Some("session-1"));
    assert!(original.new_session(cwd, &[], None).await.is_err());
    assert!(before_close.close_verified(owner).await.is_err());
    assert!(after_close.close_verified(owner).await.is_err());
    assert!(second_generation_clone.close_verified(owner).await.is_err());

    let load_result = after_close.load_session("session-1", cwd, &[]).await;
    if load_result.is_ok() {
        after_close.close().await.unwrap();
    }

    let chained_result = second_generation_clone.new_session(cwd, &[], None).await;
    if chained_result.is_ok() {
        second_generation_clone.close().await.unwrap();
    }

    assert!(
        before_result.is_err(),
        "clone spawned before original shutdown"
    );
    assert!(
        load_result.is_err(),
        "clone loaded session through unregistered reconnect"
    );
    assert!(
        chained_result.is_err(),
        "clone of clone discarded ownership fence"
    );
    assert_eq!(
        h.entries("spawn").len(),
        1,
        "owned clone launched an unregistered ACP scope"
    );
}

#[tokio::test]
async fn unowned_public_acp_clone_keeps_legacy_reconnect_behavior() {
    use cowboy_agent_acp::{AgentAccess, AgentWatchdogOptions, Client};

    for human_input in [false, true] {
        let h = Harness::new("success", Some(human_input), 0);
        let mut original = Client::connect_with_options_access_and_human_input(
            public_acp_transport(&h.log),
            AgentWatchdogOptions::default(),
            AgentAccess::Default,
            human_input,
        )
        .await
        .unwrap();
        let mut copy = original.clone();
        assert_eq!(
            copy.new_session(h.dir.path().to_str().unwrap(), &[], None)
                .await
                .unwrap(),
            "session-1"
        );
        assert_eq!(h.entries("spawn").len(), 2);
        copy.close().await.unwrap();
        original.close().await.unwrap();
    }
}
