#![cfg(unix)]

#[path = "support/diagnostics.rs"]
mod diagnostics;

use cowboy_agent_acp::transport::stdio::StdioTransport;
use cowboy_agent_acp::transport::{StdioConfig, Transport};
use cowboy_agent_acp::{AgentWatchdogOptions, Client, TransportConfig};
use cowboy_agent_client::{Event, ModelInfo, PromptContent, PromptTurnCancellation, StopReason};
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
        "private-prompt-marker",
        "private-stderr-marker",
        "private-json-marker",
        "/private/cwd-marker",
        "你🙂",
        "private-reply-marker",
        "private-message-marker",
        "private-token-marker",
        "private-password-marker",
        "private-body-marker",
        "private-content-marker",
        "private-data-marker",
        "private-arguments-marker",
    ] {
        assert!(!log.contains(value));
    }
}

const INITIALIZE: &str = "read -r request; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{\"loadSession\":true},\"agentInfo\":{\"name\":\"diagnostic-backend-marker\",\"version\":\"1\"}}}'";
const NEW_SESSION: &str = "read -r request; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"sessionId\":\"diagnostic-session-marker\"}}'";

#[tokio::test]
async fn public_acp_stdio_fragmented_unicode_and_watchdog_phase_order() {
    if diagnostics::isolated_capture_test(
        "public_acp_stdio_fragmented_unicode_and_watchdog_phase_order",
    )
    .await
    {
        return;
    }
    let capture = DiagnosticCapture::at_level(tracing::Level::DEBUG);
    let dispatch = tracing::Dispatch::new(capture.clone());
    async {
        let script = format!(r#"
{INITIALIZE}; {NEW_SESSION};
read -r request;
printf '%s\n' 'private-stderr-marker' >&2;
printf '%s' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"diagnostic-session-marker","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"';
printf '\344'; sleep 0.02; printf '\275\240\360'; sleep 0.02; printf '\237\231\202';
printf '%s\n' '"}}}}}}}}';
read -r cancel;
case "$cancel" in *'session/cancel'*) ;; *) exit 7 ;; esac;
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"stopReason":"cancelled"}}}}';
read -r continuation;
case "$continuation" in *'session/prompt'*) ;; *) exit 8 ;; esac;
printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"diagnostic-session-marker","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"private-reply-marker"}}}}}}}}';
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}';
"#);
        let mut client = Client::connect_with_options(TransportConfig::Stdio(peer(&script)), AgentWatchdogOptions {
            response_timeout_seconds: 1,
            cancel_timeout_seconds: 1,
            recovery_operation_timeout_seconds: 3,
        }).await.unwrap();
        let model = ModelInfo {
            id: "diagnostic-model-marker".into(),
            provider: Some("diagnostic-provider-marker".into()),
        };
        let session = client.new_session("/private/cwd-marker", &[], Some(&model)).await.unwrap();
        let log = capture.text();
        for metadata in ["diagnostic-session-marker", "diagnostic-backend-marker", "backend_version=Some(\"1\")", "diagnostic-model-marker", "diagnostic-provider-marker", "load_session_supported=true"] {
            assert!(log.lines().any(|line| line.starts_with("INFO") && line.contains(metadata)), "missing INFO metadata {metadata}");
        }
        assert_private(&log);
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
        assert!(log.lines().any(|line| line.starts_with("WARN") && line.contains("Agent subprocess stderr") && line.contains("command=sh") && line.contains("pid=")));
        for event in ["ACP prompt sent", "agent_watchdog_armed", "agent_watchdog_cancel_begin", "agent_watchdog_cancel_complete", "ACP prompt turn completed", "ACP client closed"] {
            assert!(log.lines().any(|line| line.starts_with("DEBUG") && line.contains(event) && line.contains("diagnostic-session-marker")), "missing session correlation for {event}");
        }
        assert!(log.lines().any(|line| line.starts_with("DEBUG") && line.contains("update=\"message_chunk\"")));
        assert_private(&log);
    }.with_subscriber(dispatch).await;
}

