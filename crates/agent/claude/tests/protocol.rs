#![cfg(unix)]

use cowboy_agent_claude::ClaudeCodeBackendConnector;
use cowboy_agent_client::{
    AgentAccess, AgentLaunchSpec, AgentWatchdogOptions, BackendRegistry, Client, Event, ModelInfo,
    PromptContent, PromptTurnCancellation, StopReason,
};
use serde_json::Value;
use std::{fs, path::Path, sync::Arc, time::Duration};

// Runs with a cleared environment and uses only shell builtins. Record input verbatim
// for structural JSON assertions in Rust; the shell only drives the handshake.
const FAKE_CLAUDE: &str = r#"
set -eu
pwd -P > "$log/cwd"
printf '%s\n' "$$" > "$log/pid"
printf '%s\n' "$@" > "$log/args"
: > "$log/input"

session=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --session-id=*|--resume=*) session=${1#*=} ;;
        --session-id|--resume) shift; session=$1 ;;
    esac
    shift
done
[ -n "$session" ]

receive() {
    IFS= read -r line || exit 0
    printf '%s\n' "$line" >> "$log/input"
}
ack() {
    printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$1"
}
result() {
    printf '{"type":"result","subtype":"success","is_error":false,"session_id":"%s"}\n' "$session"
}
stream_text() {
    printf '{"type":"stream_event","session_id":"%s","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}\n' "$session"
    for chunk in "$@"; do
        printf '{"type":"stream_event","session_id":"%s","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"%s"}}}\n' "$session" "$chunk"
    done
    printf '{"type":"stream_event","session_id":"%s","event":{"type":"content_block_stop","index":0}}\n' "$session"
}

receive
ack initialize
receive
printf '{"type":"system","subtype":"init","session_id":"%s","model":"reported-model"}\n' "$session"
case "$mode" in
    new)
        stream_text 'Hel' 'lo'
        printf '{"type":"assistant","session_id":"%s","message":{"role":"assistant","content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"toolu_read","name":"Read","input":{"file_path":"example.txt"}}]}}\n' "$session"
        printf '%s\n' '{"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Read","tool_use_id":"toolu_read","input":{"file_path":"example.txt"}}}'
        receive
        printf '{"type":"user","session_id":"%s","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_read","is_error":true,"content":"Permission denied"}]}}\n' "$session"
        result
        ;;
    resume)
        stream_text 'Resumed'
        result
        ;;
    cancel)
        stream_text 'Started'
        receive
        ack interrupt
        result
        # A second prompt must receive its own text and result, not the cancelled turn's result.
        receive
        stream_text 'Recovered'
        result
        ;;
    error)
        printf '{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"%s","errors":["test-secret-do-not-expose"],"result":"test-secret-do-not-expose"}\n' "$session"
        ;;
esac
# The hang mode deliberately never sends a result. All modes stay alive until closed.
while IFS= read -r line; do :; done
"#;

fn registry() -> BackendRegistry {
    BackendRegistry::new(vec![Arc::new(ClaudeCodeBackendConnector)])
}

fn spec(command: &str, args: Vec<String>) -> AgentLaunchSpec {
    AgentLaunchSpec {
        agent: "claude".into(),
        backend: "claude-code".into(),
        command: command.into(),
        args,
        allowed_env: vec![],
        access: AgentAccess::Default,
        watchdog: AgentWatchdogOptions {
            response_timeout_seconds: 3,
            cancel_timeout_seconds: 3,
            recovery_operation_timeout_seconds: 3,
        },
        startup_readiness: None,
        model: None,
    }
}

fn fake_spec(log: &Path, mode: &str) -> AgentLaunchSpec {
    fs::create_dir(log).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let executable = log.join("claude");
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nmode={}\nlog={}\n{FAKE_CLAUDE}",
            quote(mode),
            quote(log.to_str().unwrap())
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    spec(executable.to_str().unwrap(), vec![])
}

fn read_log(log: &Path, name: &str) -> String {
    fs::read_to_string(log.join(name)).unwrap()
}

fn arg_value<'a>(args: &'a [&str], flag: &str) -> Option<&'a str> {
    for (index, arg) in args.iter().enumerate() {
        if *arg == flag {
            return args.get(index + 1).copied();
        }
        if let Some((name, value)) = arg.split_once('=')
            && name == flag
        {
            return Some(value);
        }
    }
    None
}

fn messages(log: &Path) -> Vec<Value> {
    read_log(log, "input")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_request(message: &Value, id: &str) {
    assert_eq!(message["type"], "control_request");
    assert_eq!(message["request_id"], id);
    assert_eq!(message["request"]["subtype"], id);
}

fn assert_user(message: &Value, text: &str) {
    assert_eq!(message["type"], "user");
    assert_eq!(message["message"]["role"], "user");
    let content = &message["message"]["content"];
    if let Some(blocks) = content.as_array() {
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], text);
    } else {
        assert_eq!(content, text);
    }
}

fn message_text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::MessageChunk { content } => Some(content["text"].as_str().unwrap()),
            _ => None,
        })
        .collect()
}

