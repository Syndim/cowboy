//! Deterministic ACP peer used only by the watchdog smoke test.
//!
//! Its identity sidecar and loopback endpoint deliberately make cleanup opt-in:
//! a process is asked to exit only after all recorded identity fields match.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const RECOVERY_TEXT: &str =
    "---\nstatus: success\nsummary: watchdog recovered\n---\nwatchdog recovered";
const WORKSPACE_MARKER: &str = "cowboy-watchdog-smoke-v1\n";
/// How long the `end-turn-cancel` stall keeps its scripted tool call in flight.
///
/// It must span at least one full `response_timeout_seconds` window (the
/// generated smoke config uses 1 s) so the watchdog observes an in-flight tool
/// call, restarts itself, and only cancels once the tool reports `completed`.
const TOOL_CALL_IN_FLIGHT_DELAY: Duration = Duration::from_millis(2500);
const SYNTHETIC_ENVIRONMENT_NAMES: [&str; 4] = [
    "COWBOY_TEST_GLOBAL",
    "COWBOY_TEST_PLANNER",
    "COWBOY_TEST_IMPLEMENTER",
    "COWBOY_TEST_UNAPPROVED",
];
const DEFAULT_ENVIRONMENT_NAMES: [&str; 10] = [
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "TEMP",
    "TMP",
    "HOME",
    "COWBOY_TEST_UNAPPROVED",
];
const SYNTHETIC_ENVIRONMENT_VALUES: [(&str, &str); 4] = [
    ("COWBOY_TEST_GLOBAL", "cowboy-global-marker-7a2e"),
    ("COWBOY_TEST_PLANNER", "cowboy-planner-marker-83bf"),
    ("COWBOY_TEST_IMPLEMENTER", "cowboy-implementer-marker-19cd"),
    ("COWBOY_TEST_UNAPPROVED", "cowboy-unapproved-marker-64ef"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    AcknowledgeCancel,
    IgnoreCancel,
    CancelEndsTurn,
}

impl Mode {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "acknowledge-cancel" => Ok(Self::AcknowledgeCancel),
            "ignore-cancel" => Ok(Self::IgnoreCancel),
            "end-turn-cancel" => Ok(Self::CancelEndsTurn),
            _ => bail!("--mode must be acknowledge-cancel, ignore-cancel, or end-turn-cancel"),
        }
    }

    fn as_arg(self) -> &'static str {
        match self {
            Self::AcknowledgeCancel => "acknowledge-cancel",
            Self::IgnoreCancel => "ignore-cancel",
            Self::CancelEndsTurn => "end-turn-cancel",
        }
    }
}

#[derive(Debug)]
struct ServeArgs {
    mode: Mode,
    events: PathBuf,
    invocation_token: String,
    identity_dir: PathBuf,
    resume_session_id: Option<String>,
}

#[derive(Debug)]
struct VerifyArgs {
    cowboy: PathBuf,
    workspace: PathBuf,
    response_timeout_seconds: u64,
    cancel_timeout_seconds: u64,
    recovery_operation_timeout_seconds: u64,
    soft_deadline_seconds: u64,
    hard_deadline_seconds: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnvironmentProfile {
    Synthetic,
    Default,
}

impl EnvironmentProfile {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "synthetic" => Ok(Self::Synthetic),
            "default" => Ok(Self::Default),
            _ => bail!("--environment-profile must be synthetic or default"),
        }
    }

    fn names(self) -> &'static [&'static str] {
        match self {
            Self::Synthetic => &SYNTHETIC_ENVIRONMENT_NAMES,
            Self::Default => &DEFAULT_ENVIRONMENT_NAMES,
        }
    }
}

#[derive(Debug)]
struct ProbeEnvironmentArgs {
    output: PathBuf,
    profile: EnvironmentProfile,
}

#[derive(Debug)]
struct EnvironmentServeArgs {
    events: PathBuf,
    invocation_token: String,
    identity_dir: PathBuf,
    agent: String,
    profile: EnvironmentProfile,
    mode: Mode,
    resume_session_id: Option<String>,
}

#[derive(Debug)]
struct DefaultEnvironmentVerifyArgs {
    cowboy: PathBuf,
    workspace: PathBuf,
    deadline_seconds: u64,
}

#[derive(Clone, Debug)]
struct EnvironmentServe {
    agent: String,
    profile: EnvironmentProfile,
}

#[derive(Debug)]
struct RequestState {
    session_id: Option<String>,
    pending_prompt: Option<u64>,
    environment: Option<EnvironmentServe>,
    environment_prompt_count: u64,
}

struct EnvironmentPrompt<'a> {
    text: &'a str,
    session_id: &'a str,
    id: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Identity {
    endpoint: String,
    invocation_token: String,
    start_nonce: String,
    pid: u32,
    executable: String,
}

#[derive(Debug, Deserialize)]
struct CleanupChallenge {
    invocation_token: String,
    start_nonce: String,
    pid: u32,
    executable: String,
    action: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FixtureEvent {
    event: String,
    #[serde(flatten)]
    details: serde_json::Map<String, Value>,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") => serve(parse_serve_args(args.collect())?),
        Some("probe-environment") => {
            probe_environment(parse_probe_environment_args(args.collect())?)
        }
        Some("serve-environment") => {
            serve_environment(parse_environment_serve_args(args.collect())?)
        }
        Some("verify-environment") => verify_environment(parse_verify_args(args.collect())?),
        Some("verify-default-allowed-env") => {
            verify_default_allowed_env(parse_default_environment_verify_args(args.collect())?)
        }
        Some("verify") => verify(parse_verify_args(args.collect())?),
        Some("cleanup") => cleanup(&parse_cleanup_args(args.collect())?),
        Some("-h" | "--help") | None => {
            print_usage();
            Ok(())
        }
        Some(command) => bail!("unknown watchdog-fixture command '{command}'"),
    }
}

fn print_usage() {
    println!(
        "Usage:\n  watchdog-fixture serve --mode acknowledge-cancel|ignore-cancel|end-turn-cancel --events FILE --invocation-token TOKEN --identity-dir DIR [--resume=SESSION]\n  watchdog-fixture probe-environment --output FILE [--environment-profile synthetic|default]\n  watchdog-fixture serve-environment --agent NAME --events FILE --invocation-token TOKEN --identity-dir DIR [--environment-profile synthetic|default] [--resume=SESSION]\n  watchdog-fixture verify-environment --cowboy PATH --workspace DIR --response-timeout-seconds N --cancel-timeout-seconds N --recovery-operation-timeout-seconds N --soft-deadline-seconds N --hard-deadline-seconds N\n  watchdog-fixture verify-default-allowed-env --cowboy PATH --workspace DIR --deadline-seconds N\n  watchdog-fixture verify --cowboy PATH --workspace DIR --response-timeout-seconds N --cancel-timeout-seconds N --recovery-operation-timeout-seconds N --soft-deadline-seconds N --hard-deadline-seconds N\n  watchdog-fixture cleanup --workspace DIR"
    );
}

fn parse_probe_environment_args(args: Vec<String>) -> anyhow::Result<ProbeEnvironmentArgs> {
    let mut output = None;
    let mut profile = EnvironmentProfile::Synthetic;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{arg} requires a value"))?;
        match arg.as_str() {
            "--output" => output = Some(PathBuf::from(value)),
            "--environment-profile" => profile = EnvironmentProfile::parse(value)?,
            _ => bail!("unknown probe-environment option '{arg}'"),
        }
        index += 2;
    }
    Ok(ProbeEnvironmentArgs {
        output: output.ok_or_else(|| anyhow!("--output is required"))?,
        profile,
    })
}

fn parse_environment_serve_args(args: Vec<String>) -> anyhow::Result<EnvironmentServeArgs> {
    let mut events = None;
    let mut invocation_token = None;
    let mut identity_dir = None;
    let mut agent = None;
    let mut profile = EnvironmentProfile::Synthetic;
    let mut mode = Mode::AcknowledgeCancel;
    let mut resume_session_id = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--resume=") {
            resume_session_id = Some(value.to_owned());
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{arg} requires a value"))?;
        match arg.as_str() {
            "--events" => events = Some(PathBuf::from(value)),
            "--invocation-token" => invocation_token = Some(value.to_owned()),
            "--identity-dir" => identity_dir = Some(PathBuf::from(value)),
            "--agent" => agent = Some(value.to_owned()),
            "--environment-profile" => profile = EnvironmentProfile::parse(value)?,
            "--mode" => mode = Mode::parse(value)?,
            "--resume" => resume_session_id = Some(value.to_owned()),
            _ => bail!("unknown serve-environment option '{arg}'"),
        }
        index += 2;
    }
    Ok(EnvironmentServeArgs {
        events: events.ok_or_else(|| anyhow!("--events is required"))?,
        invocation_token: invocation_token
            .ok_or_else(|| anyhow!("--invocation-token is required"))?,
        identity_dir: identity_dir.ok_or_else(|| anyhow!("--identity-dir is required"))?,
        agent: agent.ok_or_else(|| anyhow!("--agent is required"))?,
        profile,
        mode,
        resume_session_id,
    })
}

fn parse_serve_args(args: Vec<String>) -> anyhow::Result<ServeArgs> {
    let mut mode = None;
    let mut events = None;
    let mut invocation_token = None;
    let mut identity_dir = None;
    let mut resume_session_id = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--resume=") {
            resume_session_id = Some(value.to_owned());
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{arg} requires a value"))?;
        match arg.as_str() {
            "--mode" => mode = Some(Mode::parse(value)?),
            "--events" => events = Some(PathBuf::from(value)),
            "--invocation-token" => invocation_token = Some(value.to_owned()),
            "--identity-dir" => identity_dir = Some(PathBuf::from(value)),
            "--resume" => resume_session_id = Some(value.to_owned()),
            _ => bail!("unknown serve option '{arg}'"),
        }
        index += 2;
    }
    Ok(ServeArgs {
        mode: mode.ok_or_else(|| anyhow!("--mode is required"))?,
        events: events.ok_or_else(|| anyhow!("--events is required"))?,
        invocation_token: invocation_token
            .ok_or_else(|| anyhow!("--invocation-token is required"))?,
        identity_dir: identity_dir.ok_or_else(|| anyhow!("--identity-dir is required"))?,
        resume_session_id,
    })
}

