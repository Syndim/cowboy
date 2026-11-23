use cowboy_agent_acp::ClaudeCodeBackendConnector;
use cowboy_agent_client::{
    AgentAccess, AgentLaunchSpec, BackendRegistry, Event, ModelInfo, PromptContent,
    PromptTurnCancellation, StopReason,
};
use std::{sync::Arc, time::Duration};

fn registry() -> BackendRegistry {
    BackendRegistry::new(vec![Arc::new(ClaudeCodeBackendConnector)])
}

fn spec(command: String, args: Vec<String>) -> AgentLaunchSpec {
    AgentLaunchSpec {
        agent: "claude".into(),
        backend: "claude-code".into(),
        command,
        args,
        allowed_env: vec![],
        access: AgentAccess::Default,
        watchdog: Default::default(),
        startup_readiness: None,
        model: None,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn claude_code_contract() {
    const INIT: &str = r#"
set -eu
[ "$0" = '--hide-claude-auth' ]
read -r request
case "$request" in *'"method":"initialize"'*'"protocolVersion":1'*) ;; *) exit 10;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"agentInfo":{"name":"claude-agent-acp","version":"0.109.1"}}}'
mode='{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"default","options":[{"value":"default","name":"Default"}]}'
model='{"id":"model","name":"Model","category":"model","type":"select","currentValue":"sonnet","options":[{"value":"sonnet","name":"Sonnet"},{"value":"opus","name":"Opus"}]}'
selected='{"id":"model","name":"Model","category":"model","type":"select","currentValue":"opus","options":[{"value":"opus","name":"Opus"}]}'
"#;
    const NEW: &str = r#"
read -r request
case "$request" in *'"method":"session/new"'*'"cwd":"/trusted/project"'*'"mcpServers":[]'*) ;; *) exit 11;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"sessionId":"claude-session","configOptions":['"$mode,$model"']}}'
read -r request
case "$request" in *'"method":"session/set_config_option"'*'"sessionId":"claude-session"'*'"configId":"model"'*'"value":"opus"'*) ;; *) exit 12;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"configOptions":['"$mode,$selected"']}}'
read -r request
case "$request" in *'"method":"session/prompt"'*'"sessionId":"claude-session"'*) ;; *) exit 13;; esac
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"claude-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hello"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"claude-session","update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Read file","kind":"read","status":"pending"}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"sessionId":"claude-session","toolCall":{"toolCallId":"tool-1","title":"Read file"},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"}]}}'
read -r response
case "$response" in *'"id":99'*'"outcome":{"outcome":"cancelled"}'*) ;; *) exit 14;; esac
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"claude-session","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"failed"}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
read -r request
case "$request" in *'"method":"session/prompt"'*'"sessionId":"claude-session"'*) ;; *) exit 15;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":4,"error":{"code":-32603,"message":"Claude test failure"}}'
read -r request
"#;
    const LOAD: &str = r#"
