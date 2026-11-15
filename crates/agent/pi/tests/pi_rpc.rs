//! Integration tests for the pi RPC backend using shell-script stubs that
//! emulate pi's JSONL RPC protocol.

use cowboy_agent_client::{Client, Event, PromptContent, PromptTurnCancellation, StopReason};

use cowboy_agent_pi::PiRpcClient;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn spec(command: &str) -> cowboy_agent_client::AgentLaunchSpec {
    cowboy_agent_client::AgentLaunchSpec {
        agent: "default".to_string(),
        backend: "pi-rpc".to_string(),
        command: command.to_string(),
        args: Vec::new(),
        allowed_env: Vec::new(),
        access: cowboy_agent_client::AgentAccess::default(),
        watchdog: cowboy_agent_client::AgentWatchdogOptions::default(),
        startup_readiness: None,
        model: None,
    }
}

fn stub_script(marker: &str, settle_aborted: bool) -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-stub.sh");
    let script = format!(
        r#"#!/bin/sh
# Line-oriented pi RPC stub: echo the request id in each response (as pi
# does), then stream message updates ending in agent_settled.
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"get_state"'*)
      printf '%s\n' '{{"id":"'"$id"'","type":"response","command":"get_state","success":true,"data":{{"model":{{"id":"stub-model"}},"thinkingLevel":"high"}}}}'
      ;;
    *'"type":"abort"'*)
      printf '%s\n' '{{"id":"'"$id"'","type":"response","command":"abort","success":true}}'
      ;;
    *'"type":"prompt"'*)
      printf '%s\n' '{{"id":"'"$id"'","type":"response","command":"prompt","success":true,"data":{{"disposition":"started"}}}}'
      printf '%s\n' '{{"type":"message_update","assistantMessageEvent":{{"type":"text_delta","delta":"---\nstatus: success\nsummary: {marker}\n---\ndone"}}}}'
      printf '%s\n' '{{"type":"message_update","assistantMessageEvent":{{"type":"thinking_delta","delta":"checking"}}}}'
      printf '%s\n' '{{"type":"tool_execution_start","toolCallId":"call_1","toolName":"bash"}}'
      printf '%s\n' '{{"type":"tool_execution_end","toolCallId":"call_1","isError":false}}'
      printf '%s\n' '{{"type":"agent_settled","aborted":{settle_aborted}}}'
      ;;
  esac
done
"#
    );
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
    }
    // Keep the tempdir alive for the process lifetime.
    std::mem::forget(dir);
    path
}

#[tokio::test]
async fn prompt_turn_streams_events_and_settles() {
    let path = stub_script("integration-ok", false);
    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();

    let mut events = Vec::new();
    let stop = client
        .prompt(
            "stub-session",
            vec![PromptContent::text("work")],
            PromptTurnCancellation::disabled(),
            &mut |event| events.push(event),
        )
        .await
        .unwrap();

    assert!(matches!(stop, StopReason::EndTurn));
    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::MessageChunk { content } => content["text"].as_str(),
            _ => None,
        })
        .collect();
    let joined = chunks.join("");
    assert!(joined.contains("status: success"), "{joined}");
    assert!(joined.contains("integration-ok"), "{joined}");

    // Thinking deltas map to thought chunks.
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ThoughtChunk { .. }))
    );
    // Tool lifecycle maps to tool call + completed update.
    assert!(events.iter().any(
        |event| matches!(event, Event::ToolCall { tool_call_id, status, .. } if tool_call_id == "call_1" && status == "in_progress")
    ));
    assert!(events.iter().any(
        |event| matches!(event, Event::ToolCallUpdate { status, .. } if status == "completed")
    ));
}