fn parse_verify_args(args: Vec<String>) -> anyhow::Result<VerifyArgs> {
    let mut cowboy = None;
    let mut workspace = None;
    let mut response_timeout_seconds = None;
    let mut cancel_timeout_seconds = None;
    let mut recovery_operation_timeout_seconds = None;
    let mut soft_deadline_seconds = None;
    let mut hard_deadline_seconds = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{arg} requires a value"))?;
        match arg.as_str() {
            "--cowboy" => cowboy = Some(PathBuf::from(value)),
            "--workspace" => workspace = Some(PathBuf::from(value)),
            "--response-timeout-seconds" => {
                response_timeout_seconds = Some(parse_seconds(arg, value)?)
            }
            "--cancel-timeout-seconds" => cancel_timeout_seconds = Some(parse_seconds(arg, value)?),
            "--recovery-operation-timeout-seconds" => {
                recovery_operation_timeout_seconds = Some(parse_seconds(arg, value)?)
            }
            "--soft-deadline-seconds" => soft_deadline_seconds = Some(parse_seconds(arg, value)?),
            "--hard-deadline-seconds" => hard_deadline_seconds = Some(parse_seconds(arg, value)?),
            _ => bail!("unknown verify option '{arg}'"),
        }
        index += 2;
    }
    Ok(VerifyArgs {
        cowboy: cowboy.ok_or_else(|| anyhow!("--cowboy is required"))?,
        workspace: workspace.ok_or_else(|| anyhow!("--workspace is required"))?,
        response_timeout_seconds: response_timeout_seconds
            .ok_or_else(|| anyhow!("--response-timeout-seconds is required"))?,
        cancel_timeout_seconds: cancel_timeout_seconds
            .ok_or_else(|| anyhow!("--cancel-timeout-seconds is required"))?,
        recovery_operation_timeout_seconds: recovery_operation_timeout_seconds
            .ok_or_else(|| anyhow!("--recovery-operation-timeout-seconds is required"))?,
        soft_deadline_seconds: soft_deadline_seconds
            .ok_or_else(|| anyhow!("--soft-deadline-seconds is required"))?,
        hard_deadline_seconds: hard_deadline_seconds
            .ok_or_else(|| anyhow!("--hard-deadline-seconds is required"))?,
    })
}

fn parse_default_environment_verify_args(
    args: Vec<String>,
) -> anyhow::Result<DefaultEnvironmentVerifyArgs> {
    let mut cowboy = None;
    let mut workspace = None;
    let mut deadline_seconds = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow!("{arg} requires a value"))?;
        match arg.as_str() {
            "--cowboy" => cowboy = Some(PathBuf::from(value)),
            "--workspace" => workspace = Some(PathBuf::from(value)),
            "--deadline-seconds" => deadline_seconds = Some(parse_seconds(arg, value)?),
            _ => bail!("unknown verify-default-allowed-env option '{arg}'"),
        }
        index += 2;
    }
    Ok(DefaultEnvironmentVerifyArgs {
        cowboy: cowboy.ok_or_else(|| anyhow!("--cowboy is required"))?,
        workspace: workspace.ok_or_else(|| anyhow!("--workspace is required"))?,
        deadline_seconds: deadline_seconds
            .ok_or_else(|| anyhow!("--deadline-seconds is required"))?,
    })
}

fn parse_cleanup_args(args: Vec<String>) -> anyhow::Result<PathBuf> {
    match args.as_slice() {
        [flag, value] if flag == "--workspace" => Ok(PathBuf::from(value)),
        _ => bail!("Usage: watchdog-fixture cleanup --workspace DIR"),
    }
}

fn parse_seconds(name: &str, value: &str) -> anyhow::Result<u64> {
    let value: u64 = value
        .parse()
        .with_context(|| format!("{name} must be a positive integer"))?;
    ensure!(value > 0, "{name} must be greater than zero");
    Ok(value)
}

fn serve(args: ServeArgs) -> anyhow::Result<()> {
    serve_inner(
        args.events,
        args.invocation_token,
        args.identity_dir,
        args.resume_session_id,
        args.mode,
        None,
    )
}

fn serve_environment(args: EnvironmentServeArgs) -> anyhow::Result<()> {
    serve_inner(
        args.events,
        args.invocation_token,
        args.identity_dir,
        args.resume_session_id,
        args.mode,
        Some(EnvironmentServe {
            agent: args.agent,
            profile: args.profile,
        }),
    )
}

fn serve_inner(
    events_path: PathBuf,
    invocation_token: String,
    identity_dir: PathBuf,
    resume_session_id: Option<String>,
    mode: Mode,
    environment: Option<EnvironmentServe>,
) -> anyhow::Result<()> {
    fs::create_dir_all(&identity_dir)?;
    let executable = canonical_current_exe()?;
    let start_nonce = Uuid::new_v4().to_string();
    let pid = std::process::id();
    let listener = TcpListener::bind("127.0.0.1:0").context("bind fixture identity endpoint")?;
    listener.set_nonblocking(true)?;
    let identity = Identity {
        endpoint: listener.local_addr()?.to_string(),
        invocation_token: invocation_token.clone(),
        start_nonce,
        pid,
        executable,
    };
    let identity_path = identity_dir.join(format!("{pid}.json"));
    write_json(&identity_path, &identity)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let endpoint_shutdown = Arc::clone(&shutdown);
    let endpoint_identity = Identity {
        endpoint: identity.endpoint.clone(),
        invocation_token: identity.invocation_token.clone(),
        start_nonce: identity.start_nonce.clone(),
        pid: identity.pid,
        executable: identity.executable.clone(),
    };
    thread::spawn(move || identity_server(listener, endpoint_identity, endpoint_shutdown));

    let mut events = EventWriter::new(&events_path)?;
    events.record(
        "process_started",
        json!({
            "pid": pid,
            "start_nonce": identity.start_nonce,
            "invocation_token": invocation_token,
            "executable": identity.executable,
            "endpoint": identity.endpoint,
            "resume_session_id": resume_session_id,
            "agent": environment.as_ref().map(|environment| &environment.agent),
            "environment": environment.as_ref().map(|environment| environment_state(environment.profile)),
        }),
    )?;

    let (line_tx, line_rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    let mut request_state = RequestState {
        session_id: resume_session_id,
        pending_prompt: None,
        environment,
        environment_prompt_count: 0,
    };
    while !shutdown.load(Ordering::SeqCst) {
        let Ok(line) = line_rx.recv_timeout(Duration::from_millis(50)) else {
            continue;
        };
        let line = line?;
        let request: Value =
            serde_json::from_str(&line).context("fixture received invalid JSON")?;
        let completed_continue = request.get("method").and_then(Value::as_str)
            == Some("session/prompt")
            && request
                .pointer("/params/prompt/0/text")
                .and_then(Value::as_str)
                .or_else(|| {
                    request
                        .pointer("/params/prompt/0/content")
                        .and_then(Value::as_str)
                })
                == Some("Continue");
        let completed_topic = request.get("method").and_then(Value::as_str)
            == Some("session/prompt")
            && request
                .pointer("/params/prompt/0/text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("Create a compact title-bar topic"));
        handle_request_with_environment(
            &request,
            mode,
            &mut request_state,
            &mut output,
            &mut events,
        )?;
        if completed_continue || completed_topic {
            shutdown.store(true, Ordering::SeqCst);
        }
    }
    events.record(
        "process_shutdown",
        json!({ "pid": pid, "reason": "self_shutdown" }),
    )?;
    let _ = fs::remove_file(identity_path);
    Ok(())
}

#[cfg(test)]
fn handle_request(
    request: &Value,
    mode: Mode,
    session_id: &mut Option<String>,
    pending_prompt: &mut Option<u64>,
    output: &mut impl Write,
    events: &mut EventWriter,
) -> anyhow::Result<()> {
    let mut state = RequestState {
        session_id: session_id.clone(),
        pending_prompt: *pending_prompt,
        environment: None,
        environment_prompt_count: 0,
    };
    handle_request_with_environment(request, mode, &mut state, output, events)?;
    *session_id = state.session_id;
    *pending_prompt = state.pending_prompt;
    Ok(())
}

fn handle_request_with_environment(
    request: &Value,
    mode: Mode,
    state: &mut RequestState,
    output: &mut impl Write,
    events: &mut EventWriter,
) -> anyhow::Result<()> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let id = request.get("id").and_then(Value::as_u64);
    let request_session = request
        .pointer("/params/sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match method {
        "initialize" => {
            events.record("initialize_received", json!({}))?;
            respond(
                output,
                id,
                json!({
                    "protocolVersion": 1,
                    "agentCapabilities": { "loadSession": true },
                    "agentInfo": { "name": "watchdog-fixture", "version": "1" }
                }),
            )?;
        }
        "session/new" => {
            let new_id = format!("watchdog-{}", Uuid::new_v4());
            state.session_id = Some(new_id.clone());
            events.record("session_created", json!({ "session_id": new_id }))?;
            respond(
                output,
                id,
                json!({ "sessionId": new_id, "configOptions": [] }),
            )?;
        }
        "session/load" => {
            let loaded =
                request_session.ok_or_else(|| anyhow!("session/load requires sessionId"))?;
            state.session_id = Some(loaded.clone());
            events.record(
                "session_loaded",
                json!({ "session_id": loaded, "pid": std::process::id() }),
            )?;
            respond(output, id, json!({ "configOptions": [] }))?;
        }
        "session/prompt" => {
            let current =
                request_session.ok_or_else(|| anyhow!("session/prompt requires sessionId"))?;
            ensure!(
                state.session_id.as_deref() == Some(current.as_str()),
                "session/prompt used an unknown session"
            );
            let text = request
                .pointer("/params/prompt/0/text")
                .and_then(Value::as_str)
                .or_else(|| {
                    request
                        .pointer("/params/prompt/0/content")
                        .and_then(Value::as_str)
                })
                .unwrap_or("");
            events.record(
                "prompt_received",
                json!({
                    "pid": std::process::id(),
                    "session_id": current,
                    "text": text,
                }),
            )?;
            if let Some(environment) = state.environment.clone() {
                handle_environment_prompt(
                    EnvironmentPrompt {
                        text,
                        session_id: &current,
                        id,
                    },
                    &environment,
                    state,
                    output,
                    events,
                )?;
                return Ok(());
            }
            if text.contains("Create a compact title-bar topic") {
                notify(
                    output,
                    "session/update",
                    json!({
                        "sessionId": current,
                        "update": { "sessionUpdate": "agent_message_chunk", "content": { "text": "{\"topic\":\"Watchdog smoke\"}" } }
                    }),
                )?;
                respond(output, id, json!({ "stopReason": "end_turn" }))?;
                events.record("request_topic_completed", json!({ "session_id": current }))?;
            } else if text == "Continue" {
                notify(
                    output,
                    "session/update",
                    json!({
                        "sessionId": current,
                        "update": { "sessionUpdate": "agent_message_chunk", "content": { "text": RECOVERY_TEXT } }
                    }),
                )?;
                respond(output, id, json!({ "stopReason": "end_turn" }))?;
                events.record("continue_completed", json!({ "session_id": current }))?;
            } else {
                if mode == Mode::CancelEndsTurn {
                    // Mirror the real failure: the agent is blocked inside a
                    // long-running tool call, then blocked with no tool running.
                    // The watchdog must restart itself for the first phase and
                    // only cancel once the tool reports a terminal status.
                    notify(
                        output,
                        "session/update",
                        json!({
                            "sessionId": current,
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "watchdog-tool-1",
                                "title": "Run long external work",
                                "kind": "execute",
                                "status": "in_progress"
                            }
                        }),
                    )?;
                    events.record(
                        "tool_call_started",
                        json!({ "session_id": current, "tool_call_id": "watchdog-tool-1" }),
                    )?;
                    thread::sleep(TOOL_CALL_IN_FLIGHT_DELAY);
                    notify(
                        output,
                        "session/update",
                        json!({
                            "sessionId": current,
                            "update": {
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": "watchdog-tool-1",
                                "status": "completed"
                            }
                        }),
                    )?;
                    events.record(
                        "tool_call_completed",
                        json!({ "session_id": current, "tool_call_id": "watchdog-tool-1" }),
                    )?;
                }
                state.pending_prompt = id;
            }
        }
        "session/cancel" => {
            let current =
                request_session.ok_or_else(|| anyhow!("session/cancel requires sessionId"))?;
            ensure!(
                state.session_id.as_deref() == Some(current.as_str()),
                "session/cancel used an unknown session"
            );
            events.record(
                "cancel_received",
                json!({
                    "pid": std::process::id(),
                    "session_id": current,
                }),
            )?;
            if mode == Mode::AcknowledgeCancel {
                if let Some(prompt_id) = state.pending_prompt.take() {
                    respond(
                        output,
                        Some(prompt_id),
                        json!({ "stopReason": "cancelled" }),
                    )?;
                }
                events.record("cancel_acknowledged", json!({ "session_id": current }))?;
            } else if mode == Mode::CancelEndsTurn {
                // The real backend acknowledges a cancel by ending the turn
                // normally with a trailing notice instead of reporting
                // `cancelled`.
                if let Some(prompt_id) = state.pending_prompt.take() {
                    notify(
                        output,
                        "session/update",
                        json!({
                            "sessionId": current,
                            "update": { "sessionUpdate": "agent_message_chunk", "content": { "text": "Info: Operation cancelled by user" } }
                        }),
                    )?;
                    respond(output, Some(prompt_id), json!({ "stopReason": "end_turn" }))?;
                }
                events.record("cancel_acknowledged", json!({ "session_id": current }))?;
            }
        }
        _ => {
            if let Some(id) = id {
                write_json_line(
                    output,
                    &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}}),
                )?;
            }
        }
    }
    Ok(())
}

