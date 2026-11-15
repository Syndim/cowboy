//! `pi-chat` — interactive test app for the pi RPC backend, mirroring
//! `acp-chat`: spawns pi (`pi --mode rpc`), sends prompts typed on stdin, and
//! streams normalized events.
//!
//! Config is read from the environment:
//!   PI_CHAT_COMMAND  pi executable      (pi)
//!   PI_CHAT_MODEL    model id           (unset = pi default)
//!   PI_CHAT_SESSION  session id         (fresh UUID)

use std::env;

use cowboy_agent_client::{Client, Event, PromptContent, PromptTurnCancellation};
use cowboy_agent_pi::PiRpcClient;

type CliResult = Result<(), Box<dyn std::error::Error>>;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn init_logging() {
    let dir = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    if let Ok(log_path) =
        cowboy_log::init_file_logging(dir, "pi-chat", cowboy_log::TEST_APP_DIRECTIVE)
    {
        tracing::info!(log_path = %log_path.display(), "pi-chat logging initialized");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> CliResult {
    init_logging();
    let command = env::var("PI_CHAT_COMMAND").unwrap_or_else(|_| "pi".to_string());
    let model = env::var("PI_CHAT_MODEL").ok();

    let spec = cowboy_agent_client::AgentLaunchSpec {
        agent: "default".to_string(),
        backend: cowboy_agent_pi::PiRpcBackendConnector::KIND.to_string(),
        command,
        args: Vec::new(),
        allowed_env: Vec::new(),
        access: cowboy_agent_client::AgentAccess::default(),
        watchdog: cowboy_agent_client::AgentWatchdogOptions::default(),
        startup_readiness: None,
        model: model.map(cowboy_agent_client::ModelInfo::with_id),
    };
    let mut client = PiRpcClient::connect(&spec, None).await?;
    let session = client.new_session(".", &[], None).await?;
    eprintln!("pi-chat session {session} ready — type prompts, empty line exits");

    let mut line = String::new();
    loop {
        line.clear();
        if std::io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let prompt = line.trim();
        if prompt.is_empty() {
            break;
        }
        let mut events = Vec::new();
        let stop = client
            .prompt(
                &session,
                vec![PromptContent::text(prompt)],
                PromptTurnCancellation::disabled(),
                &mut |event| {
                    match &event {
                        Event::MessageChunk { content } => {
                            if let Some(text) = content["text"].as_str() {
                                print!("{text}");
                                use std::io::Write;
                                let _ = std::io::stdout().flush();
                            }
                        }
                        Event::ThoughtChunk { .. } => eprintln!("[thinking]"),
                        Event::ToolCall { title, .. } => eprintln!("[tool: {title}]"),
                        Event::ToolCallUpdate { status, .. } => {
                            eprintln!("[tool: {status}]")
                        }
                        Event::Unknown { session_update, .. } => {
                            eprintln!("[unknown: {session_update}]")
                        }
                        _ => {}
                    }
                    events.push(event);
                },
            )
            .await?;
        println!();
        eprintln!("turn stopped: {stop:?} ({} events)", events.len());
    }

    client.close().await?;
    Ok(())
}
