//! Content blocks, tool definitions, and streaming types for tool-use support.
//!
//! These types extend the basic LLM protocol to support multi-content messages
//! (text + tool calls + tool results) as used by the reasoning brain's
//! LLM <-> tool execution loop.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Content Blocks (multi-modal message content)
// ---------------------------------------------------------------------------

/// A single content block within a chat message.
///
/// Messages may contain multiple blocks: text, tool-use requests,
/// and tool-execution results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text { text: String },
    /// Model's internal reasoning/thinking process (e.g. `<thinking>` tags from MiniMax/DeepSeek).
    /// Not shown to users by default — filtered out by `as_text()` / `text()`.
    Thinking { content: String },
    /// A tool-use request from the assistant.
    #[serde(rename = "tool_use")]
    ToolUse {
        /// Unique ID for this tool call (e.g. "toolu_01abc").
        id: String,
        /// Name of the tool to invoke.
        name: String,
        /// JSON input for the tool.
        input: serde_json::Value,
    },
    /// The result of executing a tool call, sent back to the LLM.
    #[serde(rename = "tool_result")]
    ToolResult {
        /// The ID of the tool call this result is for.
        tool_use_id: String,
        /// The output content (or error message).
        content: String,
        /// Whether the tool execution failed.
        is_error: bool,
    },
}

impl ContentBlock {
    /// Convenience: create a text block.
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text {
            text: content.into(),
        }
    }

    /// Extract text if this is a Text block, otherwise None.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }

    /// Check if this is a ToolUse block.
    pub fn is_tool_use(&self) -> bool {
        matches!(self, Self::ToolUse { .. })
    }

    /// Check if this is a ToolResult block.
    pub fn is_tool_result(&self) -> bool {
        matches!(self, Self::ToolResult { .. })
    }

    /// Convenience: create a thinking block.
    pub fn thinking(content: impl Into<String>) -> Self {
        Self::Thinking {
            content: content.into(),
        }
    }

    /// Check if this is a Thinking block.
    pub fn is_thinking(&self) -> bool {
        matches!(self, Self::Thinking { .. })
    }

    /// Extract thinking content if this is a Thinking block, otherwise None.
    pub fn as_thinking(&self) -> Option<&str> {
        match self {
            Self::Thinking { content } => Some(content),
            _ => None,
        }
    }
}

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_text_roundtrip() {
        let block = ContentBlock::text("hello world");
        let json = serde_json::to_string(&block).unwrap();
        let de: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(de.as_text(), Some("hello world"));
    }

    #[test]
    fn content_block_tool_use_roundtrip() {
        let block = ContentBlock::ToolUse {
            id: "toolu_01".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "/tmp/test.rs"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        let de: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(de.is_tool_use());
    }

    #[test]
    fn content_block_tool_result_roundtrip() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "toolu_01".into(),
            content: "file contents here".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&block).unwrap();
        let de: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(de.is_tool_result());
    }

    #[test]
    fn content_block_thinking_roundtrip() {
        let block = ContentBlock::thinking("internal reasoning here");
        let json = serde_json::to_string(&block).unwrap();
        let de: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(de.is_thinking());
        assert_eq!(de.as_thinking(), Some("internal reasoning here"));
        // as_text() returns None for Thinking blocks
        assert!(de.as_text().is_none());
    }

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
