//! Deterministic native RPC subprocess contract tests. No provider/model calls.
#![cfg(unix)]

use cowboy_agent_client::{
    AgentAccess, BackendConnector, Client, Event, PromptContent, PromptTurnCancellation, StopReason,
};
use cowboy_agent_pi::{PiRpcBackendConnector, PiRpcClient};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    binary: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let binary = root.join("pi-stub.sh");
        let script = r#"#!/bin/sh
root='__ROOT__'
mode='__MODE__'
printf 'spawn\n' >> "$root/spawns"
restore=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    --session-id) shift; sid="$1"; file="$root/$sid.jsonl" ;;
    --session) shift; file="$1"; restore=true ;;
  esac
  shift
done
if [ "$restore" = true ]; then
  [ -f "$file" ] || exit 73
  sid=$(head -n 1 "$file" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  stored_cwd=$(head -n 1 "$file" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')
  cd "$stored_cwd" || exit 74
else
  printf '{"type":"session","version":3,"id":"%s","cwd":"%s"}\n' "$sid" "$(pwd -P)" > "$file"
fi
pwd -P > "$root/cwd"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"get_state"'*)
      [ "$mode" = hang_state ] && continue
      actual="$sid"; [ "$mode" = wrong_identity ] && actual=wrong
      printf '%s\n' '{"id":"'"$id"'","type":"response","command":"get_state","success":true,"data":{"sessionId":"'"$actual"'","sessionFile":"'"$file"'","model":{"id":"stub-model"},"thinkingLevel":"high"}}'
      [ "$mode" = block_input ] && sleep 30
      ;;
    *'"type":"get_session_stats"'*)
      count=$(grep -c '"type":"message"' "$file" || true)
      printf '%s\n' '{"id":"'"$id"'","type":"response","command":"get_session_stats","success":true,"data":{"sessionId":"'"$sid"'","totalMessages":'"$count"'}}'
      ;;
    *'"type":"abort"'*)
      printf 'abort\n' >> "$root/aborts"
      [ "$mode" = abort_no_ack ] && continue
      printf '%s\n' '{"id":"'"$id"'","type":"response","command":"abort","success":true}'
      [ "$mode" = cancel_ok ] && printf '%s\n' '{"type":"agent_settled","aborted":true}'
      ;;
    *'"type":"prompt"'*)
      printf 'prompt\n' >> "$root/prompts"
      if [ "$mode" = error ]; then
        printf '%s\n' '{"id":"'"$id"'","type":"response","command":"prompt","success":false,"error":"synthetic provider unavailable"}'
        continue
      fi
      printf '%s\n' '{"id":"'"$id"'","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}'
      case "$mode" in abort_no_ack|abort_no_settle|cancel_ok) continue ;; esac
      [ "$mode" = exit ] && exit 3
      printf '%s\n' '{"type":"message","message":{"role":"assistant"}}' >> "$file"
      printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"---\nstatus: success\nsummary: fixture\n---\ndone"}}'
      printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"checking"}}'
      printf '%s\n' '{"type":"tool_execution_start","toolCallId":"call_1","toolName":"bash"}'
      printf '%s\n' '{"type":"tool_execution_end","toolCallId":"call_1","isError":false}'
      printf '%s\n' '{"type":"agent_end"}'
      [ "$mode" = delayed_settle ] && sleep 0.15
      printf '%s\n' '{"type":"agent_settled","aborted":false}'
      ;;
  esac
done
"#.replace("__ROOT__", root.to_str().unwrap()).replace("__MODE__", mode);
        std::fs::write(&binary, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _dir: dir,
            root,
            binary,
        }
    }
    fn spec(&self) -> cowboy_agent_client::AgentLaunchSpec {
        cowboy_agent_client::AgentLaunchSpec {
            agent: "fixture".into(),
            backend: "pi-rpc".into(),
            command: self.binary.to_string_lossy().into(),
            args: vec![],
            allowed_env: vec![],
            access: AgentAccess::default(),
            watchdog: cowboy_agent_client::AgentWatchdogOptions {
                response_timeout_seconds: 10,
                cancel_timeout_seconds: 1,
                recovery_operation_timeout_seconds: 1,
            },
            startup_readiness: None,
            model: None,
        }
    }
    async fn client(&self) -> (PiRpcClient, String) {
        let mut client =
            PiRpcClient::connect(&self.spec(), Some("11111111-1111-4111-8111-111111111111"))
                .await
                .unwrap();
        let id = client
            .new_session(self.root.to_str().unwrap(), &[], None)
            .await
            .unwrap();
        (client, id)
    }
    fn count(&self, name: &str) -> usize {
        std::fs::read_to_string(self.root.join(name))
            .unwrap_or_default()
            .lines()
            .count()
    }
}

