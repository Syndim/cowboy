use std::io::{self, Write};

use anyhow::{Context, anyhow};
use cowboy_agent_acp::transport::StdioConfig;
use cowboy_agent_acp::{BackendPreset, Client, TransportConfig, backend};
use cowboy_agent_client::{Event, ModelInfo, PromptContent};
use serde_json::Value;

#[derive(Debug)]
struct CliConfig {
    command: String,
    args: Vec<String>,
    cwd: String,
    model: ModelInfo,
    /// Show protocol-housekeeping notifications (usage/session/command updates).
    verbose: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let Some(config) = parse_args(std::env::args().skip(1))? else {
        print_usage();
        return Ok(());
    };

    let transport_config = TransportConfig::Stdio(StdioConfig {
        command: config.command.clone(),
        args: config.args.clone(),
        env: vec![],
    });

    eprintln!(
        "Starting ACP agent: {} {}",
        config.command,
        config.args.join(" ")
    );

    let mut client = Client::connect(transport_config)
        .await
        .with_context(|| format!("failed to start ACP agent '{}'", config.command))?;

    if let Some(agent_info) = client.agent_info.as_ref() {
        eprintln!(
            "Connected to {}{}",
            agent_info.name,
            agent_info
                .version
                .as_ref()
                .map(|version| format!(" {version}"))
                .unwrap_or_default()
        );
    }

    let session_id = client
        .new_session(&config.cwd, &[], &config.model)
        .await
        .context("failed to create ACP session")?;
    eprintln!("Session: {session_id}");
    eprintln!("Type :quit or press Ctrl-D to exit.");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("Prompt: ");
        stdout.flush()?;

        let mut input = String::new();
        let read = stdin.read_line(&mut input)?;
        if read == 0 {
            println!();
            break;
        }

        let prompt = input.trim();
        if prompt.is_empty() {
            continue;
        }
        if matches!(prompt, ":quit" | ":exit") {
            break;
        }

        let mut wrote_chunk = false;
        client
            .prompt(
                &session_id,
                vec![PromptContent::text(prompt)],
                &mut |event| print_event(event, &mut wrote_chunk, config.verbose),
            )
            .await
            .context("ACP prompt failed")?;

        if wrote_chunk {
            println!();
        }
    }

    client.close().await?;
    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<Option<CliConfig>> {
    let args: Vec<String> = args.into_iter().collect();

    // `--backend NAME` selects the base preset (default: copilot). It is
    // resolved first so the remaining env/flag overrides layer on top of it.
    let preset = resolve_backend(&args)?;

    let mut command = std::env::var("COWBOY_ACP_COMMAND").unwrap_or_else(|_| preset.command.into());
    let mut command_args = std::env::var("COWBOY_ACP_ARGS")
        .map(|args| args.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_else(|_| preset.owned_args());
    let mut cwd = std::env::var("COWBOY_ACP_CWD")
        .unwrap_or_else(|_| current_dir_string().unwrap_or_else(|_| ".".into()));
    let mut model_id = std::env::var("COWBOY_ACP_MODEL").unwrap_or_else(|_| preset.model.into());
    let mut provider = std::env::var("COWBOY_ACP_PROVIDER")
        .map(|provider| (!provider.is_empty()).then_some(provider))
        .unwrap_or_else(|_| Some(preset.provider.into()));
    let mut verbose = false;

    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--backend" => {
                // Already resolved into `preset`; consume its value.
                iter.next()
                    .ok_or_else(|| anyhow!("--backend requires a value"))?;
            }
            "-v" | "--verbose" => verbose = true,
            "--cwd" => {
                cwd = iter
                    .next()
                    .ok_or_else(|| anyhow!("--cwd requires a value"))?;
            }
            "--model" => {
                model_id = iter
                    .next()
                    .ok_or_else(|| anyhow!("--model requires a value"))?;
            }
            "--provider" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("--provider requires a value"))?;
                provider = (value != "none" && !value.is_empty()).then_some(value);
            }
            "--" => {
                if let Some(next_command) = iter.next() {
                    command = next_command;
                    command_args = iter.collect();
                }
                break;
            }
            value if value.starts_with('-') => return Err(anyhow!("unknown option: {value}")),
            value => {
                command = value.to_owned();
                command_args = iter.collect();
                break;
            }
        }
    }

    Ok(Some(CliConfig {
        command,
        args: command_args,
        cwd,
        model: ModelInfo {
            id: model_id,
            provider,
        },
        verbose,
    }))
}

/// Resolve the `--backend NAME` preset among the leading options.
///
/// Scans only the option tokens that precede any `--` separator or positional
/// command, matching where the main parse loop recognizes `--backend`. The last
/// `--backend` wins; an unknown name is an error.
fn resolve_backend(args: &[String]) -> anyhow::Result<&'static BackendPreset> {
    let mut preset = &BackendPreset::COPILOT;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" => {
                let name = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--backend requires a value"))?;
                preset = BackendPreset::lookup(name).ok_or_else(|| {
                    anyhow!(
                        "unknown backend '{name}' (known: {})",
                        BackendPreset::known_names()
                    )
                })?;
                i += 2;
            }
            "--cwd" | "--model" | "--provider" => i += 2,
            "-h" | "--help" => i += 1,
            "--" => break,
            value if value.starts_with('-') => i += 1,
            _ => break,
        }
    }
    Ok(preset)
}

fn current_dir_string() -> anyhow::Result<String> {
    Ok(std::env::current_dir()?.display().to_string())
}