#[tokio::test]
async fn public_acp_stdio_classifies_invalid_utf8_json_and_eof_without_payloads() {
    if diagnostics::isolated_capture_test(
        "public_acp_stdio_classifies_invalid_utf8_json_and_eof_without_payloads",
    )
    .await
    {
        return;
    }
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
            if event == "acp_json_parse_failed" {
                assert!(
                    error
                        .to_string()
                        .contains("expected value at line 1 column 1")
                );
            }
            let log = capture.text();
            assert!(log.lines().any(|line| line.starts_with("DEBUG")
                && line.contains("acp_stdio_spawned")
                && line.contains("command=sh")
                && line.contains("pid=")));
            assert_order(
                &log,
                &[
                    "acp_request_send_started",
                    "acp_stdio_flush_complete",
                    event,
                ],
            );
            if event == "acp_json_parse_failed" {
                assert!(log.contains("expected value"));
                assert!(log.contains("line=1"));
                assert!(log.contains("column=1"));
            }
            if event == "acp_stdout_frame_failed" {
                assert!(log.contains("invalid_utf8"));
                assert!(log.contains("bytes=21"));
                assert!(log.lines().any(|line| line.starts_with("WARN")
                    && line.contains("ACP frame is not valid UTF-8")
                    && line.contains("command=sh")
                    && line.contains("pid=")));
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
    if diagnostics::isolated_capture_test(
        "stdio_partial_frame_survives_cancelled_read_and_reports_exact_counts",
    )
    .await
    {
        return;
    }
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
        for event in [
            "acp_stdout_first_read",
            "acp_stdout_frame_complete",
            "acp_stdout_eof",
            "acp_stdio_write_started",
            "acp_stdio_write_complete",
            "acp_stdio_flush_started",
            "acp_stdio_flush_complete",
        ] {
            assert!(
                log.lines().any(|line| line.starts_with("DEBUG")
                    && line.contains(event)
                    && line.contains("command=sh")
                    && line.contains("pid=")),
                "missing command correlation for {event}"
            );
        }
        transport.close().await.unwrap();
        assert!(
            capture
                .text()
                .lines()
                .any(|line| line.contains("Closing agent subprocess")
                    && line.contains("command=sh")
                    && line.contains("pid="))
        );
        assert_private(&log);
    }
    .with_subscriber(dispatch)
    .await;
}

#[tokio::test]
async fn stdio_stuck_write_and_closed_stdin_preserve_io_explanations() {
    if diagnostics::isolated_capture_test(
        "stdio_stuck_write_and_closed_stdin_preserve_io_explanations",
    )
    .await
    {
        return;
    }
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
        assert!(capture.text().lines().any(|line| line.starts_with("WARN")
            && line.contains("Force terminating agent subprocess")
            && line.contains("command=sh")
            && line.contains("pid=")));
        capture.clear();
        assert!(
            transport
                .send(&"private-prompt-marker".repeat(8192))
                .await
                .unwrap_err()
                .to_string()
                .contains("Broken pipe")
        );
        let log = capture.text();
        assert!(log.lines().any(|line| line.starts_with("DEBUG")
            && line.contains("acp_stdio_write_failed")
            && line.contains("Broken pipe")
            && line.contains("command=sh")
            && line.contains("pid=")));
        assert!(log.lines().any(|line| line.starts_with("WARN")
            && line.contains("acp_stdio_send_failed")
            && line.contains("boundary=\"write_all\"")
            && line.contains("Broken pipe")
            && line.contains("command=sh")
            && line.contains("pid=")));
        assert_private(&log);
        let mut transport =
            StdioTransport::connect(&peer("exec 0<&-; printf 'ready\\n'; sleep 30"), &[])
                .await
                .unwrap();
        assert_eq!(transport.recv().await.unwrap().as_deref(), Some("ready"));
        capture.clear();
        let error = transport.send("private-prompt-marker").await.unwrap_err();
        assert!(error.to_string().contains("Broken pipe"));
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
        assert!(log.lines().any(|line| line.starts_with("DEBUG")
            && line.contains("acp_stdio_flush_failed")
            && line.contains("Broken pipe")
            && line.contains("command=sh")
            && line.contains("pid=")));
        assert!(log.lines().any(|line| line.starts_with("WARN")
            && line.contains("acp_stdio_send_failed")
            && line.contains("Broken pipe")
            && line.contains("command=sh")
            && line.contains("pid=")));
        assert!(!log.contains("acp_stdio_flush_complete"));
        assert_private(&log);
        transport.force_terminate().await.unwrap();
    }
    .with_subscriber(dispatch)
    .await;
}