fn handle_environment_prompt(
    prompt: EnvironmentPrompt<'_>,
    environment: &EnvironmentServe,
    state: &mut RequestState,
    output: &mut impl Write,
    events: &mut EventWriter,
) -> anyhow::Result<()> {
    state.environment_prompt_count += 1;
    if prompt.text == "Continue" {
        notify(
            output,
            "session/update",
            json!({
                "sessionId": prompt.session_id,
                "update": { "sessionUpdate": "agent_message_chunk", "content": { "text": RECOVERY_TEXT } }
            }),
        )?;
        respond(output, prompt.id, json!({ "stopReason": "end_turn" }))?;
        events.record(
            "environment_continue_completed",
            json!({
                "session_id": prompt.session_id,
                "agent": environment.agent,
                "pid": std::process::id(),
            }),
        )?;
        return Ok(());
    }
    if environment.agent == "planner"
        && prompt.text.contains("planner retry")
        && state.environment_prompt_count == 1
    {
        notify(
            output,
            "session/update",
            json!({
                "sessionId": prompt.session_id,
                "update": { "sessionUpdate": "agent_message_chunk", "content": { "text": "retry without required frontmatter" } }
            }),
        )?;
        respond(output, prompt.id, json!({ "stopReason": "end_turn" }))?;
        events.record(
            "environment_retry_requested",
            json!({
                "session_id": prompt.session_id,
                "agent": environment.agent,
                "pid": std::process::id(),
            }),
        )?;
        return Ok(());
    }
    if environment.agent == "planner" && prompt.text.contains("planner watchdog") {
        state.pending_prompt = prompt.id;
        events.record(
            "environment_watchdog_stalled",
            json!({
                "session_id": prompt.session_id,
                "agent": environment.agent,
                "pid": std::process::id(),
            }),
        )?;
        return Ok(());
    }
    let body = format!(
        "---\nstatus: success\nsummary: environment {} completed\n---\nenvironment {} completed",
        environment.agent, environment.agent
    );
    notify(
        output,
        "session/update",
        json!({
            "sessionId": prompt.session_id,
            "update": { "sessionUpdate": "agent_message_chunk", "content": { "text": body } }
        }),
    )?;
    respond(output, prompt.id, json!({ "stopReason": "end_turn" }))?;
    events.record(
        "environment_prompt_completed",
        json!({
            "session_id": prompt.session_id,
            "agent": environment.agent,
            "environment": environment_state(environment.profile),
        }),
    )?;
    Ok(())
}

fn probe_environment(args: ProbeEnvironmentArgs) -> anyhow::Result<()> {
    let state = environment_state(args.profile);
    write_json(&args.output, &state)?;
    println!("{}", environment_state_line(args.profile, &state));
    Ok(())
}

fn environment_state(profile: EnvironmentProfile) -> serde_json::Map<String, Value> {
    profile
        .names()
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                Value::String(
                    if std::env::var_os(name).is_some() {
                        "set"
                    } else {
                        "missing"
                    }
                    .to_owned(),
                ),
            )
        })
        .collect()
}

fn environment_state_line(
    profile: EnvironmentProfile,
    state: &serde_json::Map<String, Value>,
) -> String {
    profile
        .names()
        .iter()
        .map(|name| {
            let state = state
                .get(*name)
                .and_then(Value::as_str)
                .unwrap_or("missing");
            format!("{name}={state}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn respond(output: &mut impl Write, id: Option<u64>, result: Value) -> anyhow::Result<()> {
    let id = id.ok_or_else(|| anyhow!("fixture request requires a numeric id"))?;
    write_json_line(
        output,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    )
}

fn notify(output: &mut impl Write, method: &str, params: Value) -> anyhow::Result<()> {
    write_json_line(
        output,
        &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    )
}

fn write_json_line(output: &mut impl Write, value: &Value) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn identity_server(listener: TcpListener, identity: Identity, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let matches = authenticate_stream(stream, &identity, &shutdown);
                if !matches {
                    continue;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(_) => break,
        }
    }
}

fn authenticate_stream(mut stream: TcpStream, identity: &Identity, shutdown: &AtomicBool) -> bool {
    let mut line = String::new();
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let read = BufReader::new(&mut stream).read_line(&mut line);
    let challenge = read
        .ok()
        .filter(|count| *count > 0)
        .and_then(|_| serde_json::from_str::<CleanupChallenge>(&line).ok());
    let matches = challenge.as_ref().is_some_and(|challenge| {
        challenge.invocation_token == identity.invocation_token
            && challenge.start_nonce == identity.start_nonce
            && challenge.pid == identity.pid
            && challenge.executable == identity.executable
    });
    let response = if matches {
        json!({ "ok": true })
    } else {
        json!({ "ok": false })
    };
    let _ = write_json_line(&mut stream, &response);
    if matches && challenge.and_then(|challenge| challenge.action).as_deref() == Some("shutdown") {
        shutdown.store(true, Ordering::SeqCst);
    }
    let _ = stream.shutdown(Shutdown::Both);
    matches
}

fn verify(args: VerifyArgs) -> anyhow::Result<()> {
    verify_with_scenario_runner(&args, run_scenario)
}

fn verify_environment(args: VerifyArgs) -> anyhow::Result<()> {
    prepare_environment_workspace(&args.workspace, &args.cowboy)?;
    let result = (|| {
        write_allowed_environment_files(&args)?;
        let run = run_cowboy(
            &args.cowboy,
            &args.workspace,
            ["run", "--workflow", "allowed_env", "allowed env smoke"],
            "cowboy-run",
            &SYNTHETIC_ENVIRONMENT_VALUES,
            args.soft_deadline_seconds,
        )?;
        let run_id = run_id_from_stdout(&run.stdout)?;
        run_cowboy(
            &args.cowboy,
            &args.workspace,
            ["answer", &run_id, "continue", "continue"],
            "cowboy-answer",
            &SYNTHETIC_ENVIRONMENT_VALUES,
            args.hard_deadline_seconds,
        )?;
        let export = Command::new(fs::canonicalize(&args.cowboy)?)
            .current_dir(&args.workspace)
            .args(["--config", "config.toml", "export", &run_id])
            .envs(SYNTHETIC_ENVIRONMENT_VALUES)
            .output()
            .context("export allowed environment smoke run")?;
        ensure!(export.status.success(), "allowed environment export failed");
        let generated_export = fs::read_dir(&args.workspace)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("cowboy-export-") && name.ends_with(".html")
                    })
            })
            .ok_or_else(|| anyhow!("allowed environment export was not written"))?;
        fs::rename(generated_export, args.workspace.join("export.html"))?;

        let command = read_environment_matrix(&args.workspace.join("command-matrix.json"))?;
        assert_environment_matrix(
            &command,
            &[
                ("COWBOY_TEST_GLOBAL", "set"),
                ("COWBOY_TEST_PLANNER", "missing"),
                ("COWBOY_TEST_IMPLEMENTER", "missing"),
                ("COWBOY_TEST_UNAPPROVED", "missing"),
            ],
        )?;
        let events = read_events(&args.workspace.join("fixture-events.jsonl"))?;
        assert_agent_environment(&events, "planner", true, false)?;
        assert_agent_environment(&events, "implementer", false, true)?;
        assert_environment_lifecycle(&events)?;
        assert_environment_artifacts_clean(&args.workspace)?;
        println!("command global=set planner=missing implementer=missing unapproved=missing");
        println!(
            "planner.retry global=set planner=set implementer=missing unapproved=missing same_pid=true"
        );
        println!(
            "planner.resume global=set planner=set implementer=missing unapproved=missing session_loaded=true"
        );
        println!(
            "planner.replacement global=set planner=set implementer=missing unapproved=missing resumed_session=true old_pid_exited=true"
        );
        println!("implementer global=set planner=missing implementer=set unapproved=missing");
        println!(
            "artifacts sqlite=clean events=clean logs=clean fixture_jsonl=clean stdout_stderr=clean export=clean"
        );
        Ok(())
    })();
    match result {
        Ok(()) => cleanup(&args.workspace),
        Err(error) => Err(error.context(format!(
            "allowed env evidence preserved at {}",
            args.workspace.display()
        ))),
    }
}

