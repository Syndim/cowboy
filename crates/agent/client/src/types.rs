use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Model descriptor passed to agent session creation.
///
/// Backends that recognize the field can use it to select a provider/model;
/// backends that do not can ignore it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelInfo {
    /// Provider-specific model id (e.g. `"claude-sonnet-4-20250514"`).
    pub id: String,
    /// Optional provider name (e.g. `"anthropic"`, `"openai"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl ModelInfo {
    /// Build with just an id.
    pub fn with_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            provider: None,
        }
    }
}

/// Agent-returned session descriptor built exclusively from values the agent
/// reports back (never the configured `ModelInfo` Cowboy sends).
///
/// Each field holds the raw, agent-returned value for that facet; a facet the
/// agent does not report is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentSessionDescriptor {
    /// Agent-returned model value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Agent-returned supported context size value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Agent-returned reasoning/thought-level value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Provider-reported agent metadata, when available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub title: Option<String>,
    pub version: Option<String>,
}

/// Prompt content passed from Cowboy to an agent backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptContent {
    #[serde(rename = "type")]
    pub content_type: &'static str,
    pub text: String,
}

impl PromptContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content_type: "text",
            text: text.into(),
        }
    }
}

/// Normalized agent event delivered while a prompt is running.
///
/// ACP currently maps one-to-one into this shape. Future SDK-backed clients
/// should convert provider-native stream events into these variants.
#[derive(Debug, Clone)]
pub enum Event {
    MessageChunk {
        content: Value,
    },
    ThoughtChunk {
        content: Value,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: String,
        status: String,
    },
    ToolCallUpdate {
        tool_call_id: String,
        status: String,
        content: Option<Value>,
    },
    Plan {
        entries: Vec<Value>,
    },
    UserMessageChunk {
        content: Value,
    },
    Unknown {
        session_update: String,
        raw: Value,
    },
}

/// Turn stop reason normalized across agent backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StopReason {
    #[serde(rename = "end_turn")]
    EndTurn,
    #[serde(rename = "max_tokens")]
    MaxTokens,
    #[serde(rename = "max_turn_requests")]
    MaxTurnRequests,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "refusal")]
    Refusal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_info_serde() {
        let m = ModelInfo {
            id: "claude-sonnet".into(),
            provider: Some("anthropic".into()),
        };
        let json = serde_json::to_string(&m).unwrap();
        let parsed: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "claude-sonnet");
        assert_eq!(parsed.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_model_info_with_id() {
        let m = ModelInfo::with_id("gpt-4");
        assert_eq!(m.id, "gpt-4");
        assert!(m.provider.is_none());
    }
}
