use std::path::Path;
use std::process::{Command, Output};

fn cowboy(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cowboy"));
    command.arg("--config").arg(config);
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn unique_substring(haystack: &str, other: &str) -> String {
    for width in 4..=haystack.len() {
        for start in 0..=haystack.len() - width {
            let candidate = &haystack[start..start + width];
            if !other.contains(candidate) {
                return candidate.to_string();
            }
        }
    }

    panic!("no unique substring found for {haystack:?} against {other:?}");
}

#[test]
fn cli_runs_filters_by_partial_run_id() {
    let dir = tempfile::tempdir().unwrap();
    let workflow_dir = dir.path().join("workflows");
    std::fs::create_dir(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("instant.lua"),
        r#"
        local start = step("start")
        start.run = function(ctx)
          return action.status { status = "success", body = ctx.request }
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

    for request in ["first", "second"] {
        let output = cowboy(&config)
            .args(["run", "--workflow", "instant", request])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }

    let unfiltered = cowboy(&config).arg("runs").output().unwrap();
    assert!(unfiltered.status.success(), "{}", stderr(&unfiltered));
    let unfiltered_stdout = stdout(&unfiltered);
    let run_ids = unfiltered_stdout
        .lines()
        .filter(|line| line.starts_with("run-"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(run_ids.len(), 2, "{}", unfiltered_stdout);
    let partial = unique_substring(&run_ids[0], &run_ids[1]);

    let filtered = cowboy(&config).args(["runs", &partial]).output().unwrap();
    assert!(filtered.status.success(), "{}", stderr(&filtered));
    let filtered_stdout = stdout(&filtered);

    assert!(
        filtered_stdout.contains(&run_ids[0]),
        "filtered output omitted matching run id {} for partial {partial:?}:\n{filtered_stdout}",
        run_ids[0]
    );
    assert!(
        !filtered_stdout.contains(&run_ids[1]),
        "filtered output included nonmatching run id {} for partial {partial:?}:\n{filtered_stdout}",
        run_ids[1]
    );
    assert_eq!(
        filtered_stdout
            .lines()
            .filter(|line| line.starts_with("  started_at: "))
            .count(),
        1,
        "{filtered_stdout}"
    );
}

#[test]
fn cli_logging_filters_keep_private_configuration_and_requests_out_of_logs() {
    let directory = tempfile::tempdir().unwrap();
    let workflows = directory.path().join("workflows");
    std::fs::create_dir(&workflows).unwrap();
    std::fs::write(
        workflows.join("instant.lua"),
        r#"
local start = step("start")
start.run = function(ctx)
  return action.status { status = "success", body = ctx.request }
end
return workflow("instant", start)
"#,
    )
    .unwrap();
    for (label, filter, rust_filter, verbose) in [
        ("default", None, None, false),
        (
            "debug",
            Some("info,cowboy_workflow_engine=debug"),
            None,
            true,
        ),
        ("trace", Some("trace"), None, true),
        ("precedence", Some("info"), Some("trace"), false),
    ] {
        let state = directory.path().join(label);
        let config = directory.path().join(format!("{label}.toml"));
        std::fs::write(
            &config,
            format!(
                "state_dir = {}\nworkflow_store = {}\nworkflow_dirs = [{}]\n[[agents]]\nname = 'default'\ncommand = 'unused-private-executable'\nargs = ['synthetic-credential-canary']\n",
                serde_json::to_string(&state.to_string_lossy()).unwrap(),
                serde_json::to_string(&state.join("data.db").to_string_lossy()).unwrap(),
                serde_json::to_string(&workflows.to_string_lossy()).unwrap(),
            ),
        )
        .unwrap();
        let mut command = cowboy(&config);
        command
            .current_dir(directory.path())
            .env_remove("COWBOY_LOG")
            .env_remove("RUST_LOG")
            .args(["run", "--workflow", "instant", "synthetic-request-canary"]);
        if let Some(filter) = filter {
            command.env("COWBOY_LOG", filter);
        }
        if let Some(filter) = rust_filter {
            command.env("RUST_LOG", filter);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        let logs = std::fs::read_dir(state.join("logs"))
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect::<String>();
        assert!(logs.contains("cowboy logging initialized"));
        assert!(logs.contains("starting requested workflow run"));
        assert_eq!(logs.contains("workflow catalog loaded"), verbose, "{label}");
        for private in [
            "synthetic-credential-canary",
            "synthetic-request-canary",
            "unused-private-executable",
            directory.path().to_str().unwrap(),
        ] {
            assert!(!logs.contains(private), "{label} logged private data");
        }
    }
}
