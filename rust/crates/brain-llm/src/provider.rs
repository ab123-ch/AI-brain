use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// 消息角色
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }
}

/// LLM 请求
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    /// 覆盖默认模型（None 时使用 provider 默认模型）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

/// Token 用量
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// LLM 响应
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub model: String,
    pub usage: TokenUsage,
    pub finish_reason: Option<String>,
}

/// LLM Provider 统一 trait
///
/// 所有 LLM 客户端（OpenAI 兼容、本地模型等）实现此 trait。
/// 各副脑通过 `Box<dyn LlmProvider>` 持有实例。
pub trait LlmProvider: Send + Sync {
    /// 获取当前使用的模型名
    fn model(&self) -> &str;

    /// 非流式补全
    fn complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = crate::Result<ChatResponse>> + Send + '_>>;
}