async fn turn(client: &mut dyn Client, id: &str) -> anyhow::Result<(StopReason, Vec<Event>)> {
    let mut events = Vec::new();
    let reason = client
        .prompt(
            id,
            vec![PromptContent::text("synthetic work")],
            PromptTurnCancellation::disabled(),
            &mut |e| events.push(e),
        )
        .await?;
    Ok((reason, events))
}

#[tokio::test]
async fn fresh_connector_must_not_bypass_executor_session_establishment() {
    let fixture = Fixture::new("normal");
    let mut resolved = PiRpcBackendConnector
        .connect(&fixture.spec())
        .await
        .unwrap();
    assert!(resolved.client.session_id().is_none());
    assert_eq!(
        fixture.count("spawns"),
        0,
        "no process before the session cwd is known"
    );
    let id = resolved
        .client
        .new_session(fixture.root.to_str().unwrap(), &[], None)
        .await
        .unwrap();
    assert_eq!(resolved.client.session_id(), Some(id.as_str()));
    assert!(std::path::Path::new(&id).is_absolute());
    assert_eq!(fixture.count("spawns"), 1);
    resolved.client.close().await.unwrap();
}

#[tokio::test]
async fn prompt_streams_events_and_settles_without_interleaving() {
    let fixture = Fixture::new("normal");
    let (mut client, id) = fixture.client().await;
    for _ in 0..3 {
        let (reason, events) = turn(&mut client, &id).await.unwrap();
        assert!(matches!(reason, StopReason::EndTurn));
        let chunks: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let Event::MessageChunk { content } = e {
                    content["text"].as_str()
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("status: success"));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ThoughtChunk { .. }))
        );
        assert!(events.iter().any(|e|matches!(e,Event::ToolCall{tool_call_id,status,..} if tool_call_id=="call_1"&&status=="in_progress")));
        assert!(
            events
                .iter()
                .any(|e| matches!(e,Event::ToolCallUpdate{status,..} if status=="completed"))
        );
    }
    assert_eq!(fixture.count("prompts"), 3);
    let descriptor = client.session_descriptor().unwrap();
    assert_eq!(descriptor.model.as_deref(), Some("stub-model"));
    assert_eq!(descriptor.reasoning.as_deref(), Some("high"));
    client.close().await.unwrap();
    assert!(!client.is_connected());
}

#[tokio::test]
async fn agent_end_is_not_settlement() {
    let fixture = Fixture::new("delayed_settle");
    let (mut client, id) = fixture.client().await;
    let start = Instant::now();
    turn(&mut client, &id).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(120));
    client.close().await.unwrap();
}

#[tokio::test]
async fn restore_preserves_existing_history_and_rejects_different_runtime_cwd() {
    let fixture = Fixture::new("normal");
    let (mut first, id) = fixture.client().await;
    turn(&mut first, &id).await.unwrap();
    let before = std::fs::read(&id).unwrap();
    first.close().await.unwrap();
    let other = fixture.root.join("next-checkout");
    std::fs::create_dir(&other).unwrap();
    let mut restored = PiRpcBackendConnector
        .connect(&fixture.spec())
        .await
        .unwrap();
    assert!(restored.client.session_id().is_none());
    let launches = fixture.count("spawns");
    assert!(
        restored
            .client
            .load_session(&id, other.to_str().unwrap(), &[])
            .await
            .is_err()
    );
    assert_eq!(
        fixture.count("spawns"),
        launches,
        "different stored runtime cwd must fail before launch"
    );
    restored
        .client
        .load_session(&id, fixture.root.to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_eq!(restored.client.session_id(), Some(id.as_str()));
    assert_eq!(
        std::fs::read(&id).unwrap(),
        before,
        "loading must not replace history"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("cwd"))
            .unwrap()
            .trim(),
        fixture.root.to_str().unwrap()
    );
    turn(restored.client.as_mut(), &id).await.unwrap();
    assert!(std::fs::read(&id).unwrap().len() > before.len());
    restored.client.close().await.unwrap();
}

#[tokio::test]
async fn resumed_prompt_retains_the_exact_saved_opaque_alias() {
    let fixture = Fixture::new("normal");
    let (mut original, id) = fixture.client().await;
    turn(&mut original, &id).await.unwrap();
    original.close().await.unwrap();
    let alias = fixture.root.join("session-alias.jsonl");
    std::os::unix::fs::symlink(&id, &alias).unwrap();
    let saved_reference = alias.to_str().unwrap();
    let mut restored = PiRpcBackendConnector
        .connect(&fixture.spec())
        .await
        .unwrap();
    restored
        .client
        .load_session(saved_reference, fixture.root.to_str().unwrap(), &[])
        .await
        .unwrap();
    assert_eq!(restored.client.session_id(), Some(saved_reference));
    // The unchanged executor passes its saved reference, not a canonicalized replacement.
    turn(restored.client.as_mut(), saved_reference)
        .await
        .unwrap();
    restored.client.close().await.unwrap();
}