async fn prompt(
    client: &mut dyn Client,
    session: &str,
    text: &str,
    events: &mut Vec<Event>,
) -> StopReason {
    client
        .prompt(
            session,
            vec![PromptContent::text(text)],
            PromptTurnCancellation::disabled(),
            &mut |event| events.push(event),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn sessions_stream_deny_tools_and_resume_without_replay() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project with spaces");
        fs::create_dir(&project).unwrap();
        let cwd = project.to_str().unwrap();
        let new_log = dir.path().join("new");
        let mut launch = fake_spec(&new_log, "new");
        launch.model = Some(ModelInfo::with_id("requested-model"));
        let connection = registry().connect(&launch).await.unwrap();
        let mut client = connection.client;
        assert!(client.supports_load_session());
        let session = client
            .new_session(cwd, &[], connection.model.as_ref())
            .await
            .unwrap();
        assert!(client.is_connected());
        assert_eq!(client.session_id(), Some(session.as_str()));

        let mut events = Vec::new();
        let stop = prompt(client.as_mut(), &session, "Read example.txt", &mut events).await;
        assert!(matches!(stop, StopReason::EndTurn));
        // The final assistant record repeats the streamed text; it must not be emitted twice.
        assert_eq!(message_text(&events), "Hello");
        let tools: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, Event::ToolCall { .. } | Event::ToolCallUpdate { .. }))
            .collect();
        let [
            Event::ToolCall { tool_call_id, .. },
            Event::ToolCallUpdate {
                tool_call_id: updated,
                status,
                ..
            },
        ] = tools.as_slice()
        else {
            panic!("unexpected tool events: {tools:?}");
        };
        assert_eq!(tool_call_id, "toolu_read");
        assert_eq!(updated, tool_call_id);
        assert_eq!(status, "failed");
        assert_eq!(
            client.session_descriptor().unwrap().model.as_deref(),
            Some("reported-model")
        );
        client.close().await.unwrap();
        assert!(!client.is_connected());

        let args = read_log(&new_log, "args");
        let args: Vec<_> = args.lines().collect();
        assert!(args.contains(&"--safe-mode"));
        assert_eq!(arg_value(&args, "--model"), Some("requested-model"));
        assert_eq!(arg_value(&args, "--session-id"), Some(session.as_str()));
        assert_eq!(arg_value(&args, "--resume"), None);
        let uuid_parts: Vec<_> = session.split('-').collect();
        assert_eq!(
            uuid_parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(
            uuid_parts
                .iter()
                .all(|part| part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        );
        assert_eq!(
            read_log(&new_log, "cwd").trim_end(),
            project.canonicalize().unwrap().to_str().unwrap()
        );
        let input = messages(&new_log);
        assert_eq!(input.len(), 3, "unexpected new-session input: {input:?}");
        assert_request(&input[0], "initialize");
        assert_user(&input[1], "Read example.txt");
        assert_eq!(input[2]["type"], "control_response");
        assert_eq!(input[2]["response"]["request_id"], "permission-1");
        assert_eq!(input[2]["response"]["subtype"], "success");
        assert_eq!(input[2]["response"]["response"]["behavior"], "deny");

        let resume_log = dir.path().join("resume");
        let mut launch = fake_spec(&resume_log, "resume");
        launch.model = Some(ModelInfo::with_id("requested-model"));
        let mut resumed = registry().connect(&launch).await.unwrap().client;
        let history = resumed.load_session(&session, cwd, &[]).await.unwrap();
        assert!(history.is_empty(), "native resume does not replay history");
        assert_eq!(resumed.session_id(), Some(session.as_str()));
        let mut events = Vec::new();
        let stop = prompt(resumed.as_mut(), &session, "Continue", &mut events).await;
        assert!(matches!(stop, StopReason::EndTurn));
        assert_eq!(message_text(&events), "Resumed");
        resumed.close().await.unwrap();
        assert!(!resumed.is_connected());

        let args = read_log(&resume_log, "args");
        let args: Vec<_> = args.lines().collect();
        assert!(args.contains(&format!("--resume={session}").as_str()));
        assert_eq!(arg_value(&args, "--session-id"), None);
        assert_eq!(arg_value(&args, "--model"), Some("requested-model"));
        let input = messages(&resume_log);
        assert_eq!(input.len(), 2, "unexpected resume input: {input:?}");
        assert_request(&input[0], "initialize");
        assert_user(&input[1], "Continue");
    })
    .await
    .expect("session contract test timed out");
}

