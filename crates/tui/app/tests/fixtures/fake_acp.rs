use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

fn send(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}

fn record(path: &Path, value: Value) {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(format!("{value}\n").as_bytes())
        .unwrap();
}

fn count(path: &Path, method: &str) -> usize {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["method"] == method)
        .count()
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "--descendant") {
        let heartbeat = Path::new(args.get(2).expect("heartbeat path"));
        for _ in 0..6000 {
            write!(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(heartbeat)
                    .unwrap(),
                "x"
            )
            .unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }

        return;
    }

    let scenario = args.get(1).expect("scenario");
    let log = Path::new(args.get(2).expect("log path"));
    record(log, json!({"method": "spawn", "pid": std::process::id()}));
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            continue;
        };
        let id = &request["id"];
        record(
            log,
            json!({"method": method, "params": request["params"], "pid": std::process::id()}),
        );
        if id.is_null() {
            continue;
        }

        let attempt = count(log, method);
        match method {
            "initialize" if scenario == "initialize_error" && attempt == 1 => {
                send(
                    json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32000,"message":"secret init error"}}),
                );
            }
            "initialize" => send(json!({"jsonrpc":"2.0", "id":id, "result":{
                "protocolVersion":1,
                "agentCapabilities":{"loadSession": (scenario != "load_unsupported_after_wait" || attempt == 1) && (scenario != "provided_load_unsupported" || attempt > 1)},
                "agentInfo":{"name":"fake", "version":"1"}
            }})),
            "session/new" if scenario == "new_error" && attempt == 1 => {
                send(
                    json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32000,"message":"secret new error"}}),
                );
            }
            "session/new" if scenario == "prompt_send_failure" && attempt == 1 => {
                send(json!({"jsonrpc":"2.0", "id":id, "result":{"sessionId":"session-1"}}));
                std::process::exit(22);
            }
            "session/new" => {
                send(json!({"jsonrpc":"2.0", "id":id, "result":{"sessionId":"session-1"}}))
            }
            "session/load"
                if (scenario == "load_error" || scenario == "provided_load_error")
                    && attempt == 1 =>
            {
                send(
                    json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32000,"message":"secret load error"}}),
                );
            }
            "session/load" => send(json!({"jsonrpc":"2.0", "id":id, "result":{}})),
            "session/prompt"
                if scenario == "prompt_crash"
                    && !request["params"]["prompt"]
                        .to_string()
                        .contains("## Human context") =>
            {
                if attempt == 1 {
                    // Deliberately exit without reaping: Cowboy must kill the
                    // inherited pipe writer before publishing a human wait.
                    #[allow(clippy::zombie_processes)]
                    let child = Command::new(std::env::current_exe().unwrap())
                        .arg("--descendant")
                        .arg(log.with_extension("heartbeat"))
                        .stdin(Stdio::null())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::null())
                        .spawn()
                        .unwrap();
                    record(log, json!({"method":"descendant", "pid":child.id()}));
                }

                std::process::exit(24);
            }
            "session/prompt"
                if scenario == "prompt_timeout"
                    && !request["params"]["prompt"]
                        .to_string()
                        .contains("## Human context") =>
            {
                std::thread::sleep(Duration::from_secs(60));
            }
            "session/prompt"
                if scenario == "prompt_error"
                    && !request["params"]["prompt"]
                        .to_string()
                        .contains("## Human context") =>
            {
                send(
                    json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32000,"message":"secret prompt error"}}),
                );
            }
            "session/prompt" if scenario == "second_step_error" && attempt == 2 => {
                send(json!({"jsonrpc":"2.0", "id":id,
                    "error":{"code":-32000,"message":"second-step transport failed"}}));
            }
            "session/prompt" if scenario == "hold_human" && attempt == 2 => {
                let heartbeat = log.with_extension("heartbeat");
                for _ in 0..6000 {
                    if log.with_extension("stop").exists() {
                        break;
                    }

                    write!(
                        OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&heartbeat)
                            .unwrap(),
                        "x"
                    )
                    .unwrap();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            "session/prompt" if scenario == "empty_then_success" && attempt == 1 => {
                send(json!({"jsonrpc":"2.0", "id":id, "result":{"stopReason":"end_turn"}}));
            }
            "session/prompt" if scenario == "tool_stall" && attempt == 1 => {
                let session = request["params"]["sessionId"].as_str().unwrap();
                send(
                    json!({"jsonrpc":"2.0", "method":"session/update", "params":{
                        "sessionId":session, "update":{"sessionUpdate":"tool_call", "toolCallId":"stuck",
                            "title":"Waiting on tool", "kind":"execute", "status":"in_progress"}
                    }}),
                );
                let heartbeat = log.with_extension("heartbeat");
                let stop = log.with_extension("stop");
                let _writer = std::thread::spawn(move || {
                    while !stop.exists() {
                        let _ = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&heartbeat)
                            .and_then(|mut file| file.write_all(b"x"));
                        std::thread::sleep(Duration::from_millis(10));
                    }
                });
            }
            "session/prompt" if scenario == "permission" => {
                let session = request["params"]["sessionId"].as_str().unwrap();
                send(
                    json!({"jsonrpc":"2.0", "id":879,"method":"session/request_permission", "params":{
                        "sessionId":session,"toolCall":{"name":"unsafe-tool"},
                        "options":[{"optionId":"allow-once","name":"Allow","kind":"allow_once"},
                                   {"optionId":"reject-once","name":"Reject","kind":"reject_once"}]
                    }}),
                );
            }
            "session/prompt" if scenario == "success_child" && attempt == 1 => {
                #[allow(clippy::zombie_processes)]
                let child = Command::new(std::env::current_exe().unwrap())
                    .arg("--descendant")
                    .arg(log.with_extension("heartbeat"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                record(log, json!({"method":"descendant", "pid":child.id()}));
                let heartbeat = log.with_extension("heartbeat");
                for _ in 0..100 {
                    if heartbeat.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }

                let session = request["params"]["sessionId"].as_str().unwrap();
                send(
                    json!({"jsonrpc":"2.0", "method":"session/update", "params":{
                        "sessionId":session, "update":{"sessionUpdate":"agent_message_chunk",
                            "content":{"type":"text", "text":"---\nstatus: success\n---\nFinished"}}
                    }}),
                );
                send(json!({"jsonrpc":"2.0", "id":id, "result":{"stopReason":"end_turn"}}));
            }
            "session/prompt" => {
                let session = request["params"]["sessionId"].as_str().unwrap();
                let blocked = (matches!(
                    scenario.as_str(),
                    "declared"
                        | "twice"
                        | "load_error"
                        | "load_unsupported_after_wait"
                        | "hold_human"
                        | "delayed_success"
                ) && attempt <= if scenario == "twice" { 2 } else { 1 })
                    || (scenario == "role_switch" && matches!(attempt, 1 | 3));
                let output = if blocked {
                    "---\nstatus: blocked\n---\nOperator decision required"
                } else {
                    "---\nstatus: success\n---\nContinued the incomplete step"
                };
                if scenario == "delayed_success" && attempt == 2 {
                    std::thread::sleep(Duration::from_millis(500));
                }

                send(
                    json!({"jsonrpc":"2.0", "method":"session/update", "params":{
                        "sessionId":session,
                        "update":{"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":output}}
                    }}),
                );
                send(json!({"jsonrpc":"2.0", "id":id, "result":{"stopReason":"end_turn"}}));
            }
            _ => panic!("unexpected ACP method {method}"),
        }
    }
}
