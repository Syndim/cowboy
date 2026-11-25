use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};

fn write_message(value: &Value) {
    let body = value.to_string();
    let mut output = io::stdout().lock();
    write!(output, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    output.flush().unwrap();
}

fn event(session: &str, kind: &str, data: Value) {
    write_message(
        &json!({"jsonrpc":"2.0", "method":"session.event", "params":{"sessionId":session,"event":{"id":"event", "timestamp":"2026-01-01T00:00:00Z", "parentId":null, "type":kind, "data":data}}}),
    );
}

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut last_session = String::new();
    let mut last_prompt = String::new();
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).unwrap() == 0 {
            break;
        }
        let Some(length) = line
            .trim()
            .strip_prefix("Content-Length: ")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            break;
        };
        line.clear();
        input.read_line(&mut line).unwrap();
        let mut body = vec![0; length];
        input.read_exact(&mut body).unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        if request["method"].is_null() {
            if request["id"] == 991 {
                assert_eq!(request["result"]["approved"], false);
                event(&last_session, "session.idle", json!({}));
            }
            continue;
        }
        let method = request["method"].as_str().unwrap_or("");
        let session = request["params"]["sessionId"].as_str().unwrap_or("fixture");
        if method == "session.create" || method == "session.resume" {
            let params = &request["params"];
            assert_eq!(params["events"]["streaming"], true);
            assert_eq!(params["permissions"]["request"], true);
            assert_eq!(params["permissions"]["mode"], "manual");
            assert_eq!(params["interaction"]["requestExitPlanMode"], true);
            assert_eq!(params["enableConfigDiscovery"], false);
            assert_eq!(
                params["workingDirectory"],
                std::env::current_dir().unwrap().to_str().unwrap()
            );
            assert!(std::env::var_os("PATH").is_none());
            if method == "session.resume" {
                assert_eq!(params["continuePendingWork"], false);
                assert_eq!(params["suppressResumeEvent"], true);
            }
            if method == "session.create" {
                assert!(matches!(
                    params["model"].as_str(),
                    Some("test-model" | "refused-model" | "missing-mode-model")
                ));
            }
        }
        if method == "session.resume" && session == "missing-session" {
            write_message(
                &json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"not found"}}),
            );
            continue;
        }
        if method == "session.detach" && session == "close-hang" {
            continue;
        }
        if method == "session.resume" && session == "startup-events" {
            event(
                session,
                "assistant.message_delta",
                json!({"messageId":"startup", "deltaContent":"startup"}),
            );
        }
        if method == "session.send" && request["params"]["prompt"] == "rpc-error" {
            write_message(
                &json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"rpc-secret-canary"}}),
            );
            continue;
        }
        if method == "session.send" {
            last_prompt = request["params"]["prompt"]
                .as_str()
                .unwrap_or("")
                .to_owned();
        }
        if method == "session.send" && last_prompt == "completed-before-ack" {
            event(session, "user.message", json!({"messageId":"user-1"}));
            event(
                session,
                "assistant.message_delta",
                json!({"messageId":"ready", "deltaContent":"ready"}),
            );
            event(session, "session.idle", json!({}));
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        if method == "session.send"
            && matches!(
                last_prompt.as_str(),
                "delayed-ack" | "cancel-budget" | "no-ack"
            )
        {
            event(session, "user.message", json!({"messageId":"user-1"}));
            event(
                session,
                "assistant.message_delta",
                json!({"messageId":"ready", "deltaContent":"ready"}),
            );
            std::thread::sleep(std::time::Duration::from_millis(
                if last_prompt == "delayed-ack" {
                    300
                } else {
                    700
                },
            ));
            if last_prompt == "no-ack" {
                continue;
            }
        }
        if method == "session.send" && last_prompt == "origins" {
            event(session, "user.message", json!({"messageId":"user-1"}));
            event(
                session,
                "assistant.message",
                json!({"messageId":"old", "originatingMessageId":"unrelated", "content":"pre-old"}),
            );
            event(
                session,
                "assistant.message",
                json!({"messageId":"current", "originatingMessageId":"user-1", "content":"pre-current"}),
            );
            event(
                session,
                "assistant.message_delta",
                json!({"messageId":"old-stream", "originatingMessageId":"unrelated", "deltaContent":"pre-old-delta"}),
            );
            event(
                session,
                "assistant.message_delta",
                json!({"messageId":"current-stream", "originatingMessageId":"user-1", "deltaContent":"pre-delta"}),
            );
            event(
                session,
                "assistant.message",
                json!({"messageId":"current-stream", "content":"pre-delta"}),
            );
            event(
                session,
                "assistant.message_delta",
                json!({"messageId":"stream", "deltaContent":"live"}),
            );
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
        if method == "session.abort"
            && matches!(
                last_prompt.as_str(),
                "delayed-ack" | "cancel-budget" | "delayed-abort"
            )
        {
            for _ in 0..100 {
                event(
                    session,
                    "assistant.message_delta",
                    json!({"messageId":"abort-stream", "deltaContent":"x"}),
                );
            }
            event(session, "session.idle", json!({"aborted":true}));
            std::thread::sleep(std::time::Duration::from_millis(
                if last_prompt == "cancel-budget" {
                    500
                } else {
                    700
                },
            ));
        }
        if method == "session.send" && request["params"]["prompt"] == "pre-ack" {
            event(session, "user.message", json!({"messageId":"user-1"}));
            event(
                session,
                "assistant.message",
                json!({"messageId":"other", "originatingMessageId":"unrelated"}),
            );
            event(session, "session.idle", json!({}));
        }
        let result = match method {
            "connect" => json!({"ok":true,"protocolVersion":4,"version":"fake"}),
            "session.create" | "session.resume" => {
                let refused = session == "refused" || request["params"]["model"] == "refused-model";
                let missing =
                    session == "missing-mode" || request["params"]["model"] == "missing-mode-model";
                let mode = if refused { "allow-all" } else { "manual" };
                let result = if missing {
                    Value::Null
                } else {
                    json!({"success":!refused, "mode":mode})
                };
                json!({"sessionId":session,"workspacePath":null,"capabilities":null,"permissionModeResult":result})
            }
            "session.send" => json!({"messageId":"user-1"}),
            "session.detach" => json!({"success":true}),
            _ => json!({}),
        };
        if !request["id"].is_null() {
            write_message(&json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
        }
        if method == "session.send" {
            last_session = session.to_owned();
            let prompt = request["params"]["prompt"].as_str().unwrap_or("");
            if matches!(
                prompt,
                "pre-ack" | "delayed-ack" | "cancel-budget" | "completed-before-ack"
            ) {
                continue;
            }
            if prompt == "origins" {
                event(
                    session,
                    "assistant.message",
                    json!({"messageId":"old-after", "originatingMessageId":"unrelated", "content":"post-old"}),
                );
                event(
                    session,
                    "assistant.message_delta",
                    json!({"messageId":"old-delta", "originatingMessageId":"unrelated", "deltaContent":"old-delta"}),
                );
                event(
                    session,
                    "assistant.message",
                    json!({"messageId":"stream", "originatingMessageId":"user-1", "content":"live"}),
                );
                event(
                    session,
                    "assistant.message",
                    json!({"messageId":"current-after", "originatingMessageId":"user-1", "content":"post-current"}),
                );
                event(session, "session.idle", json!({}));
                continue;
            }
            if prompt == "policy" {
                event(
                    session,
                    "session.warning",
                    json!({"warningType":"policy_blocked","originatingMessageId":"user-1"}),
                );
                event(session, "session.idle", json!({}));
                continue;
            }
            if prompt == "empty-root" {
                write_message(
                    &json!({"jsonrpc":"2.0", "method":"session.event", "params":{"sessionId":session,"event":{"id":"event", "timestamp":"2026-01-01T00:00:00Z", "parentId":null, "agentId":"", "type":"assistant.message", "data":{"originatingMessageId":"user-1","content":"root"}}}}),
                );
                event(session, "session.idle", json!({}));
                continue;
            }
            if prompt == "stale-idle" {
                event(session, "session.idle", json!({}));
            }
            event(
                session,
                "user.message",
                json!({"messageId":"user-1", "content":"request"}),
            );
            if prompt == "exit-plan" {
                write_message(
                    &json!({"jsonrpc":"2.0","id":991,"method":"exitPlanMode.request","params":{"sessionId":session}}),
                );
                continue;
            }
            if prompt == "tool-finished-stall" {
                event(
                    session,
                    "tool.execution_start",
                    json!({"toolCallId":"slow", "toolName":"read"}),
                );
                event(
                    session,
                    "tool.execution_complete",
                    json!({"toolCallId":"slow", "success":true}),
                );
                continue;
            }
            if prompt == "slow-tool" {
                event(
                    session,
                    "tool.execution_start",
                    json!({"toolCallId":"slow", "toolName":"read"}),
                );
                std::thread::sleep(std::time::Duration::from_millis(1500));
                event(
                    session,
                    "tool.execution_complete",
                    json!({"toolCallId":"slow", "success":true}),
                );
                event(session, "session.idle", json!({}));
                continue;
            }
            if prompt == "permission" {
                event(
                    session,
                    "permission.requested",
                    json!({"requestId":"permission-1","sessionId":session,"permissionRequest":{"kind":"shell"}}),
                );
                continue;
            }
            if prompt == "autopilot" {
                event(session, "session.idle", json!({"mode":"autopilot"}));
                event(
                    session,
                    "assistant.message_delta",
                    json!({"messageId":"assistant-1","deltaContent":"after idle"}),
                );
                event(
                    session,
                    "session.autopilot_stopped",
                    json!({"requestId":"user-1","reason":"task_complete"}),
                );
                continue;
            }
            if prompt == "error" {
                event(
                    session,
                    "session.error",
                    json!({"errorType":"query","message":"private secret"}),
                );
                continue;
            }
            if prompt != "no-delta" {
                event(
                    session,
                    "assistant.message_delta",
                    json!({"messageId":"assistant-1","deltaContent":"hello"}),
                );
            }
            event(
                session,
                "assistant.message",
                json!({"messageId":"assistant-1", "content":"hello"}),
            );
            if prompt == "wait" || prompt == "delayed-abort" {
                continue;
            }
            event(
                session,
                "tool.execution_start",
                json!({"toolCallId":"tool-1","toolName":"read"}),
            );
            event(
                session,
                "tool.execution_progress",
                json!({"toolCallId":"tool-1", "progressMessage":"reading"}),
            );
            event(
                session,
                "tool.execution_complete",
                json!({"toolCallId":"tool-1","success":true}),
            );
            event(session, "assistant.turn_end", json!({"turnId":"turn-1"}));
            event(session, "session.idle", json!({}));
        }
        if method == "session.permissions.handlePendingPermissionRequest" {
            assert_eq!(request["params"]["result"]["kind"], "reject");
            event(session, "session.idle", json!({}));
        }
        if method == "session.abort"
            && !matches!(
                last_prompt.as_str(),
                "delayed-ack" | "cancel-budget" | "delayed-abort"
            )
        {
            event(session, "session.idle", json!({"aborted":true}));
        }
        if method == "runtime.shutdown" {
            break;
        }
    }
}
