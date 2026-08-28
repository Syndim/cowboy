use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cowboy(config: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cowboy"));
    command.arg("--config").arg(config).current_dir(cwd);
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn run_id(output: &Output) -> String {
    stdout(output)
        .lines()
        .find_map(|line| {
            line.strip_prefix("run=")
                .and_then(|line| line.split_once(' '))
        })
        .map(|(run_id, _)| run_id.to_string())
        .expect("CLI report should include a run id")
}

fn state_export_path(state_dir: &Path, run_id: &str) -> PathBuf {
    state_dir
        .join("exports")
        .join(format!("cowboy-export-{run_id}.html"))
}

#[test]
fn cli_exports_terminal_transcripts_to_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("runtime-cwd");
    let workflow_dir = dir.path().join("workflows");
    let state_dir = dir.path().join("state");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::create_dir(&workflow_dir).unwrap();
    std::fs::write(
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
    std::fs::write(
        workflow_dir.join("fail.lua"),
        r#"
        local start = step("start")
        start.run = function(ctx)
          return action.fail { reason = "expected failure" }
        end
        return workflow("fail", start)
        "#,
    )
    .unwrap();
    std::fs::write(
        workflow_dir.join("wait.lua"),
        r#"
        local start = step("start")
        start.run = function(ctx)
          return action.ask_user { id = "approval", message = "Approve?" }
        end
        return workflow("wait", start)
        "#,
    )
    .unwrap();

    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
            state_dir = "{}"
            workflow_store = "{}"
            workflow_dirs = ["{}"]
            "#,
            state_dir.display(),
            state_dir.join("data.db").display(),
            workflow_dir.display()
        ),
    )
    .unwrap();

    let completed = cowboy(&config, &cwd)
        .args(["run", "--workflow", "complete", "complete request"])
        .output()
        .unwrap();
    assert!(completed.status.success(), "{}", stdout(&completed));
    let completed_id = run_id(&completed);
    let completed_export = state_export_path(&state_dir, &completed_id);
    assert!(completed_export.exists());
    assert!(stdout(&completed).contains(&format!(
        "terminal_transcript={}",
        completed_export.display()
    )));
    assert!(
        !cwd.join(format!("cowboy-export-{completed_id}.html"))
            .exists()
    );

    let first_html = std::fs::read(&completed_export).unwrap();
    let resumed = cowboy(&config, &cwd)
        .args(["resume", &completed_id])
        .output()
        .unwrap();
    assert!(resumed.status.success(), "{}", stdout(&resumed));
    assert!(stdout(&resumed).contains(&format!(
        "terminal_transcript={}",
        completed_export.display()
    )));
    assert_eq!(std::fs::read(&completed_export).unwrap(), first_html);

    let failed = cowboy(&config, &cwd)
        .args(["run", "--workflow", "fail", "failed request"])
        .output()
        .unwrap();
    assert!(failed.status.success(), "{}", stdout(&failed));
    let failed_id = run_id(&failed);
    assert!(stdout(&failed).contains("status=Failed"));
    assert!(state_export_path(&state_dir, &failed_id).exists());

    let waiting = cowboy(&config, &cwd)
        .args(["run", "--workflow", "wait", "waiting request"])
        .output()
        .unwrap();
    assert!(waiting.status.success(), "{}", stdout(&waiting));
    let waiting_id = run_id(&waiting);
    assert!(stdout(&waiting).contains("status=WaitingForInput"));
    assert!(!stdout(&waiting).contains("terminal_transcript="));
    assert!(!state_export_path(&state_dir, &waiting_id).exists());
}
