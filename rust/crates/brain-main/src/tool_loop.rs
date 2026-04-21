use std::time::Instant;

use brain_core::guard_check::{guard_check, GuardResult};
use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{ProgressEvent, ToolCall, ToolExecutionResult};
use brain_llm::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, LlmProvider, ToolDefinition,
};

use crate::error::{MainBrainError, Result};

/// tool_loop 最大循环次数（防止无限工具调用）
const MAX_TOOL_LOOP_ITERATIONS: u32 = 20;

/// 工具调用参数截断长度
const INPUT_PREVIEW_MAX: usize = 80;
/// 工具结果预览截断长度
const OUTPUT_PREVIEW_MAX: usize = 100;

/// tool_loop 返回结果
pub(crate) struct ToolLoopResult {
    /// LLM 最终文本响应
    pub(crate) response: ChatResponse,
    /// LLM 调用总次数（含多轮工具调用）
    pub(crate) llm_calls: u32,
}

/// 运行 tool_loop — LLM ↔ 工具 循环直到 LLM 不再调用工具（默认参数的便捷入口）
#[allow(dead_code)]
pub async fn run_tool_loop(
    llm: &dyn LlmProvider,
    tool_executor: &dyn ToolExecutor,
    messages: &mut Vec<ChatMessage>,
    tools: &[ToolDefinition],
    progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
) -> Result<ToolLoopResult> {
    run_tool_loop_with_config(llm, tool_executor, messages, tools, progress_tx, 4096, 0.7).await
}

/// 带配置参数的 tool_loop
pub async fn run_tool_loop_with_config(
    llm: &dyn LlmProvider,
    tool_executor: &dyn ToolExecutor,
    messages: &mut Vec<ChatMessage>,
    tools: &[ToolDefinition],
    progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
    max_tokens: u32,
    temperature: f64,
) -> Result<ToolLoopResult> {
    let mut llm_calls = 0u32;

    loop {
        llm_calls += 1;
        if llm_calls > MAX_TOOL_LOOP_ITERATIONS {
            return Err(MainBrainError::MaxRetriesExceeded(MAX_TOOL_LOOP_ITERATIONS));
        }

        let request = build_request(messages, tools, max_tokens, temperature);

        // 实时日志：打印发送给 LLM 的完整上下文
        tracing::info!(
            "=== LLM 请求 [第{llm_calls}次] === 消息数={}, 工具数={}, max_tokens={max_tokens}, temp={temperature}",
            request.messages.len(),
            request.tools.as_ref().map_or(0, std::vec::Vec::len),
        );
        for (i, msg) in request.messages.iter().enumerate() {
            let role_str = match msg.role {
                brain_llm::MessageRole::System => "system",
                brain_llm::MessageRole::User => "user",
                brain_llm::MessageRole::Assistant => "assistant",
                brain_llm::MessageRole::Tool => "tool",
            };
            let text = msg.text_content();
            let preview = trunc(&text, 200);
            tracing::info!(
                "  msg[{i}] [{role_str}] ({len}字) {preview}",
                len = text.chars().count()
            );
            // 打印 tool_use 和 tool_result 块
            for (bi, block) in msg.content.iter().enumerate() {
                match block {
                    brain_llm::ContentBlock::ToolUse { name, input, .. } => {
                        let input_str = serde_json::to_string(input).unwrap_or_default();
                        tracing::info!(
                            "  msg[{i}].block[{bi}] [tool_use] {name}: {}",
                            trunc(&input_str, 120)
                        );
                    }
                    brain_llm::ContentBlock::ToolResult {
                        content, is_error, ..
                    } => {
                        tracing::info!(
                            "  msg[{i}].block[{bi}] [tool_result{}] {}",
                            if *is_error { " ERROR" } else { "" },
                            trunc(content, 120),
                        );
                    }
                    brain_llm::ContentBlock::Text { .. } => {} // 已在 text_content 中打印
                    brain_llm::ContentBlock::Thinking { .. } => {} // thinking 不打印
                }
            }
        }

        let response = llm
            .complete(request)
            .await
            .map_err(|e| MainBrainError::LlmError(format!("{e}")))?;

        // 实时日志：打印 LLM 响应
        let resp_text = response.text();
        let tool_calls = response.tool_calls();
        tracing::info!(
            "=== LLM 响应 === text({len}字): {preview}{}",
            if tool_calls.is_empty() {
                String::new()
            } else {
                format!(", tool_calls={}", tool_calls.len())
            },
            len = resp_text.chars().count(),
            preview = trunc(&resp_text, 200),
        );
        for (i, block) in response.content.iter().enumerate() {
            if let brain_llm::ContentBlock::ToolUse { name, input, .. } = block {
                let input_str = serde_json::to_string(input).unwrap_or_default();
                tracing::info!(
                    "  resp.block[{i}] [tool_use] {name}: {}",
                    trunc(&input_str, 120)
                );
            }
        }

        if !response.has_tool_calls() {
            return Ok(ToolLoopResult {
                response,
                llm_calls,
            });
        }

        messages.push(ChatMessage::assistant_blocks(response.content.clone()));
        execute_tool_calls(tool_executor, &response, messages, progress_tx).await;
    }
}