#[tokio::test]
async fn stdio_stderr_warnings_preserve_bounded_errors_without_private_fields() {
    if diagnostics::isolated_capture_test(
        "stdio_stderr_warnings_preserve_bounded_errors_without_private_fields",
    )
    .await
    {
        return;
    }
    let script = r#"
printf '\n\r\n' >&2;
printf 'ACP frame is not valid UTF-8\n' >&2;
printf 'session not ' >&2;
sleep 0.02;
printf 'found\r\n' >&2;
printf 'Error: session not found; token=private-token-marker; Authorization: Bearer private-token-marker; password=\047private-password-marker\047; prompt="private-prompt-marker"; body=private-body-marker with details; content={"text":"private-content-marker"}; arguments=["private-arguments-marker"]; retry later\n' >&2;
printf '%s\n' '{"error":{"code":-32001,"message":"session not found; token=private-token-marker; prompt=private-prompt-marker","data":{"message":"private-message-marker","raw":"private-data-marker"}},"body":"private-body-marker"}' >&2;
printf '%s\r\n' '{"level":"error","code":-32002,"message":"Broken pipe","data":"private-data-marker","prompt":"private-prompt-marker"}' >&2;
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"content":"private-content-marker"},"message":"private-message-marker"}' >&2;
printf '%s\n' '{"jsonrpc":"2.0","id":7,"error":{"code":-32004,"message":"private-message-marker","data":"private-data-marker"}}' >&2;
printf '%s\n' 'Error: {"error":{"code":-32005,"message":"session not found"},"data":"private-data-marker"' >&2;
printf '%s\n' '{"level":"info","message":"private-message-marker","data":"private-data-marker"}' >&2;
printf '%s\n' '{"error":{"code":-32003,"message":"session not found"},"data":"private-data-marker"' >&2;
printf 'Error: session not found; prompt="\344' >&2;
sleep 0.02;
printf '\275\240\360' >&2;
sleep 0.02;
printf '\237\231\202"; retry later\r\n' >&2;
printf '\377\n' >&2;
printf '%s\n' 'private-stderr-marker' >&2;
printf 'database is locked\n' >&2;
printf 'connection refused while connecting to backend\n' >&2;
printf '%s\n' '[ERROR] connection refused' >&2;
printf '%s\n' '[backend] database is locked' >&2;
printf '%s\n' 'ERROR received: {"jsonrpc":"2.0","id":7,"result":{"text":"private-body-marker"}}' >&2;
printf '%s\n' 'ERROR tool failed: ["private-content-marker",{"text":"private-body-marker"}]' >&2;
printf '%s\n' 'ERROR received malformed: {"jsonrpc":"2.0","result":{"text":"private-body-marker"' >&2;
printf '%s\n' '{"level":"error","code":-32006,"message":"received structured: {\"jsonrpc\":\"2.0\",\"result\":{\"text\":\"private-body-marker\"}}"}' >&2;
printf '%s\n' '{"error":{"code":-32007,"message":"structured tool failed: [\"private-content-marker\"]"}}' >&2;
printf 'Broken pipe (os error 32)' >&2;
"#;
    for level in [
        tracing::Level::INFO,
        tracing::Level::WARN,
        tracing::Level::DEBUG,
    ] {
        let capture = DiagnosticCapture::at_level(level);
        let dispatch = tracing::Dispatch::new(capture.clone());
        async {
            let mut transport = StdioTransport::connect(&peer(script), &[]).await.unwrap();
            transport.child().wait().await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !capture.text().lines().any(|line| {
                    line.starts_with("WARN") && line.contains("Broken pipe (os error 32)")
                }) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            if level == tracing::Level::DEBUG {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !capture.text().contains("acp_stderr_eof") {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            transport.close().await.unwrap();
            let log = capture.text();
            let warnings: Vec<_> = log
                .lines()
                .filter(|line| line.contains("acp_stderr_diagnostic"))
                .collect();
            assert_eq!(warnings.len(), 17);
            assert!(warnings.iter().all(|line| line.starts_with("WARN")
                && line.contains("command=sh")
                && line.contains("pid=")));
            for error in [
                "ACP frame is not valid UTF-8",
                "session not found",
                "Broken pipe",
                "retry later",
                "rpc_error_code=-32001",
                "rpc_error_code=-32002",
                "rpc_error_code=-32006",
                "rpc_error_code=-32007",
                "received structured:",
                "structured tool failed:",
                "database is locked",
                "connection refused while connecting to backend",
                "received:",
                "tool failed:",
                "received malformed:",
                "logger=\"ERROR\"",
                "logger=\"backend\"",
            ] {
                assert!(
                    warnings.iter().any(|line| line.contains(error)),
                    "missing WARN error {error}"
                );
            }
            for code in [-32003, -32004, -32005] {
                assert!(!log.contains(&format!("rpc_error_code={code}")));
            }
            assert_eq!(
                warnings
                    .iter()
                    .filter(|line| line.contains("retry later"))
                    .count(),
                2
            );
            if level == tracing::Level::DEBUG {
                assert_eq!(log.matches("acp_stderr_first_read").count(), 1);
                assert_eq!(log.matches("acp_stderr_eof").count(), 1);
                for event in ["acp_stderr_first_read", "acp_stderr_eof"] {
                    assert!(log.lines().any(|line| line.starts_with("DEBUG")
                        && line.contains(event)
                        && line.contains("command=sh")
                        && line.contains("pid=")
                        && line.contains("bytes=")));
                }
            } else {
                assert!(!log.contains("DEBUG"));
            }
            assert_private(&log);
        }
        .with_subscriber(dispatch)
        .await;
    }
}

#[tokio::test]
async fn stdio_stderr_oversized_lines_do_not_stop_drain_or_expose_fragments() {
    if diagnostics::isolated_capture_test(
        "stdio_stderr_oversized_lines_do_not_stop_drain_or_expose_fragments",
    )
    .await
    {
        return;
    }
    let capture = DiagnosticCapture::at_level(tracing::Level::DEBUG);
    let dispatch = tracing::Dispatch::new(capture.clone());
    async {
        let script = r#"
printf 'Error: session not found; prompt="' >&2;
i=0; while [ "$i" -lt 4096 ]; do printf '%s' 'private-prompt-marker' >&2; i=$((i + 1)); done;
printf '"; private-stderr-marker\n' >&2;
printf 'session not found\n' >&2;
printf 'Error: session not found; prompt="' >&2;
i=0; while [ "$i" -lt 4096 ]; do printf '%s' 'private-prompt-marker' >&2; i=$((i + 1)); done;
"#;
        let expected_bytes = 2
            * ("Error: session not found; prompt=\"".len() + 4096 * "private-prompt-marker".len())
            + "\"; private-stderr-marker\n".len()
            + "session not found\n".len();
        let mut transport = StdioTransport::connect(&peer(script), &[]).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), transport.child().wait())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !capture.text().contains("acp_stderr_eof") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        transport.close().await.unwrap();
        let log = capture.text();
        assert_eq!(log.matches("acp_stderr_first_read").count(), 1);
        assert_eq!(log.matches("acp_stderr_diagnostic").count(), 1);
        assert!(log.lines().any(|line| line.starts_with("WARN")
            && line.contains("session not found")
            && line.contains("command=sh")));
        assert!(log.lines().any(|line| line.contains("acp_stderr_eof")
            && line.contains(&format!("bytes={expected_bytes}"))));
        assert_private(&log);
    }
    .with_subscriber(dispatch)
    .await;
}