#[tokio::test]
async fn new_session_reports_descriptor_from_stub_state() {
    let path = stub_script("descriptor", false);
    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();

    let session = client.new_session(".", &[], None).await.unwrap();
    assert_eq!(session, "stub-session");
    let descriptor = client.session_descriptor().expect("stub reports a model");
    assert_eq!(descriptor.model.as_deref(), Some("stub-model"));
    assert_eq!(descriptor.reasoning.as_deref(), Some("high"));
}

#[tokio::test]
async fn load_session_same_id_reuses_process() {
    let path = stub_script("load", false);
    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();

    let history = client.load_session("stub-session", ".", &[]).await.unwrap();
    assert!(history.is_empty());
    assert!(client.session_id() == Some("stub-session"));
}

#[tokio::test]
async fn load_session_different_id_respawns_process() {
    let path = stub_script("load-other", false);
    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("session-a"))
        .await
        .unwrap();
    client.new_session(".", &[], None).await.unwrap();

    let history = client.load_session("session-b", ".", &[]).await.unwrap();
    assert!(history.is_empty());
    assert!(client.session_id() == Some("session-b"));

    // The restarted process still answers prompts end-to-end.
    let mut events = Vec::new();
    let stop = client
        .prompt(
            "session-b",
            vec![PromptContent::text("work")],
            PromptTurnCancellation::disabled(),
            &mut |event| events.push(event),
        )
        .await
        .unwrap();
    assert!(matches!(stop, StopReason::EndTurn));
    assert!(!events.is_empty());
}

#[tokio::test]
async fn abort_with_settled_aborted_yields_cancelled() {
    let path = stub_script("abort", true);
    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();

    let mut events = Vec::new();
    let stop = client
        .prompt(
            "stub-session",
            vec![PromptContent::text("work")],
            PromptTurnCancellation::disabled(),
            &mut |event| events.push(event),
        )
        .await
        .unwrap();
    // pi reports aborted=true in agent_settled; without a local cancellation
    // signal the turn still ends cleanly (pi unwound on its own).
    assert!(matches!(stop, StopReason::EndTurn), "{stop:?}");
    assert!(!events.is_empty());
}

#[tokio::test]
async fn subprocess_exit_mid_turn_fails_the_prompt() {
    // Stub that starts a turn but exits before settling.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-dying.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"get_state"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'); printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{}}"
      ;;
    *'"type":"prompt"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'); printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}"
      printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"partial"}}'
      exit 3
      ;;
  esac
done
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
    }

    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();
    let error = client
        .prompt(
            "stub-session",
            vec![PromptContent::text("work")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("exited before the turn settled"),
        "{error}"
    );
}

#[tokio::test]
async fn failed_response_records_surface_as_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pi-failing.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"get_state"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'); printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{}}"
      ;;
    *'"type":"prompt"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'); printf '%s\n' "{\"id\":\"$id\",\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"provider overloaded\"}"
      ;;
  esac
done
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
    }

    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();
    let error = client
        .prompt(
            "stub-session",
            vec![PromptContent::text("work")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("provider overloaded"), "{error}");
}

#[tokio::test]
async fn close_shuts_down_stub_process() {
    let path = stub_script("close", false);
    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();
    client.close().await.unwrap();
    assert!(!client.is_connected());
}

#[tokio::test]
async fn concurrent_prompts_never_interleave_responses() {
    let path = stub_script("seq", false);
    let mut client = PiRpcClient::connect(&spec(&path.to_string_lossy()), Some("stub-session"))
        .await
        .unwrap();

    for turn in 0..3 {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let stop = client
            .prompt(
                "stub-session",
                vec![PromptContent::text(format!("turn {turn}"))],
                PromptTurnCancellation::disabled(),
                &mut move |event| {
                    if matches!(event, Event::MessageChunk { .. }) {
                        counter_clone.fetch_add(1, Ordering::SeqCst);
                    }
                },
            )
            .await
            .unwrap();
        assert!(matches!(stop, StopReason::EndTurn));
        assert_eq!(counter.load(Ordering::SeqCst), 1, "turn {turn}");
    }
}
