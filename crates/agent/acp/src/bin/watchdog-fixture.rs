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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    AcknowledgeCancel,
    IgnoreCancel,
}

impl Mode {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "acknowledge-cancel" => Ok(Self::AcknowledgeCancel),
            "ignore-cancel" => Ok(Self::IgnoreCancel),
            _ => bail!("--mode must be acknowledge-cancel or ignore-cancel"),
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
        "Usage:\n  watchdog-fixture serve --mode acknowledge-cancel|ignore-cancel --events FILE --invocation-token TOKEN --identity-dir DIR [--resume=SESSION]\n  watchdog-fixture verify --cowboy PATH --workspace DIR --response-timeout-seconds N --cancel-timeout-seconds N --recovery-operation-timeout-seconds N --soft-deadline-seconds N --hard-deadline-seconds N\n  watchdog-fixture cleanup --workspace DIR"
    );
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
    fs::create_dir_all(&args.identity_dir)?;
    let executable = canonical_current_exe()?;
    let start_nonce = Uuid::new_v4().to_string();
    let pid = std::process::id();
    let listener = TcpListener::bind("127.0.0.1:0").context("bind fixture identity endpoint")?;
    listener.set_nonblocking(true)?;
    let identity = Identity {
        endpoint: listener.local_addr()?.to_string(),
        invocation_token: args.invocation_token.clone(),
        start_nonce,
        pid,
        executable,
    };
    let identity_path = args.identity_dir.join(format!("{pid}.json"));
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

    let mut events = EventWriter::new(&args.events)?;
    events.record(
        "process_started",
        json!({
            "pid": pid,
            "start_nonce": identity.start_nonce,
            "invocation_token": args.invocation_token,
            "executable": identity.executable,
            "endpoint": identity.endpoint,
            "resume_session_id": args.resume_session_id,
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
    let mut pending_prompt = None;
    let mut session_id = args.resume_session_id.clone();
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
        handle_request(
            &request,
            args.mode,
            &mut session_id,
            &mut pending_prompt,
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

fn handle_request(
    request: &Value,
    mode: Mode,
    session_id: &mut Option<String>,
    pending_prompt: &mut Option<u64>,
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
            *session_id = Some(new_id.clone());
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
            *session_id = Some(loaded.clone());
            events.record("session_loaded", json!({ "session_id": loaded }))?;
            respond(output, id, json!({ "configOptions": [] }))?;
        }
        "session/prompt" => {
            let current =
                request_session.ok_or_else(|| anyhow!("session/prompt requires sessionId"))?;
            ensure!(
                session_id.as_deref() == Some(current.as_str()),
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
                *pending_prompt = id;
            }
        }
        "session/cancel" => {
            let current =
                request_session.ok_or_else(|| anyhow!("session/cancel requires sessionId"))?;
            ensure!(
                session_id.as_deref() == Some(current.as_str()),
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
                if let Some(prompt_id) = pending_prompt.take() {
                    respond(
                        output,
                        Some(prompt_id),
                        json!({ "stopReason": "cancelled" }),
                    )?;
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
    let mode = match mode {
        Mode::AcknowledgeCancel => "acknowledge-cancel",
        Mode::IgnoreCancel => "ignore-cancel",
    };
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
    if mode == Mode::AcknowledgeCancel {
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
    if mode == Mode::AcknowledgeCancel {
        ensure!(
            log.contains("agent_watchdog_soft_recovered"),
            "watchdog log omitted agent_watchdog_soft_recovered"
        );
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
    for scenario in ["soft", "hard"] {
        let directory = workspace.join(scenario).join("identities");
        if !directory.exists() {
            continue;
        }
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    Ok(files)
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

fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn canonical_pid_executable(pid: u32) -> anyhow::Result<String> {
    fs::canonicalize(Path::new("/proc").join(pid.to_string()).join("exe"))
        .map(|path| path.to_string_lossy().into_owned())
        .context("canonicalize recorded process executable")
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

        assert_eq!(calls, ["soft", "hard"]);
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
