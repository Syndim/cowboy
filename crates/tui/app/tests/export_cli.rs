use std::path::Path;
use std::process::{Command, Output};

fn cowboy(config: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cowboy"));
    command.current_dir(cwd).arg("--config").arg(config);
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn cli_exports_searchable_collapsed_html_and_rejects_missing_runs() {
    let dir = tempfile::tempdir().unwrap();
    let workflow_dir = dir.path().join("workflows");
    std::fs::create_dir(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("instant.lua"),
        r#"
        local start = step("start")
        start.run = function(ctx)
          return action.status { status = "success", body = "exported " .. ctx.request }
        end
        return workflow("instant", start)
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

            [config_sets.default]
            max_steps_per_run = 5
            max_visits_per_step = 5
            max_retries_per_run = 0
            max_retries_per_step = 0

            [[agents]]
            name = "default"
            command = "unused-agent"
            args = []
            "#,
            dir.path().join("state").display(),
            dir.path().join("state/data.db").display(),
            workflow_dir.display()
        ),
    )
    .unwrap();

    let started = cowboy(&config, dir.path())
        .args(["run", "--workflow", "instant", "CLI export request"])
        .output()
        .unwrap();
    assert!(started.status.success(), "{}", stderr(&started));
    let run_id = stdout(&started)
        .lines()
        .find_map(|line| line.strip_prefix("run="))
        .and_then(|line| line.split_whitespace().next())
        .unwrap()
        .to_string();

    let exported = cowboy(&config, dir.path())
        .args(["export", &run_id])
        .output()
        .unwrap();
    assert!(exported.status.success(), "{}", stderr(&exported));
    let exported_stdout = stdout(&exported);
    assert!(exported_stdout.contains(&format!("run={run_id}")));
    let path = exported_stdout
        .trim()
        .split("path=")
        .nth(1)
        .map(Path::new)
        .unwrap();
    assert!(path.exists(), "{exported_stdout}");
    assert_eq!(path.parent(), Some(dir.path()));

    let html = std::fs::read_to_string(path).unwrap();
    assert!(html.contains("CLI export request"));
    assert!(html.contains("exported CLI export request"));
    assert!(html.contains("<details class=\"card\">"));
    assert!(!html.contains("<details open"));
    assert!(html.contains("id=\"search\""));
    assert!(html.contains("id=\"expand-all\""));
    assert!(html.contains("id=\"collapse-all\""));

    let html_count_before = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".html"))
        .count();
    let missing = cowboy(&config, dir.path())
        .args(["export", "missing-run"])
        .output()
        .unwrap();
    assert!(!missing.status.success(), "{}", stdout(&missing));
    let html_count_after = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".html"))
        .count();
    assert_eq!(html_count_after, html_count_before);
}
