use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::types::{ContentBlock, FinishReason, ToolChoice, ToolDefinition, TokenUsage};

/// 消息角色
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
}

impl ChatMessage {
    /// System message with plain text.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: vec![ContentBlock::text(content)],
        }
    }

    /// User message with plain text.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: vec![ContentBlock::text(content)],
        }
    }

    /// Assistant message with plain text.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::text(content)],
        }
    }

    /// User message from a tool result block.
    pub fn tool_result(
        tool_use_id: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: MessageRole::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: output.into(),
                is_error,
            }],
        }
    }

    /// Assistant message with mixed content blocks (text + tool calls).
    pub fn assistant_blocks(blocks: Vec<ContentBlock>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: blocks,
        }
    }

    /// Extract all text from this message's content blocks.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract all tool-use blocks from this message.
    pub fn tool_calls(&self) -> Vec<&ContentBlock> {
        self.content.iter().filter(|b| b.is_tool_use()).collect()
    }
}

/// LLM 请求
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub tool_choice: Option<ToolChoice>,
}

/// LLM 响应
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The response content blocks (text + optional tool calls).
    pub content: Vec<ContentBlock>,
    /// The model that generated this response.
    pub model: String,
    /// Token usage statistics.
    pub usage: TokenUsage,
    /// Why the LLM stopped generating.
    pub finish_reason: Option<FinishReason>,
}

impl ChatResponse {
    /// Extract the text portion of the response (excludes Thinking blocks).
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract all tool-use blocks from the response.
    pub fn tool_calls(&self) -> Vec<&ContentBlock> {
        self.content.iter().filter(|b| b.is_tool_use()).collect()
    }

    /// Check if the response contains any tool calls.
    pub fn has_tool_calls(&self) -> bool {
        self.content.iter().any(|b| b.is_tool_use())
    }
}

/// LLM Provider 统一 trait
pub trait LlmProvider: Send + Sync {
    /// 获取当前使用的模型名
    fn model(&self) -> &str;

    /// 非流式补全
    fn complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = crate::Result<ChatResponse>> + Send + '_>>;

    /// Batch streaming: collect all SSE events into a Vec
    fn stream_complete(
        &self,
        _request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = crate::Result<Vec<crate::types::StreamEvent>>> + Send + '_>>
    {
        Box::pin(async { Err(crate::error::LlmError::RequestFailed("Streaming not supported".into())) })
    }

    /// Incremental streaming: returns a channel of SSE events
    fn stream_incremental(
        &self,
        _request: ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = crate::Result<tokio::sync::mpsc::Receiver<crate::types::StreamEvent>>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async { Err(crate::error::LlmError::RequestFailed("Streaming not supported".into())) })
    }
}