fn verify_default_allowed_env(args: DefaultEnvironmentVerifyArgs) -> anyhow::Result<()> {
    prepare_environment_workspace(&args.workspace, &args.cowboy)?;
    let result = (|| {
        let started = Instant::now();
        write_default_environment_files(&args)?;
        let values = default_environment_values(&args.workspace)?;
        let value_refs: Vec<_> = values
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let output = run_cowboy(
            &args.cowboy,
            &args.workspace,
            [
                "run",
                "--workflow",
                "default_allowed_env",
                "default allowed env smoke",
            ],
            "cowboy",
            &value_refs,
            args.deadline_seconds,
        )?;
        ensure!(
            output.status.success(),
            "default allowed environment smoke run failed"
        );
        ensure!(
            started.elapsed() <= Duration::from_secs(args.deadline_seconds),
            "default allowed environment smoke run exceeded {} seconds",
            args.deadline_seconds
        );
        let command = read_environment_matrix(&args.workspace.join("command-matrix.json"))?;
        assert_environment_matrix(
            &command,
            &[
                ("PATH", "set"),
                ("PATHEXT", "set"),
                ("SystemRoot", "set"),
                ("USERPROFILE", "set"),
                ("LOCALAPPDATA", "set"),
                ("APPDATA", "set"),
                ("TEMP", "set"),
                ("TMP", "set"),
                ("HOME", "set"),
                ("COWBOY_TEST_UNAPPROVED", "missing"),
            ],
        )?;
        let events = read_events(&args.workspace.join("fixture-events.jsonl"))?;
        let agent = events
            .iter()
            .find(|event| event.event == "process_started")
            .ok_or_else(|| anyhow!("default environment fixture did not start"))?;
        let state = agent.details["environment"]
            .as_object()
            .ok_or_else(|| anyhow!("default environment fixture did not record its environment"))?;
        assert_environment_matrix(
            state,
            &[
                ("PATH", "set"),
                ("PATHEXT", "set"),
                ("SystemRoot", "set"),
                ("USERPROFILE", "set"),
                ("LOCALAPPDATA", "set"),
                ("APPDATA", "set"),
                ("TEMP", "set"),
                ("TMP", "set"),
                ("HOME", "set"),
                ("COWBOY_TEST_UNAPPROVED", "missing"),
            ],
        )?;
        println!(
            "omitted_allowed_env command=started default_agent=started defaults=9 unapproved=missing workflow=success"
        );
        Ok(())
    })();
    match result {
        Ok(()) => cleanup(&args.workspace),
        Err(error) => Err(error.context(format!(
            "default allowed env evidence preserved at {}",
            args.workspace.display()
        ))),
    }
}

fn prepare_environment_workspace(workspace: &Path, cowboy: &Path) -> anyhow::Result<()> {
    ensure!(
        !workspace.exists(),
        "refusing to overwrite existing smoke workspace {}",
        workspace.display()
    );
    ensure!(
        cowboy.is_file(),
        "Cowboy binary does not exist: {}",
        cowboy.display()
    );
    fs::create_dir_all(workspace.join("identities"))?;
    fs::create_dir_all(workspace.join("workflows"))?;
    fs::create_dir_all(workspace.join("state"))?;
    fs::write(workspace.join(".cowboy-watchdog-smoke"), WORKSPACE_MARKER)?;
    Ok(())
}

fn write_allowed_environment_files(args: &VerifyArgs) -> anyhow::Result<()> {
    let fixture = canonical_current_exe()?;
    let workspace = &args.workspace;
    let quote = |path: &Path| serde_json::to_string(&path.to_string_lossy()).unwrap();
    let fixture = serde_json::to_string(&fixture)?;
    let events = quote(Path::new("fixture-events.jsonl"));
    let identities = quote(Path::new("identities"));
    let token = Uuid::new_v4().to_string();
    let config = format!(
        "state_dir = {state}\nworkflow_store = {store}\nworkflow_dirs = [{workflows}]\nallowed_env = [\"COWBOY_TEST_GLOBAL\"]\n\n[[agents]]\nname = \"default\"\ncommand = {fixture}\nargs = [\"serve-environment\", \"--agent\", \"default\", \"--events\", {events}, \"--invocation-token\", \"{token}\", \"--identity-dir\", {identities}]\n\n[agents.model]\nid = \"fixture\"\nprovider = \"fixture\"\n\n[[agents]]\nname = \"planner\"\ncommand = {fixture}\nargs = [\"serve-environment\", \"--agent\", \"planner\", \"--mode\", \"ignore-cancel\", \"--events\", {events}, \"--invocation-token\", \"{token}\", \"--identity-dir\", {identities}]\nallowed_env = [\"COWBOY_TEST_PLANNER\"]\n\n[agents.model]\nid = \"fixture\"\nprovider = \"fixture\"\n\n[agents.watchdog]\nresponse_timeout_seconds = {response}\ncancel_timeout_seconds = {cancel}\nrecovery_operation_timeout_seconds = {recovery}\n\n[[agents]]\nname = \"implementer\"\ncommand = {fixture}\nargs = [\"serve-environment\", \"--agent\", \"implementer\", \"--events\", {events}, \"--invocation-token\", \"{token}\", \"--identity-dir\", {identities}]\nallowed_env = [\"COWBOY_TEST_IMPLEMENTER\"]\n\n[agents.model]\nid = \"fixture\"\nprovider = \"fixture\"\n",
        state = quote(Path::new("state")),
        store = quote(Path::new("state/data.db")),
        workflows = quote(Path::new("workflows")),
        response = args.response_timeout_seconds,
        cancel = args.cancel_timeout_seconds,
        recovery = args.recovery_operation_timeout_seconds,
    );
    fs::write(workspace.join("config.toml"), config)?;
    let probe = serde_json::to_string(&canonical_current_exe()?)?;
    let matrix = serde_json::to_string("command-matrix.json")?;
    fs::write(
        workspace.join("workflows/allowed_env.lua"),
        format!(
            "local planner = role(\"planner\", {{ agent = \"planner\", instructions = \"Return the requested result.\" }})\nlocal implementer = role(\"implementer\", {{ agent = \"implementer\", instructions = \"Return the requested result.\" }})\nlocal command = step(\"command\", {{ run = function(ctx) return action.command {{ program = {probe}, args = {{ \"probe-environment\", \"--output\", {matrix} }} }} end }})\nlocal retry = step(\"retry\", {{ role = planner, run = function(ctx) return action.agent {{ role = planner, prompt = \"planner retry\", output = {{ status = {{ \"success\" }}, fields = {{ summary = \"string\" }}, required_fields = {{ \"summary\" }} }} }} end }})\nlocal ask = step(\"ask\", {{ run = function(ctx) return action.ask_user {{ id = \"continue\", message = \"Continue?\", choices = {{ continue = \"Continue\" }} }} end }})\nlocal resumed = step(\"resumed\", {{ role = planner, run = function(ctx) return action.agent {{ role = planner, prompt = \"planner resumed\", output = {{ status = {{ \"success\" }}, fields = {{ summary = \"string\" }}, required_fields = {{ \"summary\" }} }} }} end }})\nlocal watchdog = step(\"watchdog\", {{ role = planner, run = function(ctx) return action.agent {{ role = planner, prompt = \"planner watchdog\", output = {{ status = {{ \"success\" }}, fields = {{ summary = \"string\" }}, required_fields = {{ \"summary\" }} }} }} end }})\nlocal implement = step(\"implement\", {{ role = implementer, run = function(ctx) return action.agent {{ role = implementer, prompt = \"implement\", output = {{ status = {{ \"success\" }}, fields = {{ summary = \"string\" }}, required_fields = {{ \"summary\" }} }} }} end }})\nlocal done = step(\"done\", {{ run = function(ctx) return action.status {{ status = \"success\", fields = ctx.prev.fields, body = ctx.prev.body }} end }})\ncommand:on(\"success\", retry)\nretry:on(\"success\", ask)\nask:on(\"answered\", resumed)\nresumed:on(\"success\", watchdog)\nwatchdog:on(\"success\", implement)\nimplement:on(\"success\", done)\nreturn workflow(\"allowed_env\", command)\n"
        ),
    )?;
    Ok(())
}

fn write_default_environment_files(args: &DefaultEnvironmentVerifyArgs) -> anyhow::Result<()> {
    let fixture = serde_json::to_string(&canonical_current_exe()?)?;
    let workspace = &args.workspace;
    let quote = |path: &Path| serde_json::to_string(&path.to_string_lossy()).unwrap();
    let events = quote(Path::new("fixture-events.jsonl"));
    let identities = quote(Path::new("identities"));
    let token = Uuid::new_v4().to_string();
    fs::write(
        workspace.join("config.toml"),
        format!(
            "state_dir = {state}\nworkflow_store = {store}\nworkflow_dirs = [{workflows}]\n\n[[agents]]\nname = \"default\"\ncommand = {fixture}\nargs = [\"serve-environment\", \"--agent\", \"default\", \"--environment-profile\", \"default\", \"--events\", {events}, \"--invocation-token\", \"{token}\", \"--identity-dir\", {identities}]\n\n[agents.model]\nid = \"fixture\"\nprovider = \"fixture\"\n",
            state = quote(Path::new("state")),
            store = quote(Path::new("state/data.db")),
            workflows = quote(Path::new("workflows")),
        ),
    )?;
    let probe = serde_json::to_string(&canonical_current_exe()?)?;
    let matrix = serde_json::to_string("command-matrix.json")?;
    fs::write(
        workspace.join("workflows/default_allowed_env.lua"),
        format!(
            "local fixture = role(\"fixture\", {{ agent = \"default\", instructions = \"Return the requested result.\" }})\nlocal command = step(\"command\", {{ run = function(ctx) return action.command {{ program = {probe}, args = {{ \"probe-environment\", \"--environment-profile\", \"default\", \"--output\", {matrix} }} }} end }})\nlocal agent = step(\"agent\", {{ role = fixture, run = function(ctx) return action.agent {{ role = fixture, prompt = \"default environment\", output = {{ status = {{ \"success\" }}, fields = {{ summary = \"string\" }}, required_fields = {{ \"summary\" }} }} }} end }})\nlocal done = step(\"done\", {{ run = function(ctx) return action.status {{ status = \"success\", fields = ctx.prev.fields, body = ctx.prev.body }} end }})\ncommand:on(\"success\", agent)\nagent:on(\"success\", done)\nreturn workflow(\"default_allowed_env\", command)\n"
        ),
    )?;
    Ok(())
}

