use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;

use crate::transport::Transport;

/// Mock transport that plays back pre-configured responses and records outgoing messages.
pub struct MockTransport {
    incoming: Arc<Mutex<VecDeque<String>>>,
    outgoing: Arc<Mutex<Vec<String>>>,
    closed: bool,
}

impl MockTransport {
    pub fn new(responses: Vec<&str>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(
                responses.into_iter().map(|s| s.to_string()).collect(),
            )),
            outgoing: Arc::new(Mutex::new(Vec::new())),
            closed: false,
        }
    }

    pub fn outgoing(&self) -> Arc<Mutex<Vec<String>>> {
        self.outgoing.clone()
    }

    #[allow(dead_code)]
    pub fn incoming(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.incoming.clone()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send(&mut self, message: &str) -> anyhow::Result<()> {
        self.outgoing.lock().push(message.to_string());
        Ok(())
    }

    async fn recv(&mut self) -> anyhow::Result<Option<String>> {
        Ok(self.incoming.lock().pop_front())
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.closed = true;
        Ok(())
    }
}

pub fn rpc_response(id: u64, result: Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
    .to_string()
}

pub fn rpc_error(id: u64, code: i64, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
    .to_string()
}

pub fn session_update(session_id: &str, update: Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": update
        }
    })
    .to_string()
}

pub fn permission_request(id: u64, session_id: &str, tool: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/request_permission",
        "params": {
            "sessionId": session_id,
            "toolCall": {"name": tool},
            "options": [
                {
                    "optionId": "allow-once",
                    "name": "Allow once",
                    "kind": "allow_once"
                },
                {
                    "optionId": "reject-once",
                    "name": "Reject",
                    "kind": "reject_once"
                }
            ]
        }
    })
    .to_string()
}

pub fn init_response(id: u64) -> String {
    rpc_response(
        id,
        serde_json::json!({
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": true},
            "agentInfo": {"name": "mock-agent", "version": "1.0"}
        }),
    )
}

pub fn session_new_response(id: u64, session_id: &str) -> String {
    rpc_response(id, serde_json::json!({"sessionId": session_id}))
}

pub fn prompt_response(id: u64, stop_reason: &str) -> String {
    rpc_response(id, serde_json::json!({"stopReason": stop_reason}))
}

pub fn text_chunk_update(session_id: &str, text: &str) -> String {
    session_update(
        session_id,
        serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text}
        }),
    )
}