#[tokio::test]
async fn stdio_normal_io_errors_remain_visible_above_debug() {
    if diagnostics::isolated_capture_test("stdio_normal_io_errors_remain_visible_above_debug").await
    {
        return;
    }
    for level in [tracing::Level::INFO, tracing::Level::WARN] {
        let capture = DiagnosticCapture::at_level(level);
        let dispatch = tracing::Dispatch::new(capture.clone());
        async {
            let mut config = peer("printf 'private-json-marker\\377\\n'");
            config.command = "/bin/sh".into();
            let mut transport = StdioTransport::connect(&config, &[]).await.unwrap();
            assert!(
                transport
                    .recv()
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("UTF-8")
            );
            transport.close().await.unwrap();
            let mut transport =
                StdioTransport::connect(&peer("exec 0<&-; printf 'ready\\n'; sleep 30"), &[])
                    .await
                    .unwrap();
            assert_eq!(transport.recv().await.unwrap().as_deref(), Some("ready"));
            assert!(
                transport
                    .send("private-prompt-marker")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("Broken pipe")
            );
            let log = capture.text();
            for error in ["ACP frame is not valid UTF-8", "Broken pipe"] {
                assert!(log.lines().any(|line| line.starts_with("WARN")
                    && line.contains(error)
                    && line.contains("command=sh")
                    && line.contains("pid=")));
            }
            assert!(!log.contains("DEBUG"));
            assert!(!log.contains("/bin/sh"));
            assert_private(&log);
            transport.force_terminate().await.unwrap();
        }
        .with_subscriber(dispatch)
        .await;
    }
}
