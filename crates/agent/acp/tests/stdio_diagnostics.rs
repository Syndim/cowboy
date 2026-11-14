#![cfg(unix)]

#[path = "support/diagnostics.rs"]
mod diagnostics;

use cowboy_agent_acp::transport::stdio::StdioTransport;
use cowboy_agent_acp::transport::{StdioConfig, Transport};
use cowboy_agent_acp::{AgentWatchdogOptions, Client, TransportConfig};
use cowboy_agent_client::{Event, PromptContent, PromptTurnCancellation, StopReason};
use diagnostics::DiagnosticCapture;
use std::time::Duration;
use tracing::instrument::WithSubscriber;

fn peer(script: &str) -> StdioConfig {
    StdioConfig {
        command: "sh".into(),
        args: vec!["-c".into(), script.into(), "private-argv-marker".into()],
        clear_env: true,
        allowed_env: vec![],
        env: vec![("PRIVATE_ENV_MARKER".into(), "private-env-marker".into())],
    }
}

fn assert_order(log: &str, events: &[&str]) {
    let mut tail = log;
    for event in events {
        let index = tail
            .find(event)
            .unwrap_or_else(|| panic!("missing diagnostic {event}"));
        tail = &tail[index + event.len()..];
    }
}

fn assert_private(log: &str) {
    for value in [
        "private-argv-marker",
        "PRIVATE_ENV_MARKER",
        "private-env-marker",
        "private-session-marker",
        "private-prompt-marker",
        "private-stderr-marker",
        "private-backend-marker",
        "private-id-marker",
        "private-json-marker",
        "/private/cwd-marker",
        "你🙂",
        "private-reply-marker",
    ] {
        assert!(!log.contains(value));
    }
}

const INITIALIZE: &str = "read -r request; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{\"loadSession\":true},\"agentInfo\":{\"name\":\"private-backend-marker\",\"version\":\"1\"}}}'";
const NEW_SESSION: &str = "read -r request; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"sessionId\":\"private-session-marker\"}}'";

#[tokio::test]
async fn public_acp_stdio_fragmented_unicode_and_watchdog_phase_order() {
    let capture = DiagnosticCapture::default();
    let dispatch = tracing::Dispatch::new(capture.clone());
    async {
        let script = format!(r#"
{INITIALIZE}; {NEW_SESSION};
read -r request;
printf '%s\n' 'private-stderr-marker' >&2;
printf '%s' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"private-session-marker","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"';
printf '\344'; sleep 0.02; printf '\275\240\360'; sleep 0.02; printf '\237\231\202';
printf '%s\n' '"}}}}}}}}';
read -r cancel;
case "$cancel" in *'session/cancel'*) ;; *) exit 7 ;; esac;
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"stopReason":"cancelled"}}}}';
read -r continuation;
case "$continuation" in *'session/prompt'*) ;; *) exit 8 ;; esac;
printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"private-session-marker","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"private-reply-marker"}}}}}}}}';
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}';
"#);
        let mut client = Client::connect_with_options(TransportConfig::Stdio(peer(&script)), AgentWatchdogOptions {
            response_timeout_seconds: 1,
            cancel_timeout_seconds: 1,
            recovery_operation_timeout_seconds: 3,
        }).await.unwrap();
        let session = client.new_session("/private/cwd-marker", &[], None).await.unwrap();
        assert_private(&capture.text());
        capture.clear();
        let mut texts = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(5), client.prompt(&session, vec![PromptContent::text("private-prompt-marker")], PromptTurnCancellation::disabled(), &mut |event| {
            if let Event::MessageChunk { content } = event {
                texts.push(content["text"].as_str().unwrap().to_string());
            }
        })).await.unwrap().unwrap();
        assert!(matches!(result, StopReason::EndTurn));
        assert_eq!(texts, ["你🙂", "private-reply-marker"]);
        client.close().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !capture.text().contains("acp_stderr_eof") {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let log = capture.text();
        assert_order(&log, &["acp_request_send_started", "acp_request_serialized", "acp_stdio_write_started", "acp_stdio_write_complete", "acp_stdio_flush_started", "acp_stdio_flush_complete", "acp_request_send_complete", "agent_watchdog_armed", "acp_stdout_frame_complete", "acp_json_parse_complete", "agent_watchdog_reset", "agent_watchdog_cancel_begin", "acp_stdio_flush_complete", "agent_watchdog_cancel_complete", "agent_watchdog_soft_recovered"]);
        assert_eq!(log.matches("agent_watchdog_armed").count(), 1);
        assert_eq!(log.matches("agent_watchdog_reset").count(), 3);
        assert!(!log.contains("acp_stdout_frame_failed"));
        assert_eq!(log.matches("acp_stderr_first_read").count(), 1);
        assert!(log.contains("acp_stderr_eof"));
        assert!(log.contains("bytes=22"));
        assert_private(&log);
    }.with_subscriber(dispatch).await;
}

