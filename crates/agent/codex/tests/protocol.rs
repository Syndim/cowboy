use cowboy_agent_client::{
    AgentAccess, AgentLaunchSpec, AgentWatchdogOptions, BackendConnector, Event, ModelInfo,
    PromptContent, PromptTurnCancellation, StopReason,
};
use cowboy_agent_codex::CodexBackendConnector;
use serde_json::Value;
use std::time::Duration;

fn spec(log: &std::path::Path, mode: &str) -> AgentLaunchSpec {
    AgentLaunchSpec {
        agent: "default".into(),
        backend: "codex-app-server".into(),
        command: env!("CARGO_BIN_EXE_codex-mock-server").into(),
        args: vec![log.to_string_lossy().into(), mode.into()],
        allowed_env: vec![],
        access: AgentAccess::Default,
        watchdog: AgentWatchdogOptions {
            response_timeout_seconds: 2,
            cancel_timeout_seconds: 2,
            recovery_operation_timeout_seconds: 2,
        },
        startup_readiness: None,
        model: Some(ModelInfo {
            id: "requested-model".into(),
            provider: Some("provider".into()),
        }),
    }
}
fn messages(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .skip(1)
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[tokio::test]
async fn sessions_stream_approvals_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let spec = spec(&log, "normal");
    let mut client = CodexBackendConnector.connect(&spec).await.unwrap().client;
    let cwd = dir.path().to_str().unwrap();
    assert_eq!(
        client
            .new_session(cwd, &[], spec.model.as_ref())
            .await
            .unwrap(),
        "thread-1"
    );
    assert_eq!(
        client.session_descriptor().unwrap().model.as_deref(),
        Some("returned-model")
    );
    let mut events = Vec::new();
    for _ in 0..2 {
        let stop = client
            .prompt(
                "thread-1",
                vec![PromptContent::text("hi")],
                PromptTurnCancellation::disabled(),
                &mut |event| events.push(event),
            )
            .await
            .unwrap();
        assert!(matches!(stop, StopReason::EndTurn));
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::MessageChunk { .. }))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::ToolCall { .. }))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::ToolCallUpdate { .. }))
            .count(),
        2
    );
    let requests = messages(&log);
    let start = requests
        .iter()
        .find(|m| m["method"] == "thread/start")
        .unwrap();
    assert_eq!(
        start["params"]["cwd"],
        std::fs::canonicalize(cwd).unwrap().to_str().unwrap()
    );
    assert_eq!(start["params"]["model"], "requested-model");
    assert_eq!(start["params"]["modelProvider"], "provider");
    assert_eq!(start["params"]["sandbox"], "workspace-write");
    assert_eq!(start["params"]["approvalsReviewer"], "user");
    assert_eq!(
        requests
            .iter()
            .filter(|m| m["result"]["decision"] == "decline")
            .count(),
        2
    );
    client.close().await.unwrap();
    let mut restored = CodexBackendConnector.connect(&spec).await.unwrap().client;
    restored.load_session("thread-1", cwd, &[]).await.unwrap();
    let requests = messages(&log);
    let resume = requests
        .iter()
        .find(|m| m["method"] == "thread/resume")
        .unwrap();
    assert_eq!(resume["params"]["threadId"], "thread-1");
    assert_eq!(resume["params"]["model"], "requested-model");
    assert_eq!(resume["params"]["sandbox"], "workspace-write");
    restored.close().await.unwrap();
}
#[tokio::test]
async fn cancellation_interrupts_and_failed_turn_is_error() {
    for mode in ["cancel", "cancel-before-start", "error"] {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let spec = spec(&log, mode);
        let mut client = CodexBackendConnector.connect(&spec).await.unwrap().client;
        client
            .new_session(dir.path().to_str().unwrap(), &[], None)
            .await
            .unwrap();
        let cancel = if mode != "error" {
            PromptTurnCancellation::from_future(async {
                tokio::time::sleep(Duration::from_millis(100)).await
            })
        } else {
            PromptTurnCancellation::disabled()
        };
        let result = client
            .prompt(
                "thread-1",
                vec![PromptContent::text("hi")],
                cancel,
                &mut |_| {},
            )
            .await;
        if mode != "error" {
            assert!(matches!(result.unwrap(), StopReason::Cancelled));
            assert!(
                messages(&log)
                    .iter()
                    .any(|m| m["method"] == "turn/interrupt")
            );
        } else {
            let err = result.unwrap_err().to_string();
            assert!(err.contains("turn failed"));
            assert!(!err.contains("secret"));
            assert!(!client.is_connected());
        }
        client.close().await.unwrap();
    }
}
#[tokio::test]
async fn rejects_unenforceable_policies_and_unsafe_server() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let mut spec = spec(&log, "unsafe");
    spec.access = AgentAccess::DenyAll;
    assert!(CodexBackendConnector.connect(&spec).await.is_err());
    spec.access = AgentAccess::Default;
    let mut client = CodexBackendConnector.connect(&spec).await.unwrap().client;
    assert!(
        client
            .new_session(dir.path().to_str().unwrap(), &[serde_json::json!({})], None)
            .await
            .is_err()
    );
    assert!(
        client
            .new_session(dir.path().to_str().unwrap(), &[], None)
            .await
            .unwrap_err()
            .to_string()
            .contains("restricted sandbox")
    );
    assert!(!client.is_connected());
}