#[tokio::test]
async fn backpressured_io_shutdown_is_bounded_and_invalidates_the_turn() {
    let fixture = Fixture::new("block_input");
    let (mut client, id) = fixture.client().await;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        client.prompt(
            &id,
            vec![PromptContent::text("x".repeat(1_048_576))],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        ),
    )
    .await
    .expect("a blocked pipe must not hang command timeout/shutdown");
    assert!(result.is_err());
    assert!(!client.is_connected());
    assert!(client.session_id().is_none());
    client.close().await.unwrap();
}

#[tokio::test]
async fn missing_or_empty_saved_history_is_not_created_or_claimed() {
    let fixture = Fixture::new("normal");
    let (mut original, id) = fixture.client().await;
    original.close().await.unwrap();
    let mut restored = PiRpcClient::connect(&fixture.spec(), None).await.unwrap();
    assert!(
        restored
            .load_session(&id, fixture.root.to_str().unwrap(), &[])
            .await
            .is_err(),
        "header-only history is not a restored conversation"
    );
    assert!(restored.session_id().is_none());
    restored.close().await.unwrap();
    std::fs::remove_file(&id).unwrap();
    let spawned = fixture.count("spawns");
    let mut missing = PiRpcClient::connect(&fixture.spec(), None).await.unwrap();
    assert!(
        missing
            .load_session(&id, fixture.root.to_str().unwrap(), &[])
            .await
            .is_err()
    );
    assert!(!std::path::Path::new(&id).exists());
    assert_eq!(
        fixture.count("spawns"),
        spawned,
        "missing history must fail before launch"
    );
    assert!(missing.session_id().is_none());
    missing.close().await.unwrap();
}

#[tokio::test]
async fn new_session_honors_cwd_and_rejects_mismatched_identity() {
    let fixture = Fixture::new("normal");
    let other = fixture.root.join("checkout");
    std::fs::create_dir(&other).unwrap();
    let mut client = PiRpcClient::connect(&fixture.spec(), None).await.unwrap();
    client
        .new_session(other.to_str().unwrap(), &[], None)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("cwd"))
            .unwrap()
            .trim(),
        other.to_str().unwrap()
    );
    client.close().await.unwrap();
    let wrong = Fixture::new("wrong_identity");
    let mut client = PiRpcClient::connect(&wrong.spec(), None).await.unwrap();
    assert!(
        client
            .new_session(wrong.root.to_str().unwrap(), &[], None)
            .await
            .is_err()
    );
    assert!(client.session_id().is_none());
    client.close().await.unwrap();
}

#[tokio::test]
async fn unsupported_access_fails_before_any_process_or_prompt() {
    for access in [
        AgentAccess::DenyAll,
        AgentAccess::DenyEscalation {
            acp_mode: "read-only".into(),
        },
    ] {
        let fixture = Fixture::new("normal");
        let mut spec = fixture.spec();
        spec.access = access;
        assert!(PiRpcBackendConnector.connect(&spec).await.is_err());
        assert_eq!(fixture.count("spawns"), 0);
        assert_eq!(fixture.count("prompts"), 0);
    }
}

#[tokio::test]
async fn missing_state_response_is_bounded_and_not_established() {
    let fixture = Fixture::new("hang_state");
    let mut client = PiRpcClient::connect(&fixture.spec(), None).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        client.new_session(fixture.root.to_str().unwrap(), &[], None),
    )
    .await
    .expect("state handshake must be bounded");
    assert!(result.is_err());
    assert!(client.session_id().is_none());
    client.close().await.unwrap();
}

#[tokio::test]
async fn actual_local_cancellation_is_one_shot_and_bounded() {
    for mode in ["cancel_ok", "abort_no_ack", "abort_no_settle"] {
        let fixture = Fixture::new(mode);
        let (mut client, id) = fixture.client().await;
        let cancel = PromptTurnCancellation::from_future(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(4),
            client.prompt(
                &id,
                vec![PromptContent::text("synthetic work")],
                cancel,
                &mut |_| {},
            ),
        )
        .await
        .expect("cancellation must not hang or resend abort forever");
        assert_eq!(fixture.count("aborts"), 1);
        if mode == "cancel_ok" {
            assert!(matches!(result.unwrap(), StopReason::Cancelled));
        } else {
            assert!(result.is_err());
            assert!(!client.is_connected());
            assert!(client.session_id().is_none());
        }
        client.close().await.unwrap();
    }
}

#[tokio::test]
async fn process_exit_and_command_error_fail_without_success() {
    for mode in ["exit", "error"] {
        let fixture = Fixture::new(mode);
        let (mut client, id) = fixture.client().await;
        assert!(turn(&mut client, &id).await.is_err());
        client.close().await.unwrap();
    }
}