/// 构建 LLM 请求
fn build_request(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    max_tokens: u32,
    temperature: f64,
) -> ChatRequest {
    ChatRequest {
        model: None,
        messages: messages.to_vec(),
        max_tokens: Some(max_tokens),
        temperature: Some(temperature),
        tools: if tools.is_empty() {
            None
        } else {
            Some(tools.to_vec())
        },
        tool_choice: None,
    }
}

/// 执行 LLM 响应中的所有工具调用，将结果追加到 messages
async fn execute_tool_calls(
    tool_executor: &dyn ToolExecutor,
    response: &ChatResponse,
    messages: &mut Vec<ChatMessage>,
    progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
) {
    for tool_block in response.tool_calls() {
        if let ContentBlock::ToolUse { id, name, input } = tool_block {
            let tool_call = ToolCall {
                tool_name: name.clone(),
                input: input.clone(),
                validated: false,
                validation_id: None,
            };

            // 安学校验
            if let GuardResult::NeedUserConfirm { reason } = guard_check(&tool_call) {
                messages.push(ChatMessage::tool_result(
                    id,
                    format!("安全拒绝: {reason}"),
                    true,
                ));
                send_progress(
                    progress_tx,
                    ProgressEvent::ToolDone {
                        brain: "main".into(),
                        tool_name: name.clone(),
                        duration_ms: 0,
                        output_preview: trunc(&format!("安全拒绝: {reason}"), OUTPUT_PREVIEW_MAX),
                        is_error: true,
                    },
                )
                .await;
                continue;
            }

            let input_preview = trunc(
                &serde_json::to_string(input).unwrap_or_default(),
                INPUT_PREVIEW_MAX,
            );
            send_progress(
                progress_tx,
                ProgressEvent::ToolStart {
                    brain: "main".into(),
                    tool_name: name.clone(),
                    input: input_preview,
                },
            )
            .await;

            // 执行工具
            let start = Instant::now();
            let result: ToolExecutionResult = tool_executor.execute(&tool_call).await;
            let duration_ms = start.elapsed().as_millis() as u64;
            let output_preview = trunc(&result.output, OUTPUT_PREVIEW_MAX);

            send_progress(
                progress_tx,
                ProgressEvent::ToolDone {
                    brain: "main".into(),
                    tool_name: name.clone(),
                    duration_ms,
                    output_preview: output_preview.clone(),
                    is_error: result.is_error,
                },
            )
            .await;

            messages.push(ChatMessage::tool_result(id, result.output, result.is_error));
        }
    }
}

async fn send_progress(
    tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
    event: ProgressEvent,
) {
    if let Some(tx) = tx {
        let _ = tx.send(event).await;
    }
}

/// 截断字符串到指定字符数
fn trunc(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::tool_executor::StubToolExecutor;
    use brain_llm::TokenUsage;
    use std::future::Future;
    use std::pin::Pin;

    /// 简单 LLM stub：直接返回文本
    struct TextLlm;

    impl LlmProvider for TextLlm {
        fn model(&self) -> &'static str {
            "stub"
        }

        fn complete(
            &self,
            _request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            Box::pin(async {
                Ok(ChatResponse {
                    content: vec![ContentBlock::text("最终回答")],
                    model: "stub".into(),
                    usage: TokenUsage::default(),
                    finish_reason: Some(brain_llm::FinishReason::EndTurn),
                })
            })
        }
    }

    #[tokio::test]
    async fn tool_loop_returns_text_when_no_tool_calls() {
        let llm = TextLlm;
        let executor = StubToolExecutor::new();
        let mut messages = vec![ChatMessage::user("测试")];

        let result = run_tool_loop(&llm, &executor, &mut messages, &[], None)
            .await
            .unwrap();

        assert_eq!(result.response.text(), "最终回答");
        assert_eq!(result.llm_calls, 1);
    }
}