#[tokio::test]
async fn timeout_terminates_process_and_invalidates_session() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let mut spec = spec(&log, "hang");
    spec.watchdog.response_timeout_seconds = 1;
    let mut client = CodexBackendConnector.connect(&spec).await.unwrap().client;
    client
        .new_session(dir.path().to_str().unwrap(), &[], None)
        .await
        .unwrap();
    let pid: i32 = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let result = client
        .prompt(
            "thread-1",
            vec![PromptContent::text("hi")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(!client.is_connected());
    #[cfg(unix)]
    {
        // SAFETY: signal 0 checks for the test-owned child without sending a signal.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Opt-in because it uses the locally authenticated Codex installation and network.
#[tokio::test]
#[ignore]
async fn real_codex_smoke() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec(&dir.path().join("unused"), "normal");
    spec.command = "codex".into();
    spec.args = vec!["app-server".into()];
    spec.allowed_env = [
        "PATH",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "SystemRoot",
        "TMP",
        "TEMP",
        "CODEX_HOME",
        "OPENAI_API_KEY",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    spec.model = None;
    spec.watchdog.response_timeout_seconds = 120;
    let mut client = CodexBackendConnector.connect(&spec).await.unwrap().client;
    let cwd = dir.path().to_str().unwrap();
    let session = client.new_session(cwd, &[], None).await.unwrap();
    let mut text = String::new();
    let stop = client
        .prompt(
            &session,
            vec![PromptContent::text(
                "Reply with the single word HELLO, without using tools.",
            )],
            PromptTurnCancellation::disabled(),
            &mut |e| {
                if let Event::MessageChunk { content } = e
                    && let Some(chunk) = content.get("text").and_then(Value::as_str)
                {
                    text.push_str(chunk);
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(stop, StopReason::EndTurn));
    assert!(text.to_uppercase().contains("HELLO"));
    client.close().await.unwrap();
    let mut resumed = CodexBackendConnector.connect(&spec).await.unwrap().client;
    resumed.load_session(&session, cwd, &[]).await.unwrap();
    resumed.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn closing_or_dropping_client_kills_descendants() {
    for close in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let spec = spec(&log, "tree");
        let mut client = CodexBackendConnector.connect(&spec).await.unwrap().client;
        client
            .new_session(dir.path().to_str().unwrap(), &[], None)
            .await
            .unwrap();
        if close {
            client.close().await.unwrap();
        }
        drop(client);
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!log.with_extension("survivor").exists());
    }
}
