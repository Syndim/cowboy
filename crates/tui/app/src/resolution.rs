//! User-facing manual-resolution command guidance.

use cowboy_workflow_engine::ResolutionStatus;

/// Build a syntactically valid resolve command for one available status.
pub fn resolution_command(command_prefix: &str, run_id: &str, status: &ResolutionStatus) -> String {
    let mut command = format!(
        "{command_prefix} {} {}",
        quote_command_argument(run_id),
        quote_command_argument(&status.status)
    );
    for field in status.required_fields.iter().chain(&status.optional_fields) {
        command.push_str(" --field ");
        command.push_str(&quote_command_argument(field));
        command.push(' ');
        command.push_str(&quote_command_argument("..."));
    }

    if status.body_expected {
        command.push_str(" --body '...'");
    }

    command
}

/// Quote one token for both POSIX shells and Cowboy's slash-command tokenizer.
fn quote_command_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> ResolutionStatus {
        ResolutionStatus {
            status: "planned".to_string(),
            target_step: Some("finish".to_string()),
            required_fields: vec!["summary".to_string(), "files".to_string()],
            optional_fields: Vec::new(),
            body_expected: true,
        }
    }

    #[test]
    fn builds_copyable_cli_and_tui_commands() {
        assert_eq!(
            resolution_command("cowboy resolve", "run-1", &status()),
            "cowboy resolve 'run-1' 'planned' --field 'summary' '...' --field 'files' '...' --body '...'"
        );
        assert_eq!(
            resolution_command("/resolve", "run-1", &status()),
            "/resolve 'run-1' 'planned' --field 'summary' '...' --field 'files' '...' --body '...'"
        );
    }

    #[test]
    fn boundary_names_round_trip_through_slash_tokenizer() {
        let names = [
            "foo=bar",
            "-review",
            " review ",
            "",
            "review 'summary' $(printf unsafe)",
        ];
        let status = ResolutionStatus {
            status: "needs 'review' $(printf unsafe)".to_string(),
            required_fields: names.iter().map(|name| (*name).to_string()).collect(),
            ..status()
        };
        let command = resolution_command("/resolve", "run $(printf unsafe)", &status);

        let cowboy_command_parser::SlashCommand::Shared(
            cowboy_command_parser::SharedCommand::Resolve(args),
        ) = cowboy_command_parser::parse_slash_command(&command).unwrap()
        else {
            panic!("expected resolve command");
        };
        assert_eq!(args.run_id, "run $(printf unsafe)");
        assert_eq!(args.status.as_deref(), Some(status.status.as_str()));
        assert_eq!(
            args.fields,
            names
                .into_iter()
                .flat_map(|name| [name.to_string(), "...".to_string()])
                .collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn boundary_names_round_trip_through_posix_shell() {
        let status = ResolutionStatus {
            status: "needs 'review' $(printf unsafe)".to_string(),
            required_fields: vec![
                "foo=bar".to_string(),
                "-review".to_string(),
                " review ".to_string(),
                String::new(),
                "review 'summary' $(printf unsafe)".to_string(),
            ],
            ..status()
        };
        let command = resolution_command("printf '<%s>\\n'", "run $(printf unsafe)", &status);
        let output = std::process::Command::new("sh")
            .args(["-c", &command])
            .output()
            .unwrap();

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "<run $(printf unsafe)>\n\
             <needs 'review' $(printf unsafe)>\n\
             <--field>\n<foo=bar>\n<...>\n\
             <--field>\n<-review>\n<...>\n\
             <--field>\n< review >\n<...>\n\
             <--field>\n<>\n<...>\n\
             <--field>\n<review 'summary' $(printf unsafe)>\n<...>\n\
             <--body>\n<...>\n"
        );
    }
}
