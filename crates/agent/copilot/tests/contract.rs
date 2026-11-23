#![cfg(unix)]

use cowboy_agent_client::{
    AgentAccess, AgentLaunchSpec, AgentWatchdogOptions, BackendConnector, Event, ModelInfo,
    PromptContent, PromptTurnCancellation, StopReason,
};
use cowboy_agent_copilot::CopilotSdkBackendConnector;

fn spec() -> AgentLaunchSpec {
    AgentLaunchSpec {
        agent: "test".into(),
        backend: "copilot".into(),
        command: env!("CARGO_BIN_EXE_copilot-test-runtime").into(),
        args: vec![],
        allowed_env: vec![],
        access: AgentAccess::Default,
        watchdog: AgentWatchdogOptions {
            response_timeout_seconds: 3,
            cancel_timeout_seconds: 2,
            recovery_operation_timeout_seconds: 3,
        },
        startup_readiness: None,
        model: None,
    }
}

#[tokio::test]
async fn create_stream_close_and_reload() {
    let connector = CopilotSdkBackendConnector::default();
    let spec = spec();
    let mut client = connector.connect(&spec).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let cwd = cwd.to_str().unwrap();
    let session = client
        .new_session(cwd, &[], Some(&ModelInfo::with_id("test-model")))
        .await
        .unwrap();
    let mut events = Vec::new();
    let result = client
        .prompt(
            &session,
            vec![PromptContent::text("hi")],
            PromptTurnCancellation::disabled(),
            &mut |event| events.push(event),
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    assert!(
        matches!(&events[..], [Event::MessageChunk { content }, Event::ToolCall { .. }, Event::ToolCallUpdate { .. }, Event::ToolCallUpdate { .. }] if content["text"] == "hello")
    );
    let mut stale_events = Vec::new();
    client
        .prompt(
            &session,
            vec![PromptContent::text("stale-idle")],
            PromptTurnCancellation::disabled(),
            &mut |event| stale_events.push(event),
        )
        .await
        .unwrap();
    assert!(matches!(
        stale_events.first(),
        Some(Event::MessageChunk { .. })
    ));
    client.close().await.unwrap();
    let mut client = connector.connect(&spec).await.unwrap().client;
    client.load_session(&session, cwd, &[]).await.unwrap();
    assert_eq!(client.session_id(), Some(session.as_str()));
    let result = client
        .prompt(
            &session,
            vec![PromptContent::text("again")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    client.close().await.unwrap();
}

#[tokio::test]
async fn rejects_unsupported_options() {
    let connector = CopilotSdkBackendConnector::default();
    let mut spec = spec();
    spec.args = vec!["--acp".into()];
    assert!(connector.connect(&spec).await.is_err());
    spec.args.clear();
    spec.access = AgentAccess::DenyAll;
    assert!(connector.connect(&spec).await.is_err());
    spec.access = AgentAccess::DenyEscalation {
        acp_mode: "safe".into(),
    };
    assert!(connector.connect(&spec).await.is_err());
    spec.access = AgentAccess::Default;
    spec.model = Some(ModelInfo {
        id: "model".into(),
        provider: Some("provider".into()),
    });
    assert!(connector.connect(&spec).await.is_err());
    spec.model = None;
    let mut client = connector.connect(&spec).await.unwrap().client;
    assert!(
        client
            .new_session(".", &[serde_json::json!({})], None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_then_another_turn_and_error_closes_client() {
    let connector = CopilotSdkBackendConnector::default();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut tx = Some(tx);
    let cancellation = PromptTurnCancellation::from_future(async {
        let _ = rx.await;
    });
    let result = client
        .prompt(
            &id,
            vec![PromptContent::text("wait")],
            cancellation,
            &mut |_| {
                if let Some(tx) = tx.take() {
                    let _ = tx.send(());
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::Cancelled));
    let result = client
        .prompt(
            &id,
            vec![PromptContent::text("again")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    let error = client
        .prompt(
            &id,
            vec![PromptContent::text("error")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private secret"));
    assert!(!client.is_connected());
}

#[tokio::test]
async fn cancellation_during_delayed_ack_and_abort_keeps_session_usable() {
    for prompt in ["delayed-ack", "delayed-abort"] {
        let connector = CopilotSdkBackendConnector::default();
        let mut config = spec();
        config.watchdog.cancel_timeout_seconds = 10;
        let mut client = connector.connect(&config).await.unwrap().client;
        let cwd = std::env::current_dir().unwrap();
        let id = client
            .new_session(
                cwd.to_str().unwrap(),
                &[],
                Some(&ModelInfo::with_id("test-model")),
            )
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut tx = Some(tx);
        let (event_tx, event_rx) = tokio::sync::oneshot::channel();
        let mut event_tx = Some(event_tx);
        let mut abort_events = 0;
        let result = {
            let mut handler = |event| {
                if let Event::MessageChunk { content } = event {
                    if content["text"] == "x" {
                        abort_events += 1;
                        if let Some(tx) = event_tx.take() {
                            let _ = tx.send(());
                        }
                    } else if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                }
            };
            let turn = client.prompt(
                &id,
                vec![PromptContent::text(prompt)],
                PromptTurnCancellation::from_future(async {
                    let _ = rx.await;
                }),
                &mut handler,
            );
            tokio::pin!(turn);
            tokio::select! {
                result = &mut turn => panic!("{prompt} completed before abort events: {result:?}"),
                result = tokio::time::timeout(std::time::Duration::from_secs(5), event_rx) => {
                    result.expect("events were not consumed while abort was pending").unwrap();
                }
            }
            turn.await
                .unwrap_or_else(|error| panic!("{prompt}: {error:#}"))
        };
        assert!(matches!(result, StopReason::Cancelled), "{prompt}");
        assert_eq!(
            abort_events, 100,
            "events must be consumed while abort is pending: {prompt}"
        );
        assert!(client.is_connected());
        assert!(matches!(
            client
                .prompt(
                    &id,
                    vec![PromptContent::text("again")],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .unwrap(),
            StopReason::EndTurn
        ));
        client.close().await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_deadline_covers_ack_and_abort_and_fails_closed() {
    for prompt in ["cancel-budget", "no-ack"] {
        let connector = CopilotSdkBackendConnector::default();
        let mut config = spec();
        config.watchdog.cancel_timeout_seconds = 1;
        let mut client = connector.connect(&config).await.unwrap().client;
        let cwd = std::env::current_dir().unwrap();
        let id = client
            .new_session(
                cwd.to_str().unwrap(),
                &[],
                Some(&ModelInfo::with_id("test-model")),
            )
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut tx = Some(tx);
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(1600),
            client.prompt(
                &id,
                vec![PromptContent::text(prompt)],
                PromptTurnCancellation::from_future(async {
                    let _ = rx.await;
                }),
                &mut |_| {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                },
            ),
        )
        .await
        .expect("single cancellation deadline was exceeded");
        assert!(result.is_err(), "{prompt}");
        assert!(started.elapsed() >= std::time::Duration::from_millis(900));
        assert!(!client.is_connected());
    }
}

#[tokio::test]
async fn preserves_stream_order_while_filtering_unrelated_origins() {
    let connector = CopilotSdkBackendConnector::default();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    let mut messages = Vec::new();
    let result = client
        .prompt(
            &id,
            vec![PromptContent::text("origins")],
            PromptTurnCancellation::disabled(),
            &mut |event| {
                if let Event::MessageChunk { content } = event {
                    messages.push(content["text"].as_str().unwrap().to_owned());
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    assert_eq!(
        messages,
        ["pre-current", "pre-delta", "live", "post-current"]
    );
    client.close().await.unwrap();
}

#[tokio::test]
async fn failed_reload_is_not_replaced_and_terminate_all_kills_runtime() {
    let connector = CopilotSdkBackendConnector::default();
    let cwd = std::env::current_dir().unwrap();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    assert!(
        client
            .load_session("missing-session", cwd.to_str().unwrap(), &[])
            .await
            .is_err()
    );
    assert!(client.session_id().is_none());
    assert!(!client.is_connected());
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    assert_eq!(
        connector
            .terminate_all_agents(std::time::Duration::from_secs(2))
            .await,
        1
    );
    assert!(
        client
            .prompt(
                &id,
                vec![PromptContent::text("hi")],
                PromptTurnCancellation::disabled(),
                &mut |_| {}
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn permission_requests_are_rejected_and_autopilot_waits_for_stop() {
    let connector = CopilotSdkBackendConnector::default();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    let result = client
        .prompt(
            &id,
            vec![PromptContent::text("permission")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    let mut messages = Vec::new();
    let result = client
        .prompt(
            &id,
            vec![PromptContent::text("autopilot")],
            PromptTurnCancellation::disabled(),
            &mut |event| messages.push(event),
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    assert!(
        matches!(&messages[..], [Event::MessageChunk { content }] if content["text"] == "after idle")
    );
    client.close().await.unwrap();
}

#[tokio::test]
async fn inactivity_timeout_closes_client() {
    let connector = CopilotSdkBackendConnector::default();
    let mut config = spec();
    config.watchdog.response_timeout_seconds = 1;
    let mut client = connector.connect(&config).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    assert!(
        client
            .prompt(
                &id,
                vec![PromptContent::text("wait")],
                PromptTurnCancellation::disabled(),
                &mut |_| {}
            )
            .await
            .is_err()
    );
    assert!(!client.is_connected());
}

#[tokio::test]
async fn nonstreaming_message_is_not_lost() {
    let connector = CopilotSdkBackendConnector::default();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    let mut messages = Vec::new();
    client
        .prompt(
            &id,
            vec![PromptContent::text("no-delta")],
            PromptTurnCancellation::disabled(),
            &mut |event| {
                if let Event::MessageChunk { content } = event {
                    messages.push(content);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["text"], "hello");
    client.close().await.unwrap();
}

#[tokio::test]
async fn dropping_a_prompt_future_terminates_the_runtime() {
    let connector = CopilotSdkBackendConnector::default();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        client.prompt(
            &id,
            vec![PromptContent::text("wait")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        ),
    )
    .await;
    assert!(result.is_err());
    assert!(!client.is_connected());
}

#[tokio::test]
async fn rejects_unconfirmed_permission_modes_on_resume() {
    let connector = CopilotSdkBackendConnector::default();
    let cwd = std::env::current_dir().unwrap();
    for id in ["refused", "missing-mode"] {
        let mut client = connector.connect(&spec()).await.unwrap().client;
        assert!(
            client
                .load_session(id, cwd.to_str().unwrap(), &[])
                .await
                .is_err()
        );
        assert!(!client.is_connected());
    }
}

#[tokio::test]
async fn policy_block_and_rpc_errors_fail_closed_and_redact() {
    let connector = CopilotSdkBackendConnector::default();
    let cwd = std::env::current_dir().unwrap();
    for prompt in ["policy", "rpc-error"] {
        let mut client = connector.connect(&spec()).await.unwrap().client;
        let id = client
            .new_session(
                cwd.to_str().unwrap(),
                &[],
                Some(&ModelInfo::with_id("test-model")),
            )
            .await
            .unwrap();
        let error = client
            .prompt(
                &id,
                vec![PromptContent::text(prompt)],
                PromptTurnCancellation::disabled(),
                &mut |_| {},
            )
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("rpc-secret-canary"));
        assert!(!client.is_connected());
    }
}

#[tokio::test]
async fn handles_pre_ack_continuations_empty_root_and_exit_plan() {
    let connector = CopilotSdkBackendConnector::default();
    let cwd = std::env::current_dir().unwrap();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    for prompt in ["pre-ack", "exit-plan"] {
        assert!(matches!(
            client
                .prompt(
                    &id,
                    vec![PromptContent::text(prompt)],
                    PromptTurnCancellation::disabled(),
                    &mut |_| {}
                )
                .await
                .unwrap_or_else(|e| panic!("{prompt}: {e}")),
            StopReason::EndTurn
        ));
    }
    let mut events = Vec::new();
    client
        .prompt(
            &id,
            vec![PromptContent::text("empty-root")],
            PromptTurnCancellation::disabled(),
            &mut |e| events.push(e),
        )
        .await
        .unwrap();
    assert!(matches!(&events[..], [Event::MessageChunk { content }] if content["text"] == "root"));
    client.close().await.unwrap();
}

#[tokio::test]
async fn running_tool_rearms_watchdog() {
    let connector = CopilotSdkBackendConnector::default();
    let mut config = spec();
    config.watchdog.response_timeout_seconds = 1;
    let cwd = std::env::current_dir().unwrap();
    let mut client = connector.connect(&config).await.unwrap().client;
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    assert!(matches!(
        client
            .prompt(
                &id,
                vec![PromptContent::text("slow-tool")],
                PromptTurnCancellation::disabled(),
                &mut |_| {}
            )
            .await
            .unwrap(),
        StopReason::EndTurn
    ));
    client.close().await.unwrap();
}

#[tokio::test]
async fn resume_returns_buffered_startup_events_and_dropped_close_kills() {
    let connector = CopilotSdkBackendConnector::default();
    let cwd = std::env::current_dir().unwrap();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let events = client
        .load_session("startup-events", cwd.to_str().unwrap(), &[])
        .await
        .unwrap();
    assert!(
        matches!(&events[..], [Event::MessageChunk { content }] if content["text"] == "startup")
    );
    client.close().await.unwrap();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    client
        .load_session("close-hang", cwd.to_str().unwrap(), &[])
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), client.close())
            .await
            .is_err()
    );
    assert!(!client.is_connected());
}

#[tokio::test]
async fn rejects_unconfirmed_permission_modes_on_create() {
    let connector = CopilotSdkBackendConnector::default();
    let cwd = std::env::current_dir().unwrap();
    for model in ["refused-model", "missing-mode-model"] {
        let mut client = connector.connect(&spec()).await.unwrap().client;
        assert!(
            client
                .new_session(cwd.to_str().unwrap(), &[], Some(&ModelInfo::with_id(model)))
                .await
                .is_err()
        );
        assert!(!client.is_connected());
    }
}

#[tokio::test]
async fn finished_tool_does_not_disable_timeout() {
    let connector = CopilotSdkBackendConnector::default();
    let mut config = spec();
    config.watchdog.response_timeout_seconds = 1;
    let cwd = std::env::current_dir().unwrap();
    let mut client = connector.connect(&config).await.unwrap().client;
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    assert!(
        client
            .prompt(
                &id,
                vec![PromptContent::text("tool-finished-stall")],
                PromptTurnCancellation::disabled(),
                &mut |_| {}
            )
            .await
            .is_err()
    );
    assert!(!client.is_connected());
}

#[tokio::test]
async fn close_timeout_reports_failure() {
    let connector = CopilotSdkBackendConnector::default();
    let mut config = spec();
    config.watchdog.recovery_operation_timeout_seconds = 1;
    let cwd = std::env::current_dir().unwrap();
    let mut client = connector.connect(&config).await.unwrap().client;
    client
        .load_session("close-hang", cwd.to_str().unwrap(), &[])
        .await
        .unwrap();
    assert!(client.close().await.is_err());
    assert!(!client.is_connected());
}

#[tokio::test]
async fn sdk_rpc_errors_do_not_leak_into_product_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = cowboy_log::init_file_logging(dir.path(), "sdk-test", "trace").unwrap();
    let connector = CopilotSdkBackendConnector::default();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    let error = client
        .prompt(
            &id,
            vec![PromptContent::text("rpc-error")],
            PromptTurnCancellation::disabled(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("rpc-secret-canary"));
    let contents = std::fs::read_to_string(path).unwrap();
    assert!(!contents.contains("rpc-secret-canary"));
}

#[tokio::test]
async fn completed_turn_before_ack_survives_cancellation() {
    let connector = CopilotSdkBackendConnector::default();
    let mut client = connector.connect(&spec()).await.unwrap().client;
    let cwd = std::env::current_dir().unwrap();
    let id = client
        .new_session(
            cwd.to_str().unwrap(),
            &[],
            Some(&ModelInfo::with_id("test-model")),
        )
        .await
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut tx = Some(tx);
    let result = client
        .prompt(
            &id,
            vec![PromptContent::text("completed-before-ack")],
            PromptTurnCancellation::from_future(async {
                let _ = rx.await;
            }),
            &mut |_| {
                if let Some(tx) = tx.take() {
                    let _ = tx.send(());
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    assert!(matches!(
        client
            .prompt(
                &id,
                vec![PromptContent::text("again")],
                PromptTurnCancellation::disabled(),
                &mut |_| {}
            )
            .await
            .unwrap(),
        StopReason::EndTurn
    ));
    client.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a compatible runtime, authentication, and network access"]
async fn authenticated_smoke() {
    let mut config = spec();
    config.command = std::env::var("COWBOY_COPILOT_RUNTIME")
        .expect("set COWBOY_COPILOT_RUNTIME to the absolute runtime path");
    config.allowed_env = [
        "HOME",
        "PATH",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "COPILOT_GITHUB_TOKEN",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    config.watchdog.response_timeout_seconds = 120;
    config.watchdog.recovery_operation_timeout_seconds = 60;
    let connector = CopilotSdkBackendConnector::default();
    let cwd = std::env::current_dir().unwrap();
    let cwd = cwd.to_str().unwrap();
    let mut client = connector.connect(&config).await.unwrap().client;
    let id = client.new_session(cwd, &[], None).await.unwrap();
    let mut text_received = false;
    let result = client
        .prompt(
            &id,
            vec![PromptContent::text("Reply with exactly: ok")],
            PromptTurnCancellation::disabled(),
            &mut |event| {
                if let Event::MessageChunk { content } = event {
                    text_received |= content["text"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty());
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, StopReason::EndTurn));
    assert!(text_received);
    client.close().await.unwrap();
    let mut client = connector.connect(&config).await.unwrap().client;
    client.load_session(&id, cwd, &[]).await.unwrap();
    assert_eq!(client.session_id(), Some(id.as_str()));
    client.close().await.unwrap();
}