#[tokio::test]
async fn public_acp_stdio_classifies_invalid_utf8_json_and_eof_without_payloads() {
    for (output, event) in [
        (
            "printf 'private-json-marker\\377\\n'",
            "acp_stdout_frame_failed",
        ),
        ("printf 'private-json-marker\\n'", "acp_json_parse_failed"),
        ("printf ''", "acp_stdout_eof"),
    ] {
        let capture = DiagnosticCapture::default();
        let dispatch = tracing::Dispatch::new(capture.clone());
        async {
            let script = format!("read -r request; {output}");
            let error = Client::connect(TransportConfig::Stdio(peer(&script)))
                .await
                .unwrap_err();
            assert!(!error.to_string().contains("private-json-marker"));
            let log = capture.text();
            assert_order(
                &log,
                &[
                    "acp_request_send_started",
                    "acp_stdio_flush_complete",
                    event,
                ],
            );
            if event == "acp_stdout_frame_failed" {
                assert!(log.contains("invalid_utf8"));
                assert!(log.contains("bytes=21"));
                assert!(!log.contains("acp_json_parse_complete"));
            }
            assert_private(&log);
        }
        .with_subscriber(dispatch)
        .await;
    }
}

#[tokio::test]
async fn stdio_partial_frame_survives_cancelled_read_and_reports_exact_counts() {
    let capture = DiagnosticCapture::default();
    let dispatch = tracing::Dispatch::new(capture.clone());
    async {
        let mut transport = StdioTransport::connect(
            &peer(r"printf '\344'; read -r release; printf '\275\240\n'; printf tail"),
            &[],
        )
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), transport.recv())
                .await
                .is_err()
        );
        assert!(capture.text().contains("acp_stdout_first_read"));
        assert!(!capture.text().contains("acp_stdout_frame_complete"));
        transport.send("release").await.unwrap();
        assert_eq!(transport.recv().await.unwrap().as_deref(), Some("你"));
        assert_eq!(transport.recv().await.unwrap().as_deref(), Some("tail"));
        assert_eq!(transport.recv().await.unwrap(), None);
        let log = capture.text();
        assert_eq!(log.matches("acp_stdout_first_read").count(), 1);
        assert!(log.contains("stdout_bytes=8"));
        assert!(log.contains("frames=2"));
        transport.close().await.unwrap();
        assert_private(&log);
    }
    .with_subscriber(dispatch)
    .await;
}

#[tokio::test]
async fn stdio_stuck_write_and_closed_stdin_expose_only_boundaries() {
    let capture = DiagnosticCapture::default();
    let dispatch = tracing::Dispatch::new(capture.clone());
    async {
        let mut transport = StdioTransport::connect(&peer("printf 'ready\\n'; sleep 30"), &[])
            .await
            .unwrap();
        assert_eq!(transport.recv().await.unwrap().as_deref(), Some("ready"));
        capture.clear();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                transport.send(&"x".repeat(2 * 1024 * 1024))
            )
            .await
            .is_err()
        );
        let log = capture.text();
        assert!(log.contains("acp_stdio_write_started"));
        assert!(!log.contains("acp_stdio_write_complete"));
        assert!(!log.contains("acp_stdio_flush_complete"));
        transport.force_terminate().await.unwrap();
        let mut transport =
            StdioTransport::connect(&peer("exec 0<&-; printf 'ready\\n'; sleep 30"), &[])
                .await
                .unwrap();
        assert_eq!(transport.recv().await.unwrap().as_deref(), Some("ready"));
        capture.clear();
        assert!(transport.send("private-prompt-marker").await.is_err());
        let log = capture.text();
        assert_order(
            &log,
            &[
                "acp_stdio_write_started",
                "acp_stdio_write_complete",
                "acp_stdio_flush_started",
                "acp_stdio_flush_failed",
            ],
        );
        assert!(log.contains("completed_bytes=22"));
        assert!(!log.contains("acp_stdio_flush_complete"));
        assert_private(&log);
        transport.force_terminate().await.unwrap();
    }
    .with_subscriber(dispatch)
    .await;
}
