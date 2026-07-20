use std::path::Path;
use std::process::{Command, Output};

use cowboy::resolution::resolution_command;
use cowboy_workflow_engine::ResolutionStatus;

const RESOLUTION_STATUS: &str = "needs 'review' $(printf unsafe)";
const REQUIRED_FIELDS: [&str; 4] = [
    "review 'summary' $(printf unsafe)",
    "foo=bar",
    "-review",
    " review ",
];

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

#[test]
fn cli_resolve_dispatches_fields_and_renders_quoted_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let workflow_dir = dir.path().join("workflows");
    std::fs::create_dir(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("quote-review.lua"),
        r#"
        local developer = role("developer", { instructions = "Implement" })
        local resolution_status = [[needs 'review' $(printf unsafe)]]
        local required_field = [[review 'summary' $(printf unsafe)]]
        local equals_field = [[foo=bar]]
        local hyphen_field = [[-review]]
        local spaced_field = [[ review ]]
        local start = step("start", { role = developer })
        start.run = function(ctx)
          return action.agent {
            role = developer,
            prompt = "Do work",
            output = {
              status = { resolution_status },
              fields = {
                [required_field] = "string",
                [equals_field] = "string",
                [hyphen_field] = "boolean",
                [spaced_field] = "array"
              },
              required_fields = { spaced_field, hyphen_field, equals_field, required_field }
            }
          }
        end
        local finish = step("finish")
        finish.run = function(ctx)
          local fields = ctx.prev.fields
          return action.status {
            status = "success",
            fields = {
              required = fields[required_field],
              equals_name = fields[equals_field],
              hyphen_name = fields[hyphen_field],
              spaced_name = fields[spaced_field][1]
            },
            body = fields[required_field] .. "|" .. fields[equals_field] .. "|" .. type(fields[hyphen_field]) .. "|" .. fields[spaced_field][1]
          }
        end
        start:on(resolution_status, finish)
        return workflow("quote-review", start)
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
            command = "definitely-missing-agent"
            args = []
            "#,
            dir.path().join("state").display(),
            dir.path().join("state/workflow.redb").display(),
            workflow_dir.display()
        ),
    )
    .unwrap();

    let failed = cowboy(&config)
        .args(["run", "--workflow", "quote-review", "do work"])
        .output()
        .unwrap();
    assert!(!failed.status.success(), "{}", stdout(&failed));

    let runs = cowboy(&config).arg("runs").output().unwrap();
    assert!(runs.status.success(), "{}", stderr(&runs));
    let runs_stdout = stdout(&runs);
    let run_id = runs_stdout
        .lines()
        .find(|line| line.starts_with("run-"))
        .unwrap();

    let options = cowboy(&config).args(["resolve", run_id]).output().unwrap();
    assert!(options.status.success(), "{}", stderr(&options));
    let expected = resolution_command(
        "cowboy resolve",
        run_id,
        &ResolutionStatus {
            status: RESOLUTION_STATUS.to_string(),
            target_step: Some("finish".to_string()),
            required_fields: {
                let mut fields = REQUIRED_FIELDS
                    .iter()
                    .map(|field| (*field).to_string())
                    .collect::<Vec<_>>();
                fields.sort();
                fields
            },
            optional_fields: Vec::new(),
            body_expected: true,
        },
    );
    assert!(stdout(&options).contains(&expected), "{}", stdout(&options));

    let resolved = cowboy(&config)
        .args([
            "resolve",
            run_id,
            RESOLUTION_STATUS,
            "--field",
            REQUIRED_FIELDS[0],
            "manual resolution",
            "--field",
            REQUIRED_FIELDS[1],
            "equals-value",
            "--field",
            REQUIRED_FIELDS[2],
            "false",
            "--field",
            REQUIRED_FIELDS[3],
            r#"["src/a.rs"]"#,
            "--body",
            "manual body",
        ])
        .output()
        .unwrap();
    assert!(resolved.status.success(), "{}", stderr(&resolved));
    let resolved_stdout = stdout(&resolved);
    assert!(
        resolved_stdout.contains("status=Completed"),
        "{resolved_stdout}"
    );
    assert!(
        resolved_stdout.contains("body: \"manual resolution|equals-value|boolean|src/a.rs\""),
        "{resolved_stdout}"
    );

    let malformed_value = r#"{"token":"private-token""#;
    let malformed = cowboy(&config)
        .args([
            "resolve",
            run_id,
            RESOLUTION_STATUS,
            "--field",
            "credentials",
            malformed_value,
        ])
        .output()
        .unwrap();
    assert!(!malformed.status.success(), "{}", stdout(&malformed));
    let malformed_stderr = stderr(&malformed);
    assert!(
        malformed_stderr.contains("field \"credentials\" has malformed JSON value:"),
        "{malformed_stderr}"
    );
    assert!(
        malformed_stderr.contains("line 1 column"),
        "{malformed_stderr}"
    );
    assert!(
        !malformed_stderr.contains(malformed_value),
        "{malformed_stderr}"
    );
    assert!(
        !malformed_stderr.contains("private-token"),
        "{malformed_stderr}"
    );

    let duplicate = cowboy(&config)
        .args([
            "resolve",
            run_id,
            RESOLUTION_STATUS,
            "--field",
            "summary",
            "one",
            "--field",
            "summary",
            "two",
        ])
        .output()
        .unwrap();
    assert!(!duplicate.status.success(), "{}", stdout(&duplicate));
    assert!(
        stderr(&duplicate).contains("field \"summary\" was provided more than once"),
        "{}",
        stderr(&duplicate)
    );

    for arguments in [
        vec![
            "resolve", run_id, "--field", "summary", "one", "--field", "summary", "two",
        ],
        vec!["resolve", run_id, "--body", "details"],
    ] {
        let rejected = cowboy(&config).args(arguments).output().unwrap();
        assert!(!rejected.status.success(), "{}", stdout(&rejected));
        let rejected_stderr = stderr(&rejected);
        assert!(rejected_stderr.contains("<status>"), "{rejected_stderr}");
    }
}