fn print_usage() {
    let backends = backend::PRESETS
        .iter()
        .map(|preset| {
            format!(
                "{} ({} {}, {})",
                preset.name,
                preset.command,
                preset.args.join(" "),
                preset.provider
            )
        })
        .collect::<Vec<_>>()
        .join("\n  ");
    println!(
        "Usage: acp-chat [--backend NAME] [--cwd PATH] [--model ID] [--provider NAME|none] [-v|--verbose] [--] [COMMAND [ARG...]]\n\n\
Backends:\n  {backends}\n\n\
Default backend: copilot. Explicit --command/positional, --model, --provider, and env override the preset.\n\
--verbose shows protocol-housekeeping events (usage/session/command updates) that are hidden by default.\n\
Env: COWBOY_ACP_COMMAND, COWBOY_ACP_ARGS, COWBOY_ACP_CWD, COWBOY_ACP_MODEL, COWBOY_ACP_PROVIDER\n\n\
Examples:\n  cargo run -p cowboy-agent-acp --bin acp-chat -- --backend omp\n  cargo run -p cowboy-agent-acp --bin acp-chat -- copilot --acp"
    );
}

fn print_event(event: Event, wrote_chunk: &mut bool, verbose: bool) {
    match event {
        Event::MessageChunk { content } => {
            print!("{}", render_content(&content));
            let _ = io::stdout().flush();
            *wrote_chunk = true;
        }
        Event::ThoughtChunk { content } => {
            print_labeled_event("thought", render_content(&content), wrote_chunk);
        }
        Event::ToolCall {
            tool_call_id,
            title,
            kind,
            status,
        } => {
            let label = format!("tool {tool_call_id}");
            let text = format!("{title} ({kind}, {status})");
            print_labeled_event(&label, text, wrote_chunk);
        }
        Event::ToolCallUpdate {
            tool_call_id,
            status,
            content,
        } => {
            let label = format!("tool {tool_call_id}");
            let text = content
                .as_ref()
                .map(|content| format!("{status}: {}", render_content(content)))
                .unwrap_or(status);
            print_labeled_event(&label, text, wrote_chunk);
        }
        Event::Plan { entries } => {
            print_labeled_event("plan", Value::Array(entries).to_string(), wrote_chunk);
        }
        Event::UserMessageChunk { content } => {
            print_labeled_event("user", render_content(&content), wrote_chunk);
        }
        Event::Unknown {
            session_update,
            raw,
        } => {
            // Backends like Oh My Pi stream housekeeping notifications
            // (available_commands_update, usage_update, session_info_update)
            // that would otherwise bury the agent's reply. Hide unless --verbose.
            if verbose {
                print_labeled_event(&session_update, raw.to_string(), wrote_chunk);
            }
        }
    }
}

fn print_labeled_event(label: &str, text: String, wrote_chunk: &mut bool) {
    if *wrote_chunk {
        println!();
    }
    println!("[{label}] {text}");
    *wrote_chunk = false;
}

fn render_content(content: &Value) -> String {
    content
        .as_str()
        .or_else(|| content.get("text").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_positional_command_and_args() {
        let config = parse_args([
            "--model".to_string(),
            "test-model".to_string(),
            "copilot".to_string(),
            "--acp".to_string(),
        ])
        .unwrap()
        .unwrap();

        assert_eq!(config.command, "copilot");
        assert_eq!(config.args, ["--acp"]);
        assert_eq!(config.model.id, "test-model");
    }

    #[test]
    fn parse_provider_none() {
        let config = parse_args([
            "--provider".to_string(),
            "none".to_string(),
            "--".to_string(),
            "agent".to_string(),
        ])
        .unwrap()
        .unwrap();

        assert_eq!(config.command, "agent");
        assert!(config.args.is_empty());
        assert!(config.model.provider.is_none());
    }

    #[test]
    fn backend_selects_preset_defaults() {
        let config = parse_args(["--backend".to_string(), "omp".to_string()])
            .unwrap()
            .unwrap();

        assert_eq!(config.command, "omp");
        assert_eq!(config.args, ["acp"]);
        assert_eq!(config.model.id, "claude-sonnet-4.5");
        assert_eq!(config.model.provider.as_deref(), Some("github-copilot"));
    }

    #[test]
    fn explicit_flags_override_backend_preset() {
        let config = parse_args([
            "--backend".to_string(),
            "omp".to_string(),
            "--model".to_string(),
            "gpt-5".to_string(),
            "--provider".to_string(),
            "openai".to_string(),
        ])
        .unwrap()
        .unwrap();

        // Command/args still come from the omp preset...
        assert_eq!(config.command, "omp");
        assert_eq!(config.args, ["acp"]);
        // ...while explicit field flags win.
        assert_eq!(config.model.id, "gpt-5");
        assert_eq!(config.model.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn unknown_backend_is_rejected() {
        let err = parse_args(["--backend".to_string(), "nope".to_string()]).unwrap_err();
        assert!(err.to_string().contains("unknown backend 'nope'"));
    }

    #[test]
    fn verbose_flag_defaults_off_and_opts_in() {
        let default = parse_args(["--backend".to_string(), "omp".to_string()])
            .unwrap()
            .unwrap();
        assert!(!default.verbose);

        let verbose = parse_args(["--backend".to_string(), "omp".to_string(), "-v".to_string()])
            .unwrap()
            .unwrap();
        assert!(verbose.verbose);
    }
}
