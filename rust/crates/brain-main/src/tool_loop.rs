use std::time::Instant;

use brain_core::guard_check::{guard_check, GuardResult};
use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{
    ProgressEvent, ToolCall, ToolCallRecord, ToolExecutionResult, TurnRecord, TurnRole,
};
use brain_hooks::runner::HookRunner;
use brain_hooks::types::{HookDecision, HookEvent, HookInput};
use brain_llm::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, LlmProvider, ToolDefinition,
};

use crate::error::{MainBrainError, Result};

/// tool_loop 返回结果
pub(crate) struct ToolLoopResult {
    /// LLM 最终文本响应
    pub(crate) response: ChatResponse,
    /// LLM 调用总次数（含多轮工具调用）
    pub(crate) llm_calls: u32,
    /// 完整对话轨迹（每轮 assistant 回复 + 工具调用 + 工具结果）
    pub(crate) turns: Vec<TurnRecord>,
    /// 累计 prompt_tokens（从 LLM 返回的 usage 中累加）
    pub(crate) total_prompt_tokens: u64,
}

/// 运行 tool_loop — LLM ↔ 工具 循环直到 LLM 不再调用工具（默认参数的便捷入口）
#[allow(dead_code)]
pub async fn run_tool_loop(
    llm: &dyn LlmProvider,
    tool_executor: &dyn ToolExecutor,
    messages: &mut Vec<ChatMessage>,
    tools: &[ToolDefinition],
    progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
    hook_runner: Option<&HookRunner>,
) -> Result<ToolLoopResult> {
    run_tool_loop_with_config(
        llm,
        tool_executor,
        messages,
        tools,
        progress_tx,
        hook_runner,
        4096,
        0.7,
    )
    .await
}