fn run_cowboy<const N: usize>(
    cowboy: &Path,
    workspace: &Path,
    args: [&str; N],
    artifact_stem: &str,
    environment: &[(&str, &str)],
    deadline_seconds: u64,
) -> anyhow::Result<std::process::Output> {
    let mut child = Command::new(fs::canonicalize(cowboy)?)
        .current_dir(workspace)
        .arg("--config")
        .arg("config.toml")
        .args(args)
        .envs(environment.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run Cowboy {}", args.join(" ")))?;
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() > Duration::from_secs(deadline_seconds) {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "Cowboy {} exceeded {} seconds",
                args.join(" "),
                deadline_seconds
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output()?;
    fs::write(
        workspace.join(format!("{artifact_stem}.stdout")),
        &output.stdout,
    )?;
    fs::write(
        workspace.join(format!("{artifact_stem}.stderr")),
        &output.stderr,
    )?;
    ensure!(output.status.success(), "Cowboy {} failed", args.join(" "));
    Ok(output)
}

fn run_id_from_stdout(stdout: &[u8]) -> anyhow::Result<String> {
    let text = std::str::from_utf8(stdout).context("Cowboy run stdout was not UTF-8")?;
    text.lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("run=")
                .and_then(|rest| rest.split_whitespace().next())
                .map(str::to_owned)
        })
        .ok_or_else(|| anyhow!("Cowboy run stdout did not contain a run id"))
}

fn default_environment_values(workspace: &Path) -> anyhow::Result<Vec<(&'static str, String)>> {
    let mut values = Vec::new();
    for name in &DEFAULT_ENVIRONMENT_NAMES[..9] {
        let value = std::env::var(name).unwrap_or_else(|_| {
            workspace
                .join(format!("{name}-placeholder"))
                .to_string_lossy()
                .into_owned()
        });
        #[cfg(windows)]
        ensure!(
            *name != "SystemRoot" || std::env::var_os(name).is_some(),
            "SystemRoot must be set for the default allowed-env verifier on Windows"
        );
        values.push((*name, value));
    }
    values.push((
        "COWBOY_TEST_UNAPPROVED",
        "cowboy-default-unapproved-marker".to_owned(),
    ));
    Ok(values)
}

fn read_environment_matrix(path: &Path) -> anyhow::Result<serde_json::Map<String, Value>> {
    let value: Value = serde_json::from_reader(
        File::open(path).with_context(|| format!("open command matrix {}", path.display()))?,
    )?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("command matrix is not a JSON object"))
}

fn assert_environment_matrix(
    state: &serde_json::Map<String, Value>,
    expected: &[(&str, &str)],
) -> anyhow::Result<()> {
    for (name, expected_state) in expected {
        ensure!(
            state.get(*name).and_then(Value::as_str) == Some(*expected_state),
            "environment state for {name} was not {expected_state}"
        );
    }
    Ok(())
}

fn assert_agent_environment(
    events: &[FixtureEvent],
    agent: &str,
    planner_set: bool,
    implementer_set: bool,
) -> anyhow::Result<()> {
    let starts: Vec<_> = events
        .iter()
        .filter(|event| {
            event.event == "process_started" && event.details["agent"].as_str() == Some(agent)
        })
        .collect();
    ensure!(!starts.is_empty(), "{agent} fixture never started");
    for event in starts {
        let state = event.details["environment"]
            .as_object()
            .ok_or_else(|| anyhow!("{agent} fixture did not record environment state"))?;
        assert_environment_matrix(
            state,
            &[
                ("COWBOY_TEST_GLOBAL", "set"),
                (
                    "COWBOY_TEST_PLANNER",
                    if planner_set { "set" } else { "missing" },
                ),
                (
                    "COWBOY_TEST_IMPLEMENTER",
                    if implementer_set { "set" } else { "missing" },
                ),
                ("COWBOY_TEST_UNAPPROVED", "missing"),
            ],
        )?;
    }
    Ok(())
}

