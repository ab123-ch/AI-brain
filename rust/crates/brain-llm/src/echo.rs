//! 回声 LLM Provider — 无 API Key 时的安全回退
//!
//! 当 LLM 配置不可用（无 API Key、网络不可达等）时，
//! 使用此 Provider 保持系统可用，所有查询返回回声响应。
//! 工具调用不可用，但 status/weights/memory 查询正常工作。

use std::future::Future;
use std::pin::Pin;

use crate::provider::{ChatRequest, ChatResponse, LlmProvider};
use crate::types::{ContentBlock, FinishReason, StreamEvent, TokenUsage};

/// 回声 LLM Provider
///
/// 不调用任何远程 API，仅将用户最后一条消息包装为回声响应。
/// 用于首次运行或无 API Key 时的安全回退。
pub struct EchoLlmProvider {
    model_name: String,
}

impl EchoLlmProvider {
    /// 创建回声 Provider
    #[must_use]
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
        }
    }
}

impl LlmProvider for EchoLlmProvider {
    fn model(&self) -> &str {
        &self.model_name
    }

    fn complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = crate::Result<ChatResponse>> + Send + '_>> {
        let model = self.model_name.clone();
        Box::pin(async move {
            // 提取最后一条用户消息
            let last_user_msg = request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, crate::provider::MessageRole::User))
                .map(|m| m.text_content())
                .unwrap_or_default();

            let echo_text = if last_user_msg.is_empty() {
                "（回声模式：没有检测到用户输入）".to_string()
            } else {
                format!("[回声模式] 收到你的消息: {last_user_msg}\n\n提示: 配置 ZHIPU_API_KEY 环境变量以启用真实 AI 响应。")
            };

            Ok(ChatResponse {
                content: vec![ContentBlock::text(echo_text)],
                model,
                usage: TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    ..Default::default()
                },
                finish_reason: Some(FinishReason::EndTurn),
            })
        })
    }

    fn stream_complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = crate::Result<Vec<StreamEvent>>> + Send + '_>> {
        let _model = self.model_name.clone();
        Box::pin(async move {
            let last_user_msg = request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, crate::provider::MessageRole::User))
                .map(|m| m.text_content())
                .unwrap_or_default();

            Ok(vec![
                StreamEvent::TextDelta {
                    text: format!("[回声模式] {last_user_msg}"),
                },
                StreamEvent::Done {
                    usage: Some(TokenUsage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                        ..Default::default()
                    }),
                    finish_reason: Some(FinishReason::EndTurn),
                },
            ])
        })
    }

    fn stream_incremental(
        &self,
        request: ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = crate::Result<tokio::sync::mpsc::Receiver<StreamEvent>>>
                + Send
                + '_,
        >,
    > {
        let _model = self.model_name.clone();
        Box::pin(async move {
            let last_user_msg = request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, crate::provider::MessageRole::User))
                .map(|m| m.text_content())
                .unwrap_or_default();

            let (tx, rx) = tokio::sync::mpsc::channel(4);
            let _ = tx
                .send(StreamEvent::TextDelta {
                    text: format!("[回声模式] {last_user_msg}"),
                })
                .await;
            let _ = tx
                .send(StreamEvent::Done {
                    usage: Some(TokenUsage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                        ..Default::default()
                    }),
                    finish_reason: Some(FinishReason::EndTurn),
                })
                .await;

            Ok(rx)
        })
    }
}