#[tokio::test]
async fn cancellation_interrupts_and_settles_before_next_prompt() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("cancel");
        let mut client = registry()
            .connect(&fake_spec(&log, "cancel"))
            .await
            .unwrap()
            .client;
        let session = client
            .new_session(dir.path().to_str().unwrap(), &[], None)
            .await
            .unwrap();
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let mut cancel = Some(cancel);
        let stop = client
            .prompt(
                &session,
                vec![PromptContent::text("Cancel this turn")],
                PromptTurnCancellation::from_future(async { cancelled.await.unwrap() }),
                &mut |event| {
                    if matches!(event, Event::MessageChunk { content } if content["text"] == "Started")
                        && let Some(cancel) = cancel.take()
                    {
                        cancel.send(()).unwrap();
                    }
                },
            )
            .await
            .unwrap();
        assert!(matches!(stop, StopReason::Cancelled));
        let mut events = Vec::new();
        let stop = prompt(client.as_mut(), &session, "Try again", &mut events).await;
        assert!(matches!(stop, StopReason::EndTurn));
        assert_eq!(message_text(&events), "Recovered");
        client.close().await.unwrap();
        assert!(!client.is_connected());

        let input = messages(&log);
        assert_eq!(input.len(), 4, "unexpected cancellation input: {input:?}");
        assert_request(&input[0], "initialize");
        assert_user(&input[1], "Cancel this turn");
        assert_request(&input[2], "interrupt");
        assert_user(&input[3], "Try again");
    })
    .await
    .expect("cancellation contract test timed out");
}

#[tokio::test]
async fn failed_result_does_not_expose_provider_payload() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("error");
        let mut client = registry()
            .connect(&fake_spec(&log, "error"))
            .await
            .unwrap()
            .client;
        let session = client
            .new_session(dir.path().to_str().unwrap(), &[], None)
            .await
            .unwrap();
        let error = client
            .prompt(
                &session,
                vec![PromptContent::text("Fail this turn")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.to_lowercase().contains("failed"), "{diagnostic}");
        assert!(!diagnostic.contains("test-secret-do-not-expose"));
        client.close().await.unwrap();
        assert!(!client.is_connected());
    })
    .await
    .expect("error contract test timed out");
}

#[tokio::test]
async fn response_watchdog_disconnects_and_reaps_process() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("hang");
        let mut launch = fake_spec(&log, "hang");
        launch.watchdog.response_timeout_seconds = 1;
        let mut client = registry().connect(&launch).await.unwrap().client;
        let session = client
            .new_session(dir.path().to_str().unwrap(), &[], None)
            .await
            .unwrap();
        let error = client
            .prompt(
                &session,
                vec![PromptContent::text("Never finishes")],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.to_lowercase().contains("timed out"),
            "{diagnostic}"
        );
        assert!(!client.is_connected());
        let pid = read_log(&log, "pid").trim().parse::<u32>().unwrap();
        assert!(
            !std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "kill -0 \"$1\" 2>/dev/null",
                    "check-child",
                    &pid.to_string()
                ])
                .status()
                .unwrap()
                .success(),
            "watchdog left the fake Claude process alive"
        );
        client.close().await.unwrap();
    })
    .await
    .expect("response watchdog contract test timed out");
}

/// Opt in with `cargo test -p cowboy-agent-claude --test protocol real_claude_smoke -- --ignored`.
/// Requires a Claude Code CLI (`claude` on PATH) with native stream-json support,
/// network access, a billed ANTHROPIC_API_KEY, and CLAUDE_CONFIG_DIR pointing to
/// an empty, writable absolute directory. The test makes two billable turns.
#[tokio::test]
#[ignore = "requires Claude Code, network access, a billed API key, and isolated Claude config"]
async fn real_claude_smoke() {
    assert!(std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|key| !key.is_empty()));
    let config = std::path::PathBuf::from(
        std::env::var_os("CLAUDE_CONFIG_DIR").expect("set CLAUDE_CONFIG_DIR to an empty directory"),
    );
    assert!(config.is_absolute() && config.is_dir());
    assert!(fs::read_dir(&config).unwrap().next().is_none());
    let project = tempfile::tempdir().unwrap();
    let cwd = project.path().to_str().unwrap();
    let mut launch = spec("claude", vec![]);
    launch.allowed_env = [
        "PATH",
        "HOME",
        "TMPDIR",
        "ANTHROPIC_API_KEY",
        "CLAUDE_CONFIG_DIR",
    ]
    .map(str::to_owned)
    .to_vec();
    launch.watchdog.response_timeout_seconds = 120;
    launch.watchdog.recovery_operation_timeout_seconds = 120;

    tokio::time::timeout(Duration::from_secs(300), async {
        let mut client = registry().connect(&launch).await.unwrap().client;
        let session = client.new_session(cwd, &[], None).await.unwrap();
        let mut events = Vec::new();
        let stop = prompt(
            client.as_mut(),
            &session,
            "Remember the word bluebird for this conversation. Reply with just that word, without using tools.",
            &mut events,
        ).await;
        assert!(matches!(stop, StopReason::EndTurn));
        assert!(message_text(&events).to_lowercase().contains("bluebird"));
        client.close().await.unwrap();

        let mut resumed = registry().connect(&launch).await.unwrap().client;
        assert!(resumed.load_session(&session, cwd, &[]).await.unwrap().is_empty());
        events.clear();
        let stop = prompt(
            resumed.as_mut(),
            &session,
            "Reply with just the word I asked you to remember, without using tools.",
            &mut events,
        ).await;
        assert!(matches!(stop, StopReason::EndTurn));
        assert!(message_text(&events).to_lowercase().contains("bluebird"));
        resumed.close().await.unwrap();
    })
    .await
    .expect("live Claude smoke test timed out");
}