fn assert_environment_lifecycle(events: &[FixtureEvent]) -> anyhow::Result<()> {
    let planner_pids: Vec<u32> = events
        .iter()
        .filter(|event| {
            event.event == "process_started" && event.details["agent"].as_str() == Some("planner")
        })
        .filter_map(|event| event.details["pid"].as_u64().map(|pid| pid as u32))
        .collect();
    ensure!(
        planner_pids.len() >= 3,
        "planner did not start for initial, resumed, and replacement sessions"
    );
    let retry_pid = events
        .iter()
        .find(|event| event.event == "environment_retry_requested")
        .and_then(|event| event.details["pid"].as_u64())
        .ok_or_else(|| anyhow!("planner did not request the recoverable retry"))?
        as u32;
    let retry_prompts = events
        .iter()
        .filter(|event| {
            event.event == "prompt_received"
                && event.details["pid"].as_u64() == Some(retry_pid as u64)
                && event.details["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("planner retry"))
        })
        .count();
    ensure!(
        retry_prompts >= 2,
        "planner retry did not remain on the original fixture process"
    );

    let loaded_pid = events
        .iter()
        .find(|event| event.event == "session_loaded")
        .and_then(|event| event.details["pid"].as_u64())
        .ok_or_else(|| anyhow!("persisted resume did not load a planner session"))?
        as u32;
    ensure!(
        loaded_pid != retry_pid,
        "persisted CLI resume reused the original planner process"
    );
    let stalled_pid = events
        .iter()
        .find(|event| event.event == "environment_watchdog_stalled")
        .and_then(|event| event.details["pid"].as_u64())
        .ok_or_else(|| anyhow!("planner watchdog scenario did not stall"))?
        as u32;
    let replacement = events
        .iter()
        .filter(|event| {
            event.event == "process_started"
                && event.details["agent"].as_str() == Some("planner")
                && event.details["resume_session_id"].is_string()
        })
        .collect::<Vec<_>>();
    ensure!(
        replacement.len() == 1,
        "watchdog hard recovery did not start exactly one resumed planner replacement"
    );
    let replacement_pid = replacement[0].details["pid"]
        .as_u64()
        .ok_or_else(|| anyhow!("replacement fixture had no PID"))? as u32;
    ensure!(
        replacement_pid != stalled_pid && !process_is_alive(stalled_pid),
        "watchdog hard recovery did not terminate the stalled planner process"
    );
    Ok(())
}

fn assert_environment_artifacts_clean(workspace: &Path) -> anyhow::Result<()> {
    let artifact_groups = [
        (
            "sqlite",
            vec![
                workspace.join("state/data.db"),
                workspace.join("state/data.db-wal"),
                workspace.join("state/data.db-shm"),
            ],
        ),
        (
            "events",
            files_in_directory(&workspace.join("state/events"))?,
        ),
        ("logs", files_in_directory(&workspace.join("state/logs"))?),
        (
            "fixture_jsonl",
            vec![
                workspace.join("fixture-events.jsonl"),
                workspace.join("command-matrix.json"),
            ],
        ),
        (
            "stdout_stderr",
            vec![
                workspace.join("cowboy-run.stdout"),
                workspace.join("cowboy-run.stderr"),
                workspace.join("cowboy-answer.stdout"),
                workspace.join("cowboy-answer.stderr"),
            ],
        ),
        ("export", vec![workspace.join("export.html")]),
    ];
    for (group, files) in artifact_groups {
        for path in files {
            if !path.exists() {
                continue;
            }
            let contents = fs::read(&path)
                .with_context(|| format!("read {group} artifact {}", path.display()))?;
            for (_, marker) in SYNTHETIC_ENVIRONMENT_VALUES {
                ensure!(
                    !contents
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes()),
                    "{group} artifact leaked an environment marker: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn files_in_directory(directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    fs::read_dir(directory)?
        .map(|entry| Ok(entry?.path()))
        .collect()
}
fn verify_with_scenario_runner(
    args: &VerifyArgs,
    mut scenario_runner: impl FnMut(&VerifyArgs, &str, Mode, u64) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    ensure!(
        !args.workspace.exists(),
        "refusing to overwrite existing smoke workspace {}",
        args.workspace.display()
    );
    ensure!(
        args.cowboy.is_file(),
        "Cowboy binary does not exist: {}",
        args.cowboy.display()
    );
    fs::create_dir_all(&args.workspace)?;
    fs::write(
        args.workspace.join(".cowboy-watchdog-smoke"),
        WORKSPACE_MARKER,
    )?;
    let result: anyhow::Result<()> = (|| {
        scenario_runner(
            args,
            "soft",
            Mode::AcknowledgeCancel,
            args.soft_deadline_seconds,
        )?;
        scenario_runner(args, "hard", Mode::IgnoreCancel, args.hard_deadline_seconds)?;
        scenario_runner(
            args,
            "end-turn-cancel",
            Mode::CancelEndsTurn,
            args.soft_deadline_seconds,
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            cleanup(&args.workspace)?;
            Ok(())
        }
        Err(error) => Err(error.context(format!(
            "watchdog evidence preserved at {}",
            args.workspace.display()
        ))),
    }
}

fn run_scenario(
    args: &VerifyArgs,
    name: &str,
    mode: Mode,
    deadline_seconds: u64,
) -> anyhow::Result<()> {
    let scenario = args.workspace.join(name);
    fs::create_dir_all(scenario.join("workflows"))?;
    let fixture = canonical_current_exe()?;
    let token = Uuid::new_v4().to_string();
    write_scenario_files(args, &scenario, &fixture, &token, mode)?;

    let started = Instant::now();
    let mut child = Command::new(&args.cowboy)
        .args([
            "--config",
            scenario
                .join("config.toml")
                .to_str()
                .ok_or_else(|| anyhow!("non-UTF8 config path"))?,
            "run",
            "--workflow",
            "watchdog_smoke",
            "watchdog smoke",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("launch Cowboy smoke scenario")?;
    while child.try_wait()?.is_none() && started.elapsed() < Duration::from_secs(deadline_seconds) {
        thread::sleep(Duration::from_millis(25));
    }
    if child.try_wait()?.is_none() {
        let _ = child.kill();
        bail!("{name} scenario exceeded {deadline_seconds} seconds");
    }
    let output = child.wait_with_output()?;
    fs::write(scenario.join("cowboy.stdout"), &output.stdout)?;
    fs::write(scenario.join("cowboy.stderr"), &output.stderr)?;
    ensure!(
        output.status.success(),
        "{name} scenario Cowboy process failed"
    );
    verify_scenario_events(&scenario, mode)?;
    verify_scenario_logs(&scenario, mode)?;
    Ok(())
}

fn write_scenario_files(
    args: &VerifyArgs,
    scenario: &Path,
    fixture: &str,
    token: &str,
    mode: Mode,
) -> anyhow::Result<()> {
    let mode = mode.as_arg();
    let state = scenario.join("state");
    let store = scenario.join("workflow.redb");
    let workflows = scenario.join("workflows");
    let events = scenario.join("events.jsonl");
    let identities = scenario.join("identities");
    fs::create_dir_all(&state)?;
    fs::create_dir_all(&workflows)?;
    let quote = |value: &Path| serde_json::to_string(&value.to_string_lossy()).unwrap();
    let config = format!(
        "state_dir = {state}\nworkflow_store = {store}\nworkflow_dirs = [{workflows}]\n\n[[agents]]\nname = \"default\"\ncommand = {fixture}\nargs = [\"serve\", \"--mode\", \"{mode}\", \"--events\", {events}, \"--invocation-token\", \"{token}\", \"--identity-dir\", {identities}]\n\n[agents.watchdog]\nresponse_timeout_seconds = {response}\ncancel_timeout_seconds = {cancel}\nrecovery_operation_timeout_seconds = {recovery}\n",
        state = quote(&state),
        store = quote(&store),
        workflows = quote(&workflows),
        fixture = serde_json::to_string(fixture)?,
        events = quote(&events),
        identities = quote(&identities),
        response = args.response_timeout_seconds,
        cancel = args.cancel_timeout_seconds,
        recovery = args.recovery_operation_timeout_seconds,
    );
    fs::write(scenario.join("config.toml"), config)?;
    fs::write(
        workflows.join("watchdog_smoke.lua"),
        "local fixture = role(\"fixture\", { agent = \"default\", instructions = \"Return the requested result.\" })\nlocal smoke = step(\"watchdog_smoke\", {\n  role = fixture,\n  run = function(ctx)\n    return action.agent {\n      role = fixture,\n      prompt = \"watchdog smoke\",\n      output = { status = { \"success\" }, fields = { summary = \"string\" }, required_fields = { \"summary\" } }\n    }\n  end\n})\nlocal done = step(\"done\", {\n  run = function(ctx)\n    return action.status { status = \"success\", body = ctx.prev.body, fields = ctx.prev.fields }\n  end\n})\nsmoke:on(\"success\", done)\nreturn workflow(\"watchdog_smoke\", smoke)\n",
    )?;
    Ok(())
}

fn verify_scenario_events(scenario: &Path, mode: Mode) -> anyhow::Result<()> {
    let events = read_events(&scenario.join("events.jsonl"))?;
    let started: Vec<_> = events
        .iter()
        .filter(|event| event.event == "process_started")
        .collect();
    ensure!(!started.is_empty(), "scenario recorded no fixture process");
    ensure!(
        events.iter().any(|event| event.event == "cancel_received"),
        "scenario recorded no cancel"
    );
    ensure!(
        events
            .iter()
            .any(|event| event.event == "continue_completed"),
        "scenario recorded no recovery Continue"
    );
    if mode != Mode::IgnoreCancel {
        let recovered_pid = events
            .iter()
            .find(|event| event.event == "cancel_received")
            .and_then(|event| event.details["pid"].as_u64());
        let recovered_starts = started
            .iter()
            .filter(|event| event.details["pid"].as_u64() == recovered_pid)
            .count();
        ensure!(
            recovered_starts == 1,
            "soft recovery started a replacement fixture"
        );
        let created = events
            .iter()
            .find(|event| event.event == "cancel_received")
            .and_then(|event| event.details["session_id"].as_str());
        let continued = events
            .iter()
            .find(|event| event.event == "continue_completed")
            .and_then(|event| event.details["session_id"].as_str());
        ensure!(
            created == continued,
            "soft recovery changed the ACP session"
        );
    } else {
        let created = events
            .iter()
            .find(|event| event.event == "cancel_received")
            .and_then(|event| event.details["session_id"].as_str());
        let relevant: Vec<_> = started
            .iter()
            .filter(|event| {
                event.details["resume_session_id"].as_str() == created
                    || events.iter().any(|candidate| {
                        candidate.event == "cancel_received"
                            && candidate.details["pid"] == event.details["pid"]
                    })
            })
            .collect();
        ensure!(
            relevant.len() == 2,
            "hard recovery did not start exactly one replacement fixture"
        );
        let first_pid = relevant[0].details["pid"].as_u64();
        let second_pid = relevant[1].details["pid"].as_u64();
        ensure!(
            first_pid != second_pid,
            "hard recovery reused the old fixture process"
        );
        ensure!(
            relevant[1].details["resume_session_id"].is_string(),
            "hard replacement did not receive --resume=<session-id>"
        );
        let loaded = relevant[1].details["resume_session_id"].as_str();
        ensure!(
            created == loaded,
            "hard recovery loaded a different ACP session"
        );
    }
    Ok(())
}

fn verify_scenario_logs(scenario: &Path, mode: Mode) -> anyhow::Result<()> {
    let log_dir = scenario.join("state/logs");
    let mut log = String::new();
    for entry in fs::read_dir(&log_dir)
        .with_context(|| format!("read watchdog log directory {}", log_dir.display()))?
    {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("cowboy") && name.ends_with(".log"))
        {
            log.push_str(&fs::read_to_string(path)?);
        }
    }
    ensure!(
        !log.is_empty(),
        "watchdog log directory contained no Cowboy log"
    );
    for event in ["agent_watchdog_timeout", "agent_watchdog_cancel_sent"] {
        ensure!(log.contains(event), "watchdog log omitted {event}");
    }
    if mode != Mode::IgnoreCancel {
        ensure!(
            log.contains("agent_watchdog_soft_recovered"),
            "watchdog log omitted agent_watchdog_soft_recovered"
        );
        ensure!(
            !log.contains("agent_watchdog_force_terminated"),
            "soft recovery force-terminated the transport"
        );
        if mode == Mode::CancelEndsTurn {
            // The only reproducible observation point for the tool-wait restart
            // and the last-activity diagnostics: unit tests install no tracing
            // subscriber, so `tracing::warn!` emits nothing there.
            for field in [
                "agent_watchdog_tool_wait",
                "last_tool_call_status",
                "seconds_since_last_activity",
            ] {
                ensure!(log.contains(field), "watchdog log omitted {field}");
            }
        }
    } else {
        for event in [
            "agent_watchdog_force_terminated",
            "agent_watchdog_transport_resumed",
        ] {
            ensure!(log.contains(event), "watchdog log omitted {event}");
        }
    }
    Ok(())
}

fn cleanup(workspace: &Path) -> anyhow::Result<()> {
    ensure!(
        workspace.is_dir(),
        "smoke workspace does not exist: {}",
        workspace.display()
    );
    ensure!(
        fs::read_to_string(workspace.join(".cowboy-watchdog-smoke"))
            .ok()
            .as_deref()
            == Some(WORKSPACE_MARKER),
        "cleanup refused: {} is not a recognized watchdog smoke workspace",
        workspace.display()
    );
    let identities = find_identity_files(workspace)?;
    let mut failures = Vec::new();
    for path in identities {
        match cleanup_identity(&path) {
            Ok(()) => {}
            Err(error) => failures.push(format!("{}: {error:#}", path.display())),
        }
    }
    if !failures.is_empty() {
        bail!(
            "cleanup refused; evidence preserved:\n{}",
            failures.join("\n")
        );
    }
    fs::remove_dir_all(workspace)?;
    Ok(())
}

fn find_identity_files(workspace: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    inspect_identity_directory(&workspace.join("identities"), &mut files)?;
    for scenario in ["soft", "hard", "end-turn-cancel"] {
        let scenario_directory = workspace.join(scenario);
        if !path_exists_without_following(&scenario_directory)? {
            continue;
        }
        ensure_regular_directory(&scenario_directory)?;
        inspect_identity_directory(&scenario_directory.join("identities"), &mut files)?;
    }
    Ok(files)
}

fn inspect_identity_directory(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if !path_exists_without_following(directory)? {
        return Ok(());
    }
    ensure_regular_directory(directory)?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("read identity metadata {}", path.display()))?;
        ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "identity entry must be a regular .json file: {}",
            path.display()
        );
        files.push(path);
    }
    Ok(())
}

fn path_exists_without_following(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn ensure_regular_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("read identity directory metadata {}", path.display()))?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "identity directory must be a non-symlink directory: {}",
        path.display()
    );
    Ok(())
}

fn cleanup_identity(path: &Path) -> anyhow::Result<()> {
    let identity: Identity = serde_json::from_reader(File::open(path)?)?;
    // A hard-recovery fixture is intentionally force-terminated before its
    // replacement starts. It is no longer a live process to authenticate.
    if !process_is_alive(identity.pid) {
        return Ok(());
    }
    ensure!(
        canonical_pid_executable(identity.pid)? == identity.executable,
        "recorded executable does not match PID {}",
        identity.pid
    );
    let mut stream = TcpStream::connect(&identity.endpoint)
        .with_context(|| format!("connect to fixture endpoint {}", identity.endpoint))?;
    let challenge = json!({
        "invocation_token": identity.invocation_token,
        "start_nonce": identity.start_nonce,
        "pid": identity.pid,
        "executable": identity.executable,
        "action": "shutdown",
    });
    write_json_line(&mut stream, &challenge)?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    ensure!(
        serde_json::from_str::<Value>(&response)?.get("ok") == Some(&Value::Bool(true)),
        "fixture identity challenge was rejected"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_is_alive(identity.pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        !process_is_alive(identity.pid),
        "fixture PID {} did not exit",
        identity.pid
    );
    Ok(())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends no signal; it only probes existence and
    // permission for `pid`, which is valid for any pid value.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        #[cfg(target_os = "linux")]
        {
            return !linux_process_is_zombie(pid);
        }
        #[cfg(not(target_os = "linux"))]
        return true;
    }
    // A process we don't own (EPERM) still exists; only ESRCH means gone.
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(target_os = "linux")]
fn linux_process_is_zombie(pid: u32) -> bool {
    fs::read_to_string(Path::new("/proc").join(pid.to_string()).join("stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, state)| state.starts_with('Z'))
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: The returned process handle is checked and closed, and the exit
    // code pointer references a live local variable for the duration of the call.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut exit_code = 0;
        let alive =
            GetExitCodeProcess(process, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as u32;
        CloseHandle(process);
        alive
    }
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn canonical_pid_executable(pid: u32) -> anyhow::Result<String> {
    fs::canonicalize(Path::new("/proc").join(pid.to_string()).join("exe"))
        .map(|path| path.to_string_lossy().into_owned())
        .context("canonicalize recorded process executable")
}

#[cfg(target_os = "macos")]
fn canonical_pid_executable(pid: u32) -> anyhow::Result<String> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buffer` is sized to `PROC_PIDPATHINFO_MAXSIZE`, the maximum
    // path length `proc_pidpath` may write, per Apple's libproc contract.
    let len = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    ensure!(
        len > 0,
        "resolve executable path for pid {pid}: {}",
        io::Error::last_os_error()
    );
    let path = std::str::from_utf8(&buffer[..len as usize])
        .context("process executable path was not valid UTF-8")?;
    fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .context("canonicalize recorded process executable")
}

#[cfg(windows)]
fn canonical_pid_executable(pid: u32) -> anyhow::Result<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    // SAFETY: The process handle is checked and closed. The UTF-16 buffer and
    // size pointer remain valid for the duration of QueryFullProcessImageNameW.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(!process.is_null(), "open recorded process {pid}");
        let mut buffer = vec![0u16; 32_768];
        let mut len = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut len);
        CloseHandle(process);
        ensure!(result != 0, "resolve executable path for pid {pid}");
        let path = String::from_utf16(&buffer[..len as usize])
            .context("process executable path was not valid UTF-16")?;
        fs::canonicalize(path)
            .map(|path| path.to_string_lossy().into_owned())
            .context("canonicalize recorded process executable")
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn canonical_pid_executable(_pid: u32) -> anyhow::Result<String> {
    bail!("process executable lookup is unsupported on this platform")
}

fn canonical_current_exe() -> anyhow::Result<String> {
    Ok(fs::canonicalize(std::env::current_exe()?)?
        .to_string_lossy()
        .into_owned())
}

struct EventWriter {
    file: File,
}

impl EventWriter {
    fn new(path: &Path) -> anyhow::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("events path has no parent"))?;
        fs::create_dir_all(parent)?;
        Ok(Self {
            file: OpenOptions::new().create(true).append(true).open(path)?,
        })
    }

    fn record(&mut self, event: &str, details: Value) -> anyhow::Result<()> {
        let details = details
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow!("fixture event details must be an object"))?;
        serde_json::to_writer(
            &mut self.file,
            &FixtureEvent {
                event: event.to_owned(),
                details,
            },
        )?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn read_events(path: &Path) -> anyhow::Result<Vec<FixtureEvent>> {
    let file = File::open(path).with_context(|| format!("open events {}", path.display()))?;
    BufReader::new(file)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_fixture_parses_resume_forms() {
        let equals = parse_serve_args(vec![
            "--mode".into(),
            "acknowledge-cancel".into(),
            "--events".into(),
            "events".into(),
            "--invocation-token".into(),
            "token".into(),
            "--identity-dir".into(),
            "ids".into(),
            "--resume=session-1".into(),
        ])
        .unwrap();
        assert_eq!(equals.resume_session_id.as_deref(), Some("session-1"));
        let separated = parse_serve_args(vec![
            "--mode".into(),
            "ignore-cancel".into(),
            "--events".into(),
            "events".into(),
            "--invocation-token".into(),
            "token".into(),
            "--identity-dir".into(),
            "ids".into(),
            "--resume".into(),
            "session-2".into(),
        ])
        .unwrap();
        assert_eq!(separated.resume_session_id.as_deref(), Some("session-2"));
    }

    #[test]
    fn watchdog_fixture_records_jsonl_shape() {
        let directory = tempfile::tempdir().unwrap();
        let events = directory.path().join("events.jsonl");
        let mut writer = EventWriter::new(&events).unwrap();
        writer
            .record("process_started", json!({"pid": 42}))
            .unwrap();
        let parsed = read_events(&events).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].event, "process_started");
        assert_eq!(parsed[0].details["pid"], 42);
    }

    #[test]
    fn watchdog_fixture_handles_initialize_session_prompt_and_cancel() {
        let directory = tempfile::tempdir().unwrap();
        let mut events = EventWriter::new(&directory.path().join("events.jsonl")).unwrap();
        let mut output = Vec::new();
        let mut session_id = None;
        let mut pending_prompt = None;

        handle_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            Mode::AcknowledgeCancel,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();
        handle_request(
            &json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{}}),
            Mode::AcknowledgeCancel,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();
        let new_response: Value =
            serde_json::from_slice(output.split(|byte| *byte == b'\n').nth(1).unwrap()).unwrap();
        let current = new_response["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        handle_request(
            &json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":current,"prompt":[{"text":"watchdog smoke"}]}}),
            Mode::AcknowledgeCancel,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();
        handle_request(
            &json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":current}}),
            Mode::AcknowledgeCancel,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();
        let responses: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice)
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(responses[0]["result"]["protocolVersion"], 1);
        assert_eq!(responses[2]["result"]["stopReason"], "cancelled");
        let events = read_events(&directory.path().join("events.jsonl")).unwrap();
        assert!(events.iter().any(|event| event.event == "cancel_received"));
    }

    #[test]
    fn watchdog_fixture_end_turn_cancel_mode_answers_prompt_with_end_turn() {
        let directory = tempfile::tempdir().unwrap();
        let mut events = EventWriter::new(&directory.path().join("events.jsonl")).unwrap();
        let mut output = Vec::new();
        let mut session_id = None;
        let mut pending_prompt = None;

        handle_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            Mode::CancelEndsTurn,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();
        handle_request(
            &json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{}}),
            Mode::CancelEndsTurn,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();
        let new_response: Value =
            serde_json::from_slice(output.split(|byte| *byte == b'\n').nth(1).unwrap()).unwrap();
        let current = new_response["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        handle_request(
            &json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":current,"prompt":[{"text":"watchdog smoke"}]}}),
            Mode::CancelEndsTurn,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();
        assert_eq!(pending_prompt, Some(3));
        handle_request(
            &json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":current}}),
            Mode::CancelEndsTurn,
            &mut session_id,
            &mut pending_prompt,
            &mut output,
            &mut events,
        )
        .unwrap();

        let messages: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice)
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let updates: Vec<&Value> = messages
            .iter()
            .filter(|message| message["method"] == "session/update")
            .collect();
        assert_eq!(updates[0]["params"]["update"]["sessionUpdate"], "tool_call");
        assert_eq!(
            updates[0]["params"]["update"]["toolCallId"],
            "watchdog-tool-1"
        );
        assert_eq!(updates[0]["params"]["update"]["status"], "in_progress");
        assert_eq!(
            updates[1]["params"]["update"]["sessionUpdate"],
            "tool_call_update"
        );
        assert_eq!(
            updates[1]["params"]["update"]["toolCallId"],
            "watchdog-tool-1"
        );
        assert_eq!(updates[1]["params"]["update"]["status"], "completed");
        assert_eq!(
            updates[2]["params"]["update"]["content"]["text"],
            "Info: Operation cancelled by user"
        );
        let answer = messages
            .iter()
            .find(|message| message["id"] == 3 && message.get("result").is_some())
            .expect("the pending prompt must be answered");
        assert_eq!(answer["result"]["stopReason"], "end_turn");
        assert_eq!(pending_prompt, None);
    }

    #[test]
    fn watchdog_fixture_rejects_zero_deadlines() {
        assert!(parse_seconds("--soft-deadline-seconds", "0").is_err());
        assert!(parse_seconds("--hard-deadline-seconds", "not-a-number").is_err());
    }

    #[test]
    fn watchdog_fixture_verify_failure_preserves_evidence_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let args = test_verify_args(workspace.clone());
        let mut calls = Vec::new();

        let error = verify_with_scenario_runner(&args, |args, name, _, _| {
            calls.push(name.to_owned());
            let evidence = args.workspace.join(name).join("evidence.txt");
            fs::create_dir_all(evidence.parent().unwrap())?;
            fs::write(evidence, format!("{name} evidence"))?;
            if name == "hard" {
                bail!("forced verifier failure")
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(calls, ["soft", "hard"]);
        assert!(error.to_string().contains("watchdog evidence preserved at"));
        assert_eq!(
            fs::read_to_string(workspace.join(".cowboy-watchdog-smoke")).unwrap(),
            WORKSPACE_MARKER
        );
        assert_eq!(
            fs::read_to_string(workspace.join("hard/evidence.txt")).unwrap(),
            "hard evidence"
        );
        cleanup(&workspace).unwrap();
    }

    #[test]
    fn watchdog_fixture_verify_success_removes_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let args = test_verify_args(workspace.clone());
        let mut calls = Vec::new();

        verify_with_scenario_runner(&args, |args, name, _, _| {
            calls.push(name.to_owned());
            let evidence = args.workspace.join(name).join("evidence.txt");
            fs::create_dir_all(evidence.parent().unwrap())?;
            fs::write(evidence, format!("{name} evidence"))?;
            Ok(())
        })
        .unwrap();

        assert_eq!(calls, ["soft", "hard", "end-turn-cancel"]);
        assert!(!workspace.exists());
    }

    #[test]
    fn watchdog_fixture_cleanup_refuses_unmarked_directory() {
        let directory = tempfile::tempdir().unwrap();

        let error = cleanup(directory.path()).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("not a recognized watchdog smoke workspace")
        );
        assert!(directory.path().exists());
    }

    fn test_verify_args(workspace: PathBuf) -> VerifyArgs {
        VerifyArgs {
            cowboy: std::env::current_exe().unwrap(),
            workspace,
            response_timeout_seconds: 1,
            cancel_timeout_seconds: 2,
            recovery_operation_timeout_seconds: 3,
            soft_deadline_seconds: 15,
            hard_deadline_seconds: 20,
        }
    }

    #[test]
    fn watchdog_fixture_generates_exact_smoke_contract() {
        let directory = tempfile::tempdir().unwrap();
        let args = test_verify_args(directory.path().join("workspace"));
        let scenario = directory.path().join("scenario");
        fs::create_dir_all(&scenario).unwrap();
        write_scenario_files(
            &args,
            &scenario,
            "/fixture",
            "token",
            Mode::AcknowledgeCancel,
        )
        .unwrap();
        let config = fs::read_to_string(scenario.join("config.toml")).unwrap();
        let workflow = fs::read_to_string(scenario.join("workflows/watchdog_smoke.lua")).unwrap();
        assert!(config.contains("response_timeout_seconds = 1"));
        assert!(config.contains("cancel_timeout_seconds = 2"));
        assert!(config.contains("recovery_operation_timeout_seconds = 3"));
        assert!(config.contains("\"serve\", \"--mode\", \"acknowledge-cancel\""));
        assert!(workflow.contains("status = { \"success\" }"));
        assert!(workflow.contains("required_fields = { \"summary\" }"));
    }

    #[test]
    fn watchdog_fixture_generates_allowed_env_contract() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("allowed-env");
        fs::create_dir_all(workspace.join("identities")).unwrap();
        fs::create_dir_all(workspace.join("workflows")).unwrap();
        fs::create_dir_all(workspace.join("state")).unwrap();
        fs::write(workspace.join(".cowboy-watchdog-smoke"), WORKSPACE_MARKER).unwrap();
        let args = VerifyArgs {
            workspace: workspace.clone(),
            ..test_verify_args(workspace.clone())
        };

        write_allowed_environment_files(&args).unwrap();

        let config = fs::read_to_string(workspace.join("config.toml")).unwrap();
        let workflow = fs::read_to_string(workspace.join("workflows/allowed_env.lua")).unwrap();
        assert!(config.contains("allowed_env = [\"COWBOY_TEST_GLOBAL\"]"));
        assert!(config.contains("name = \"planner\""));
        assert!(config.contains("allowed_env = [\"COWBOY_TEST_PLANNER\"]"));
        assert!(config.contains("name = \"implementer\""));
        assert!(config.contains("allowed_env = [\"COWBOY_TEST_IMPLEMENTER\"]"));
        assert!(config.contains("--identity-dir"));
        assert!(workflow.contains("probe-environment"));
        assert!(workflow.contains("id = \"continue\""));
        assert!(workflow.contains("planner watchdog"));
        assert_eq!(
            fs::read_to_string(workspace.join(".cowboy-watchdog-smoke")).unwrap(),
            WORKSPACE_MARKER
        );
        assert!(workspace.join("identities").is_dir());
    }

    #[test]
    fn watchdog_fixture_environment_matrix_rejects_cross_role_leakage() {
        let leaked = FixtureEvent {
            event: "process_started".to_owned(),
            details: json!({
                "agent": "implementer",
                "environment": {
                    "COWBOY_TEST_GLOBAL": "set",
                    "COWBOY_TEST_PLANNER": "set",
                    "COWBOY_TEST_IMPLEMENTER": "set",
                    "COWBOY_TEST_UNAPPROVED": "missing"
                }
            })
            .as_object()
            .unwrap()
            .clone(),
        };
        assert!(assert_agent_environment(&[leaked], "implementer", false, true).is_err());
    }

    #[test]
    fn watchdog_fixture_environment_artifact_scan_rejects_marker_values() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        fs::create_dir_all(workspace.join("state/events")).unwrap();
        fs::write(
            workspace.join("state/events/run.json"),
            SYNTHETIC_ENVIRONMENT_VALUES[0].1,
        )
        .unwrap();

        let error = assert_environment_artifacts_clean(workspace).unwrap_err();

        assert!(error.to_string().contains("events artifact leaked"));
    }

    #[test]
    fn watchdog_fixture_omitted_allowed_env_starts_command_and_default_agent() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("default-allowed-env");
        fs::create_dir_all(workspace.join("identities")).unwrap();
        fs::create_dir_all(workspace.join("workflows")).unwrap();
        fs::create_dir_all(workspace.join("state")).unwrap();
        fs::write(workspace.join(".cowboy-watchdog-smoke"), WORKSPACE_MARKER).unwrap();
        let args = DefaultEnvironmentVerifyArgs {
            cowboy: std::env::current_exe().unwrap(),
            workspace: workspace.clone(),
            deadline_seconds: 20,
        };

        write_default_environment_files(&args).unwrap();

        let config = fs::read_to_string(workspace.join("config.toml")).unwrap();
        let workflow =
            fs::read_to_string(workspace.join("workflows/default_allowed_env.lua")).unwrap();
        assert!(!config.contains("allowed_env"));
        assert!(config.contains("name = \"default\""));
        assert!(workflow.contains("probe-environment"));
        assert!(config.contains("serve-environment"));
    }

    #[test]
    fn watchdog_fixture_cleanup_authenticates_allowed_env_layout() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = marked_workspace(directory.path(), "allowed-env");
        let mut child = launch_identity_fixture(&workspace);

        cleanup(&workspace).unwrap();

        assert!(child.wait().unwrap().success());
        assert!(!workspace.exists());
    }

    #[test]
    fn watchdog_fixture_cleanup_authenticates_default_allowed_env_layout() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = marked_workspace(directory.path(), "default-allowed-env");
        let mut child = launch_identity_fixture(&workspace);

        cleanup(&workspace).unwrap();

        assert!(child.wait().unwrap().success());
        assert!(!workspace.exists());
    }

    #[test]
    fn watchdog_fixture_cleanup_preserves_workspace_on_identity_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = marked_workspace(directory.path(), "mismatch");
        let mut child = launch_identity_fixture(&workspace);
        let identity_path = wait_for_identity(&workspace.join("identities"));
        let original = fs::read_to_string(&identity_path).unwrap();
        let mut altered: Identity = serde_json::from_str(&original).unwrap();
        altered.start_nonce = "incorrect-nonce".to_owned();
        write_json(&identity_path, &altered).unwrap();

        assert!(cleanup(&workspace).is_err());
        assert!(workspace.exists());
        assert!(child.try_wait().unwrap().is_none());

        fs::write(&identity_path, original).unwrap();
        cleanup(&workspace).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(!workspace.exists());
    }

    #[test]
    fn watchdog_fixture_identity_discovery_rejects_symlink_and_non_regular_entries() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = marked_workspace(directory.path(), "invalid-identities");
        let identities = workspace.join("identities");
        let outside = directory.path().join("outside.json");
        fs::write(&outside, "{}").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, identities.join("linked.json")).unwrap();
        #[cfg(not(unix))]
        fs::create_dir(identities.join("linked.json")).unwrap();
        fs::create_dir(identities.join("directory.json")).unwrap();

        let error = find_identity_files(&workspace).unwrap_err();

        assert!(error.to_string().contains("regular .json"));
        assert!(workspace.exists());
        assert!(outside.exists());
    }

    fn marked_workspace(parent: &Path, name: &str) -> PathBuf {
        let workspace = parent.join(name);
        fs::create_dir_all(workspace.join("identities")).unwrap();
        fs::write(workspace.join(".cowboy-watchdog-smoke"), WORKSPACE_MARKER).unwrap();
        workspace
    }

    fn launch_identity_fixture(workspace: &Path) -> std::process::Child {
        let executable = fixture_executable_for_test();
        let events = workspace
            .join("fixture-events.jsonl")
            .to_string_lossy()
            .into_owned();
        let identities = workspace.join("identities").to_string_lossy().into_owned();
        let child = Command::new(executable)
            .args([
                "serve",
                "--mode",
                "acknowledge-cancel",
                "--events",
                &events,
                "--invocation-token",
                "cleanup-test-token",
                "--identity-dir",
                &identities,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let _ = wait_for_identity(&workspace.join("identities"));
        child
    }

    fn fixture_executable_for_test() -> PathBuf {
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_watchdog-fixture") {
            return PathBuf::from(path);
        }
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let executable = workspace.join("target/debug/watchdog-fixture");
        if executable.is_file() {
            return executable;
        }
        let status = Command::new("cargo")
            .current_dir(workspace)
            .args([
                "build",
                "-p",
                "cowboy-agent-acp",
                "--bin",
                "watchdog-fixture",
            ])
            .status()
            .unwrap();
        assert!(status.success(), "build watchdog fixture for cleanup test");
        executable
    }

    fn wait_for_identity(identities: &Path) -> PathBuf {
        for _ in 0..200 {
            if let Ok(entries) = fs::read_dir(identities)
                && let Some(path) = entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .find(|path| {
                        path.extension().and_then(|extension| extension.to_str()) == Some("json")
                    })
            {
                return path;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "fixture did not write an identity record in {}",
            identities.display()
        );
    }

    #[test]
    fn watchdog_fixture_rejects_identity_mismatch_without_signalling() {
        let directory = tempfile::tempdir().unwrap();
        let identity = Identity {
            endpoint: "127.0.0.1:1".into(),
            invocation_token: "token".into(),
            start_nonce: "nonce".into(),
            pid: std::process::id(),
            executable: "/not/a/process".into(),
        };
        let path = directory.path().join("identity.json");
        write_json(&path, &identity).unwrap();
        assert!(cleanup_identity(&path).is_err());
    }

    #[test]
    fn watchdog_fixture_identity_challenge_requires_every_field() {
        let identity = Identity {
            endpoint: "127.0.0.1:0".into(),
            invocation_token: "token".into(),
            start_nonce: "nonce".into(),
            pid: 1,
            executable: "/fixture".into(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = Arc::clone(&shutdown);
        thread::spawn(move || identity_server(listener, identity, server_shutdown));
        let mut stream = TcpStream::connect(address).unwrap();
        write_json_line(&mut stream, &json!({
            "invocation_token": "token", "start_nonce": "wrong", "pid": 1, "executable": "/fixture", "action": "shutdown"
        })).unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap()["ok"],
            false
        );
        assert!(!shutdown.load(Ordering::SeqCst));
    }

    #[test]
    fn watchdog_fixture_identity_challenge_all_fields_shuts_down() {
        let identity = Identity {
            endpoint: "127.0.0.1:0".into(),
            invocation_token: "token".into(),
            start_nonce: "nonce".into(),
            pid: 1,
            executable: "/fixture".into(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = Arc::clone(&shutdown);
        thread::spawn(move || identity_server(listener, identity, server_shutdown));
        let mut stream = TcpStream::connect(address).unwrap();
        write_json_line(
            &mut stream,
            &json!({
                "invocation_token": "token", "start_nonce": "nonce", "pid": 1, "executable": "/fixture", "action": "shutdown"
            }),
        )
        .unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap()["ok"],
            true
        );
        for _ in 0..40 {
            if shutdown.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("matched identity challenge did not request shutdown");
    }
}
