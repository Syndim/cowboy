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
            dir.path().join("state/workflow.redb").display(),
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
}