read -r request
case "$request" in *'"method":"session/load"'*'"sessionId":"claude-session"'*'"cwd":"/trusted/project"'*) ;; *) exit 16;; esac
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"claude-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Replayed"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"configOptions":['"$mode,$selected"']}}'
read -r request
case "$request" in *'"method":"session/prompt"'*'"sessionId":"claude-session"'*) ;; *) exit 17;; esac
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"claude-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Continuing"}}}}'
read -r request
case "$request" in *'"method":"session/cancel"'*'"sessionId":"claude-session"'*) ;; *) exit 18;; esac
case "$request" in *'"id":'*) exit 19;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"stopReason":"cancelled"}}'
read -r request
"#;
    tokio::time::timeout(Duration::from_secs(10), async {
        let cwd = "/trusted/project";
        let launch = |body| spec("sh".into(), vec!["-c".into(), format!("{INIT}{body}")]);
        let registry = registry();
        let mut launch_new = launch(NEW);
        launch_new.model = Some(ModelInfo::with_id("opus"));
        let resolved = registry.connect(&launch_new).await.unwrap();
        let mut client = resolved.client;
        assert!(client.supports_load_session());
        let session = client.new_session(cwd, &[], resolved.model.as_ref()).await.unwrap();
        assert_eq!(session, "claude-session");
        let mut events = Vec::new();
        let result = client.prompt(&session, vec![PromptContent::text("Read")], PromptTurnCancellation::disabled(), &mut |event| events.push(event)).await.unwrap();
        assert!(matches!(result, StopReason::EndTurn));
        assert!(matches!(&events[..], [Event::MessageChunk { content }, Event::ToolCall { tool_call_id, .. }, Event::ToolCallUpdate { status, .. }]
            if content["text"] == "Hello" && tool_call_id == "tool-1" && status == "failed"));
        let error = client.prompt(&session, vec![PromptContent::text("Fail")], PromptTurnCancellation::disabled(), &mut |_| {}).await.unwrap_err();
        assert!(error.to_string().contains("Claude test failure"));
        client.close().await.unwrap();
        assert!(!client.is_connected());

        let mut resumed = registry.connect(&launch(LOAD)).await.unwrap().client;
        let replay = resumed.load_session(&session, cwd, &[]).await.unwrap();
        assert!(matches!(&replay[..], [Event::MessageChunk { content }] if content["text"] == "Replayed"));
        assert_eq!(resumed.session_descriptor().unwrap().model.as_deref(), Some("opus"));
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let mut cancel = Some(cancel);
        let result = resumed.prompt(&session, vec![PromptContent::text("Continue")],
            PromptTurnCancellation::from_future(async { cancelled.await.unwrap() }), &mut |event| {
                if matches!(event, Event::MessageChunk { .. }) && let Some(cancel) = cancel.take() {
                    cancel.send(()).unwrap();
                }
            }).await.unwrap();
        assert!(matches!(result, StopReason::Cancelled));
        resumed.close().await.unwrap();
        assert!(!resumed.is_connected());

        let mut unsafe_launch = launch(LOAD);
        unsafe_launch.args[1] = unsafe_launch.args[1].replace(
            r#""currentValue":"default""#, r#""currentValue":"bypassPermissions""#,
        );
        let mut unsafe_mode = registry.connect(&unsafe_launch).await.unwrap().client;
        let error = unsafe_mode.load_session(&session, cwd, &[]).await.unwrap_err();
        assert!(error.to_string().contains("mode"));
        assert!(!unsafe_mode.is_connected());
    }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires claude-agent-acp 0.109.1 and a billed Anthropic API key"]
async fn real_claude_code_smoke() {
    assert!(
        std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|key| !key.is_empty()),
        "set ANTHROPIC_API_KEY before running the live smoke test"
    );
    let project = tempfile::tempdir().unwrap();
    let cwd = project.path().to_str().unwrap();
    let mut spec = spec(
        std::env::var("CLAUDE_ACP_COMMAND").unwrap_or_else(|_| "claude-agent-acp".into()),
        vec![],
    );
    spec.allowed_env = [
        "ANTHROPIC_API_KEY",
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "TMPDIR",
        "TEMP",
        "TMP",
        "CLAUDE_CONFIG_DIR",
    ]
    .map(str::to_owned)
    .to_vec();
    let state = std::env::var_os("CLAUDE_CONFIG_DIR")
        .expect("set CLAUDE_CONFIG_DIR to an isolated absolute directory");
    let state = std::path::PathBuf::from(state);
    assert!(state.is_absolute() && state.is_dir());
    assert!(
        std::fs::read_dir(&state).unwrap().next().is_none(),
        "use an empty CLAUDE_CONFIG_DIR for the smoke test"
    );
    std::fs::write(
        state.join("settings.json"),
        r#"{"permissions":{"defaultMode":"default"}}"#,
    )
    .unwrap();
    let registry = registry();
    tokio::time::timeout(Duration::from_secs(180), async {
        let mut client = registry.connect(&spec).await.unwrap().client;
        assert_eq!(client.agent_info().and_then(|info| info.version.as_deref()), Some("0.109.1"));
        let session = client.new_session(cwd, &[], None).await.unwrap();
        let mut text = String::new();
        let stop = client.prompt(&session, vec![PromptContent::text("Remember the word bluebird for this conversation. Without using tools, reply with just that word.")], PromptTurnCancellation::disabled(), &mut |event| {
            if let Event::MessageChunk { content } = event && let Some(chunk) = content["text"].as_str() { text.push_str(chunk); }
        }).await.unwrap();
        assert!(matches!(stop, StopReason::EndTurn));
        assert!(text.to_lowercase().contains("bluebird"));
        client.close().await.unwrap();
        let mut resumed = registry.connect(&spec).await.unwrap().client;
        let history = resumed.load_session(&session, cwd, &[]).await.unwrap();
        assert!(history.iter().any(|event| matches!(event, Event::MessageChunk { content } if content["text"].as_str().is_some_and(|text| text.to_lowercase().contains("bluebird")))));
        let mut text = String::new();
        let stop = resumed.prompt(&session, vec![PromptContent::text("Without using tools, reply with just the word I asked you to remember.")], PromptTurnCancellation::disabled(), &mut |event| {
            if let Event::MessageChunk { content } = event && let Some(chunk) = content["text"].as_str() { text.push_str(chunk); }
        }).await.unwrap();
        assert!(matches!(stop, StopReason::EndTurn));
        assert!(text.to_lowercase().contains("bluebird"));
        resumed.close().await.unwrap();
    }).await.unwrap();
}
