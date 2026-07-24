use serde::{Deserialize, Serialize};
use serde_json::Value;

use cowboy_agent_client::{AgentInfo, Event, PromptContent, StopReason};

// ============================================================
// JSON-RPC 2.0 envelope types
// ============================================================

/// JSON-RPC 2.0 Request
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest<P: Serialize> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'static str,
    pub params: P,
}

impl<P: Serialize> JsonRpcRequest<P> {
    pub fn new(id: u64, method: &'static str, params: P) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params,
        }
    }
}

/// JSON-RPC 2.0 notification with no request id.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcNotification<P: Serialize> {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub params: P,
}

impl<P: Serialize> JsonRpcNotification<P> {
    pub fn new(method: &'static str, params: P) -> Self {
        Self {
            jsonrpc: "2.0",
            method,
            params,
        }
    }
}

/// JSON-RPC 2.0 Response (outgoing — for replying to Agent requests)
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse<R: Serialize> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub result: R,
}

impl<R: Serialize> JsonRpcResponse<R> {
    pub fn new(id: u64, result: R) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result,
        }
    }
}

// ============================================================
// ACP request parameter types (Client → Agent)
// ============================================================

/// initialize request params
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub client_capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

/// Client capabilities declared during initialize
#[derive(Debug, Clone, Serialize)]
pub struct ClientCapabilities {
    pub fs: FsCapabilities,
    pub terminal: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

/// Client info declared during initialize
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: &'static str,
    pub title: &'static str,
    pub version: &'static str,
}

/// session/new request params
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewParams {
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<SessionMeta>,
}

/// _meta extension field in session/new
#[derive(Debug, Clone, Serialize)]
pub struct SessionMeta {
    pub model: SessionModelMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionModelMeta {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// session/set_config_option request params for a select option
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionParams<'a> {
    pub session_id: &'a str,
    pub config_id: &'a str,
    pub value: &'a str,
}

/// session/cancel notification params
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCancelParams {
    pub session_id: String,
}

/// session/prompt request params
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptParams {
    pub session_id: String,
    pub prompt: Vec<PromptContent>,
}

/// session/load request params
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoadParams {
    pub session_id: String,
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
}

/// Permission grant response (Client → Agent)
#[derive(Debug, Clone, Serialize)]
pub struct PermissionOutcome {
    pub outcome: PermissionDecision,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionDecision {
    Selected {
        #[serde(rename = "optionId")]
        option_id: String,
    },
    Cancelled,
}

impl PermissionOutcome {
    pub fn selected(option_id: impl Into<String>) -> Self {
        Self {
            outcome: PermissionDecision::Selected {
                option_id: option_id.into(),
            },
        }
    }

    pub fn cancelled() -> Self {
        Self {
            outcome: PermissionDecision::Cancelled,
        }
    }

    pub fn allow_from_options(options: &[Value]) -> Self {
        let Some(option_id) = options
            .iter()
            .find(|option| {
                option
                    .get("kind")
                    .and_then(|kind| kind.as_str())
                    .is_some_and(|kind| kind == "allow_once" || kind == "allow_always")
            })
            .and_then(|option| option.get("optionId"))
            .and_then(|option_id| option_id.as_str())
        else {
            return Self::cancelled();
        };

        Self::selected(option_id)
    }
}

// ============================================================
// ACP response types (Agent → Client)
// ============================================================

/// initialize response result
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub agent_capabilities: Option<Value>,
    pub agent_info: Option<AgentInfo>,
}

/// session/new response result
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewResult {
    pub session_id: String,
    #[serde(default)]
    pub config_options: Vec<SessionConfigOption>,
}

/// session/load response result
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoadResult {
    #[serde(default)]
    pub config_options: Vec<SessionConfigOption>,
}

/// A session-level configuration option exposed by the ACP agent.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOption {
    pub id: String,
    #[serde(default)]
    pub category: Option<String>,
    pub current_value: Value,
    #[serde(default)]
    pub options: Vec<SessionConfigOptionValue>,
}

