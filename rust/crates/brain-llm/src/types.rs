//! Content blocks, tool definitions, and streaming types for tool-use support.
//!
//! These types extend the basic LLM protocol to support multi-content messages
//! (text + tool calls + tool results) as used by the reasoning brain's
//! LLM <-> tool execution loop.

use serde::{Deserialize, Serialize};

// ContentBlock is defined in brain-core; re-export here for convenience.
pub use brain_core::types::ContentBlock;

// ---------------------------------------------------------------------------
// Tool Definitions (sent to the LLM so it knows what tools are available)
// ---------------------------------------------------------------------------

/// Description of a tool the LLM can invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (e.g. "read_file", "bash").
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

/// How the LLM should choose which tool to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// LLM decides whether to use a tool.
    Auto,
    /// LLM must not use any tool.
    None,
    /// LLM must use a specific tool.
    Tool { name: String },
}

// ---------------------------------------------------------------------------
// Finish Reason
// ---------------------------------------------------------------------------

/// Why the LLM stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Normal end of turn (text response complete).
    EndTurn,
    /// LLM is requesting a tool call.
    ToolUse,
    /// Hit the max_tokens limit.
    MaxTokens,
}

impl FinishReason {
    /// Parse from the string returned by OpenAI-compatible APIs.
    pub fn from_api_str(s: &str) -> Self {
        match s {
            "tool_calls" => Self::ToolUse,
            "length" => Self::MaxTokens,
            _ => Self::EndTurn,
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming Events
// ---------------------------------------------------------------------------

/// An event emitted during SSE streaming.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of text content.
    TextDelta { text: String },
    /// A chunk of thinking/reasoning content (from models that stream reasoning).
    ThinkingDelta { content: String },
    /// Start of a tool call (ID + name known, input streaming).
    ToolCallStart { id: String, name: String },
    /// A chunk of tool input JSON.
    ToolCallDelta { tool_use_id: String, delta: String },
    /// The full response is complete.
    Done {
        finish_reason: Option<FinishReason>,
        usage: Option<TokenUsage>,
    },
}

/// Token usage statistics (re-exported here for stream consumers).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Tokens written to prompt cache (Anthropic/DeepSeek cache write).
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// Tokens read from prompt cache (cache hit).
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl TokenUsage {
    /// Total input tokens including cache creation and read.
    pub fn total_input_tokens(&self) -> u64 {
        self.prompt_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }

    /// Cache hit rate: what fraction of total input tokens came from cache.
    /// Returns None if total input is zero.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let total = self.total_input_tokens();
        if total == 0 {
            None
        } else {
            Some(self.cache_read_input_tokens as f64 / total as f64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_reason_from_api_str() {
        assert_eq!(FinishReason::from_api_str("stop"), FinishReason::EndTurn);
        assert_eq!(
            FinishReason::from_api_str("tool_calls"),
            FinishReason::ToolUse
        );
        assert_eq!(
            FinishReason::from_api_str("length"),
            FinishReason::MaxTokens
        );
        assert_eq!(FinishReason::from_api_str("unknown"), FinishReason::EndTurn);
    }

    #[test]
    fn tool_definition_serialization() {
        let def = ToolDefinition {
            name: "bash".into(),
            description: "Run a bash command".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        };
        let json = serde_json::to_string(&def).unwrap();
        let de: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(de.name, "bash");
    }

    #[test]
    fn tool_choice_serialization() {
        let choice = ToolChoice::Auto;
        let json = serde_json::to_string(&choice).unwrap();
        assert!(json.contains("auto"));
    }
}
