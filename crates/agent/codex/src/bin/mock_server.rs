//! Deterministic stdio fixture for the app-server contract tests.
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

fn send(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mut log = std::fs::File::create(&args[1]).unwrap();
    writeln!(log, "{}", std::process::id()).unwrap();
    let mode = &args[2];
    #[cfg(unix)]
    if mode == "tree" {
        let marker = std::path::Path::new(&args[1]).with_extension("survivor");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 1; touch \"$1\"", "--"])
            .arg(marker)
            .spawn()
            .unwrap();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    for line in io::stdin().lock().lines() {
        let msg: Value = serde_json::from_str(&line.unwrap()).unwrap();
        writeln!(log, "{msg}").unwrap();
        log.flush().unwrap();
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => send(json!({"id":id,"result":{"userAgent":"codex-test"}})),
            "initialized" => {}
            "thread/start" | "thread/resume" => {
                let sandbox = if mode == "unsafe" {
                    "dangerFullAccess"
                } else {
                    "workspaceWrite"
                };
                send(
                    json!({"id":id,"result":{"thread":{"id":"thread-1"},"model":"returned-model","sandbox":{"type":sandbox},"approvalPolicy":"on-request","approvalsReviewer":"user"}}),
                );
            }
            "turn/start" => {
                send(
                    json!({"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","delta":"hello"}}),
                );
                if mode == "cancel-before-start" {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                send(json!({"id":id,"result":{"turn":{"id":"turn-1"}}}));
                if mode == "cancel" || mode == "cancel-before-start" || mode == "hang" {
                    continue;
                }
                if mode == "error" {
                    send(
                        json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"failed","error":{"message":"secret"}}}}),
                    );
                    continue;
                }
                send(
                    json!({"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","item":{"id":"tool","type":"commandExecution","command":"pwd"}}}),
                );
                send(
                    json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{}}),
                );
            }
            "turn/interrupt" => {
                send(json!({"id":id,"result":{}}));
                send(
                    json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"interrupted"}}}),
                );
            }
            _ if msg.get("id") == Some(&json!("approval")) => {
                if msg.pointer("/result/decision") != Some(&json!("decline")) {
                    std::process::exit(2);
                }
                send(
                    json!({"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","item":{"id":"tool","type":"commandExecution","status":"completed","aggregatedOutput":"ok"}}}),
                );
                send(
                    json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}}),
                );
            }
            _ => {}
        }
    }
}