/// A selectable value for an ACP session configuration option.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionConfigOptionValue {
    pub value: String,
}

/// session/set_config_option response result
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionResult {
    #[serde(default)]
    pub config_options: Vec<SessionConfigOption>,
}

/// session/prompt response result
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptResult {
    pub stop_reason: Option<StopReason>,
}

// ============================================================
// ACP incoming message types (parsed from Agent output)
// ============================================================

/// Parsed ACP message from Agent
#[derive(Debug, Clone)]
pub enum Message {
    /// JSON-RPC Response
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// session/update notification
    SessionUpdate { session_id: String, update: Event },
    /// session/request_permission (Agent → Client request)
    PermissionRequest {
        id: u64,
        session_id: String,
        tool_call: Value,
        options: Vec<Value>,
    },
}

/// Parse ACP `session/update` payload into a normalized agent event.
fn parse_session_update_payload(value: &Value) -> Option<Event> {
    let update_type = value.get("sessionUpdate")?.as_str()?;
    match update_type {
        "agent_message_chunk" => Some(Event::MessageChunk {
            content: value.get("content").cloned().unwrap_or(Value::Null),
        }),
        "agent_thought_chunk" => Some(Event::ThoughtChunk {
            content: value.get("content").cloned().unwrap_or(Value::Null),
        }),
        "tool_call" => Some(Event::ToolCall {
            tool_call_id: value
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: value
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            kind: value
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "tool_call_update" => Some(Event::ToolCallUpdate {
            tool_call_id: value
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            content: value.get("content").cloned(),
        }),
        "plan" => Some(Event::Plan {
            entries: value
                .get("entries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
        }),
        "user_message_chunk" => Some(Event::UserMessageChunk {
            content: value.get("content").cloned().unwrap_or(Value::Null),
        }),
        other => Some(Event::Unknown {
            session_update: other.to_string(),
            raw: value.clone(),
        }),
    }
}

// ============================================================
// Message parsing
// ============================================================

/// Parse raw JSON-RPC line into typed Message
pub fn parse_acp_message(msg: &Value) -> Option<Message> {
    // JSON-RPC Response (has "id" + ("result" or "error"), no "method")
    if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
        if msg.get("method").is_none() {
            return Some(Message::Response {
                id,
                result: msg.get("result").cloned(),
                error: msg.get("error").cloned(),
            });
        }

        // JSON-RPC Request from Agent (has "id" + "method")
        if let Some(method) = msg.get("method").and_then(|v| v.as_str())
            && method == "session/request_permission"
        {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            return Some(Message::PermissionRequest {
                id,
                session_id: params
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                tool_call: params.get("toolCall").cloned().unwrap_or(Value::Null),
                options: params
                    .get("options")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default(),
            });
        }
    }

    // JSON-RPC Notification (has "method", no "id")
    if let Some(method) = msg.get("method").and_then(|v| v.as_str())
        && method == "session/update"
    {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // ACP spec: update payload is in params.update, not params itself
        let update_value = params.get("update").cloned().unwrap_or(Value::Null);
        tracing::trace!(raw_update = %update_value, "session/update raw payload");
        if let Some(update) = parse_session_update_payload(&update_value) {
            return Some(Message::SessionUpdate { session_id, update });
        } else {
            tracing::warn!(raw = %update_value, "session/update: failed to parse update payload");
        }
    }

    tracing::trace!(raw = %msg, "parse_acp_message: unrecognized message");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_load_result_defaults_missing_config_options() {
        let result: SessionLoadResult = serde_json::from_value(serde_json::json!({})).unwrap();

        assert!(result.config_options.is_empty());
    }

    #[test]
    fn test_parse_response() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"sessionId": "abc"}
        });
        let parsed = parse_acp_message(&msg).unwrap();
        match parsed {
            Message::Response { id, result, error } => {
                assert_eq!(id, 1);
                assert!(result.is_some());
                assert!(error.is_none());
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_parse_error_response() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {"code": -1, "message": "fail"}
        });
        let parsed = parse_acp_message(&msg).unwrap();
        match parsed {
            Message::Response { id, error, .. } => {
                assert_eq!(id, 2);
                assert!(error.is_some());
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_parse_permission_request() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess_1",
                "toolCall": {"name": "write_file"}
            }
        });
        let parsed = parse_acp_message(&msg).unwrap();
        match parsed {
            Message::PermissionRequest {
                id,
                session_id,
                options,
                ..
            } => {
                assert_eq!(id, 5);
                assert_eq!(session_id, "sess_1");
                assert!(options.is_empty());
            }
            _ => panic!("Expected PermissionRequest"),
        }
    }

    #[test]
    fn test_permission_outcome_selects_allow_option() {
        let outcome = PermissionOutcome::allow_from_options(&[
            serde_json::json!({
                "optionId": "reject-once",
                "kind": "reject_once"
            }),
            serde_json::json!({
                "optionId": "allow-once",
                "kind": "allow_once"
            }),
        ]);
        let value = serde_json::to_value(outcome).unwrap();

        assert_eq!(value["outcome"]["outcome"], "selected");
        assert_eq!(value["outcome"]["optionId"], "allow-once");
    }

    #[test]
    fn test_permission_outcome_cancels_without_allow_option() {
        let outcome = PermissionOutcome::allow_from_options(&[serde_json::json!({
            "optionId": "reject-once",
            "kind": "reject_once"
        })]);
        let value = serde_json::to_value(outcome).unwrap();

        assert_eq!(value["outcome"]["outcome"], "cancelled");
        assert!(value["outcome"].get("optionId").is_none());
    }

    #[test]
    fn parses_tool_call_session_update_fields() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call_1",
                    "title": "Reading app state",
                    "kind": "read",
                    "status": "pending"
                }
            }
        });

        let parsed = parse_acp_message(&msg).unwrap();
        match parsed {
            Message::SessionUpdate {
                session_id,
                update:
                    Event::ToolCall {
                        tool_call_id,
                        title,
                        kind,
                        status,
                    },
            } => {
                assert_eq!(session_id, "sess_1");
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(title, "Reading app state");
                assert_eq!(kind, "read");
                assert_eq!(status, "pending");
            }
            other => panic!("expected tool call session update, got {other:?}"),
        }
    }

    #[test]
    fn parses_tool_call_update_session_update_content() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call_1",
                    "status": "completed",
                    "content": {"text": "read complete"}
                }
            }
        });

        let parsed = parse_acp_message(&msg).unwrap();
        match parsed {
            Message::SessionUpdate {
                session_id,
                update:
                    Event::ToolCallUpdate {
                        tool_call_id,
                        status,
                        content,
                    },
            } => {
                assert_eq!(session_id, "sess_1");
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(status, "completed");
                assert_eq!(content.unwrap()["text"], "read complete");
            }
            other => panic!("expected tool update session update, got {other:?}"),
        }
    }

    #[test]
    fn parses_agent_message_chunk_session_update() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": "assistant text"
                }
            }
        });

        let parsed = parse_acp_message(&msg).unwrap();
        match parsed {
            Message::SessionUpdate {
                session_id,
                update: Event::MessageChunk { content },
            } => {
                assert_eq!(session_id, "sess_1");
                assert_eq!(content, serde_json::json!("assistant text"));
            }
            other => panic!("expected agent message chunk, got {other:?}"),
        }
    }

    #[test]
    fn parses_agent_thought_chunk_session_update() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"text": "thinking"}
                }
            }
        });

        let parsed = parse_acp_message(&msg).unwrap();
        match parsed {
            Message::SessionUpdate {
                session_id,
                update: Event::ThoughtChunk { content },
            } => {
                assert_eq!(session_id, "sess_1");
                assert_eq!(content["text"], "thinking");
            }
            other => panic!("expected agent thought chunk, got {other:?}"),
        }
    }
    #[test]
    fn test_parse_unknown_returns_none() {
        let msg = serde_json::json!({"foo": "bar"});
        assert!(parse_acp_message(&msg).is_none());
    }
}