/// 带配置参数的 tool_loop
pub async fn run_tool_loop_with_config(
    llm: &dyn LlmProvider,
    tool_executor: &dyn ToolExecutor,
    messages: &mut Vec<ChatMessage>,
    tools: &[ToolDefinition],
    progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
    hook_runner: Option<&HookRunner>,
    max_tokens: u32,
    temperature: f64,
) -> Result<ToolLoopResult> {
    let mut llm_calls = 0u32;
    let mut turns: Vec<TurnRecord> = Vec::new();
    let mut total_prompt_tokens = 0u64;

    loop {
        llm_calls += 1;
        // 不设硬限制，由上下文窗口和 LLM 自身决定何时停止
        // （Claude Code 同样无硬限制）

        let request = build_request(messages, tools, max_tokens, temperature);

        // === 日志：LLM 请求（按轮次区分） ===
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
            let char_count = text.chars().count();

            // system prompt 只在第一轮完整打印，后续轮次只打印长度
            if msg.role == brain_llm::MessageRole::System && llm_calls > 1 {
                tracing::info!("  msg[{i}] [system] ({char_count}字) [同上，省略重复]");
            }

            // 非空文本完整打印（不截断）
            if !text.is_empty() {
                tracing::info!("  msg[{i}] [{role_str}] ({char_count}字) {text}");
            } else {
                tracing::info!("  msg[{i}] [{role_str}] (0字)");
            }

            // 打印 tool_use 和 tool_result 块（完整，不截断）
            for (bi, block) in msg.content.iter().enumerate() {
                match block {
                    brain_llm::ContentBlock::ToolUse { name, input, .. } => {
                        let input_str = serde_json::to_string(input).unwrap_or_default();
                        tracing::info!("  msg[{i}].block[{bi}] [tool_use] {name}: {input_str}");
                    }
                    brain_llm::ContentBlock::ToolResult {
                        content, is_error, ..
                    } => {
                        tracing::info!(
                            "  msg[{i}].block[{bi}] [tool_result{}] {content}",
                            if *is_error { " ERROR" } else { "" },
                        );
                    }
                    brain_llm::ContentBlock::Text { .. } => {} // 已在上方打印
                    brain_llm::ContentBlock::Thinking { .. } => {}
                }
            }
        }

        // === 调用 LLM ===
        let response = match llm.complete(request).await {
            Ok(resp) => resp,
            Err(e) => {
                let err_msg = format!("{e}");
                tracing::error!("=== LLM 调用失败 [第{llm_calls}次] === 错误: {err_msg}");
                return Err(MainBrainError::LlmError(err_msg));
            }
        };

        // 累计 prompt_tokens
        total_prompt_tokens += response.usage.prompt_tokens;

        // === 日志：LLM 响应 ===
        let resp_text = response.text();
        let tool_calls = response.tool_calls();

        // 将 LLM 的文本推理通过 TextDelta 发送到 TUI（让用户看到思考过程）
        // 遍历所有 content blocks，Text 和 Thinking 都发送
        for block in &response.content {
            match block {
                ContentBlock::Text { text } if !text.is_empty() => {
                    send_progress(
                        progress_tx,
                        ProgressEvent::TextDelta { text: text.clone() },
                    )
                    .await;
                }
                ContentBlock::Thinking { content } if !content.is_empty() => {
                    // 用 <thinklh> 标签包裹，TUI 的 strip_thinking_tags 会检测并分离
                    send_progress(
                        progress_tx,
                        ProgressEvent::TextDelta { text: format!("<thinklh>{content}</thinklh>") },
                    )
                    .await;
                }
                _ => {}
            }
        }
        tracing::info!(
            "=== LLM 响应 [第{llm_calls}次] === text({}字){}{}",
            resp_text.chars().count(),
            if resp_text.is_empty() {
                String::new()
            } else {
                format!(": {resp_text}")
            },
            if tool_calls.is_empty() {
                String::new()
            } else {
                format!(", tool_calls={}", tool_calls.len())
            },
        );
        for (i, block) in response.content.iter().enumerate() {
            if let brain_llm::ContentBlock::ToolUse { name, input, .. } = block {
                let input_str = serde_json::to_string(input).unwrap_or_default();
                tracing::info!("  resp.block[{i}] [tool_use] {name}: {input_str}");
            }
        }

        if !response.has_tool_calls() {
            // 记录最终 assistant 回复
            let text = response.text();
            if !text.is_empty() {
                turns.push(TurnRecord {
                    role: TurnRole::Assistant,
                    content: text,
                    tool_call: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });
            }
            return Ok(ToolLoopResult {
                response,
                llm_calls,
                turns,
                total_prompt_tokens,
            });
        }

        // 记录本轮 assistant 回复（含工具调用的中间轮次）
        let assistant_text = response.text();
        if !assistant_text.is_empty() {
            turns.push(TurnRecord {
                role: TurnRole::Assistant,
                content: assistant_text,
                tool_call: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
            });
        }

        messages.push(ChatMessage::assistant_blocks(response.content.clone()));
        execute_tool_calls(
            tool_executor,
            &response,
            messages,
            progress_tx,
            hook_runner,
            &mut turns,
        )
        .await;
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
    hook_runner: Option<&HookRunner>,
    turns: &mut Vec<TurnRecord>,
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
                let deny_msg = format!("安全拒绝: {reason}");
                tracing::warn!("工具 {name} 被安全检查拒绝: {reason}");
                messages.push(ChatMessage::tool_result(id, &deny_msg, true));
                turns.push(TurnRecord {
                    role: TurnRole::ToolCall,
                    content: String::new(),
                    tool_call: Some(ToolCallRecord {
                        tool_name: name.clone(),
                        input: input.clone(),
                        output: deny_msg,
                        duration_ms: 0,
                        is_error: true,
                    }),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });
                send_progress(
                    progress_tx,
                    ProgressEvent::ToolDone {
                        brain: "main".into(),
                        tool_name: name.clone(),
                        duration_ms: 0,
                        output_preview: format!("安全拒绝: {reason}"),
                        is_error: true,
                    },
                )
                .await;
                continue;
            }

            // === PreToolUse hook ===
            if let Some(runner) = hook_runner {
                let hook_input = HookInput {
                    event: HookEvent::PreToolUse,
                    session_id: String::new(),
                    cwd: std::env::current_dir().unwrap_or_default(),
                    tool_name: Some(name.clone()),
                    tool_input: Some(serde_json::to_string(input).unwrap_or_default()),
                    tool_output: None,
                    is_error: false,
                    user_input: None,
                    ai_output: None,
                };
                let hook_outputs = runner.run(&hook_input).await;
                if hook_outputs
                    .iter()
                    .any(|o| o.decision == HookDecision::Deny)
                {
                    let reason = hook_outputs
                        .iter()
                        .find_map(|o| o.reason.clone())
                        .unwrap_or_else(|| "PreToolUse hook denied".into());
                    let deny_msg = format!("Hook 拒绝: {reason}");
                    tracing::warn!("工具 {name} 被 hook 拒绝: {reason}");
                    messages.push(ChatMessage::tool_result(id, &deny_msg, true));
                    turns.push(TurnRecord {
                        role: TurnRole::ToolCall,
                        content: String::new(),
                        tool_call: Some(ToolCallRecord {
                            tool_name: name.clone(),
                            input: input.clone(),
                            output: deny_msg,
                            duration_ms: 0,
                            is_error: true,
                        }),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                    });
                    send_progress(
                        progress_tx,
                        ProgressEvent::ToolDone {
                            brain: "main".into(),
                            tool_name: name.clone(),
                            duration_ms: 0,
                            output_preview: format!("Hook 拒绝: {reason}"),
                            is_error: true,
                        },
                    )
                    .await;
                    continue;
                }
            }
            // === PreToolUse hook 结束 ===

            let input_str = serde_json::to_string(input).unwrap_or_default();
            send_progress(
                progress_tx,
                ProgressEvent::ToolStart {
                    brain: "main".into(),
                    tool_name: name.clone(),
                    input: input_str,
                },
            )
            .await;

            // 执行工具
            let start = Instant::now();
            let result: ToolExecutionResult = tool_executor.execute(&tool_call).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            // 工具结果完整记录到日志（不截断）
            tracing::info!(
                "工具执行完成: {name} ({}ms){} | 输出: {}",
                duration_ms,
                if result.is_error { " [ERROR]" } else { "" },
                result.output,
            );

            send_progress(
                progress_tx,
                ProgressEvent::ToolDone {
                    brain: "main".into(),
                    tool_name: name.clone(),
                    duration_ms,
                    output_preview: result.output.clone(),
                    is_error: result.is_error,
                },
            )
            .await;

            messages.push(ChatMessage::tool_result(
                id,
                result.output.clone(),
                result.is_error,
            ));

            // 记录工具调用轨迹
            turns.push(TurnRecord {
                role: TurnRole::ToolCall,
                content: String::new(),
                tool_call: Some(ToolCallRecord {
                    tool_name: name.clone(),
                    input: input.clone(),
                    output: result.output,
                    duration_ms,
                    is_error: result.is_error,
                }),
                timestamp: chrono::Utc::now().to_rfc3339(),
            });

            // === PostToolUse hook ===
            if let Some(runner) = hook_runner {
                let hook_input = HookInput {
                    event: HookEvent::PostToolUse,
                    session_id: String::new(),
                    cwd: std::env::current_dir().unwrap_or_default(),
                    tool_name: Some(name.clone()),
                    tool_input: Some(serde_json::to_string(input).unwrap_or_default()),
                    tool_output: Some(turns.last().map_or(String::new(), |t| {
                        t.tool_call
                            .as_ref()
                            .map_or(String::new(), |tc| tc.output.chars().take(200).collect())
                    })),
                    is_error: result.is_error,
                    user_input: None,
                    ai_output: None,
                };
                let _ = runner.run(&hook_input).await;
            }
            // === PostToolUse hook 结束 ===
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

        let result = run_tool_loop(&llm, &executor, &mut messages, &[], None, None)
            .await
            .unwrap();

        assert_eq!(result.response.text(), "最终回答");
        assert_eq!(result.llm_calls, 1);
    }
}
