use std::time::{Duration, Instant};

use brain_core::guard_check::{guard_check, GuardResult};
use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{
    ProgressEvent, ToolCall, ToolCallRecord, ToolExecutionResult, TurnRecord, TurnRole,
    UserResponseSender,
};
use brain_hooks::runner::HookRunner;
use brain_hooks::types::{HookDecision, HookEvent, HookInput};
use brain_llm::types::TokenUsage;
use brain_llm::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmProvider, ToolDefinition,
};

use crate::error::{MainBrainError, Result};

/// 截断工具输出用于日志（避免日志文件爆炸）
fn truncate_tool_output_for_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max).collect();
    format!("{truncated}... [共{}字, 截断显示]", s.chars().count())
}

/// 估算输入 token 数（当 API 不返回 usage 信息时使用）
///
/// 中文通常 1 字 ≈ 1.5 token，英文约 4 字符 ≈ 1 token。
/// 混合场景下用 chars * 3 / 4 作为折中估算。
fn estimate_input_tokens(messages: &[ChatMessage]) -> u64 {
    messages
        .iter()
        .map(|m| {
            let mut chars = 0usize;
            // 统计文本内容
            chars += m.text_content().chars().count();
            // 统计工具调用的 JSON 输入
            for block in &m.content {
                if let ContentBlock::ToolUse { input, .. } = block {
                    chars += serde_json::to_string(input)
                        .unwrap_or_default()
                        .chars()
                        .count();
                }
                if let ContentBlock::ToolResult { content, .. } = block {
                    chars += content.chars().count();
                }
            }
            // 中文友好估算：每字符约 0.75 token
            (chars * 3 / 4) as u64
        })
        .sum()
}

/// 估算输出 token 数（当 API 不返回 usage 信息时使用）
fn estimate_output_tokens(response: &ChatResponse) -> u64 {
    let mut chars = 0usize;
    for block in &response.content {
        match block {
            ContentBlock::Text { text } => chars += text.chars().count(),
            ContentBlock::Thinking { content } => chars += content.chars().count(),
            ContentBlock::ToolUse { input, .. } => {
                chars += serde_json::to_string(input)
                    .unwrap_or_default()
                    .chars()
                    .count();
            }
            _ => {}
        }
    }
    // 中文友好估算：每字符约 0.75 token
    (chars * 3 / 4) as u64
}

/// tool_loop 返回结果
pub(crate) struct ToolLoopResult {
    /// LLM 最终文本响应
    pub(crate) response: ChatResponse,
    /// LLM 调用总次数（含多轮工具调用）
    pub(crate) llm_calls: u32,
    /// 完整对话轨迹（每轮 assistant 回复 + 工具调用 + 工具结果）
    pub(crate) turns: Vec<TurnRecord>,
    /// 累计 prompt_tokens（从 LLM 返回的 usage 中累加，用于计费）
    pub(crate) total_prompt_tokens: u64,
    /// 最后一次 LLM 调用的 prompt_tokens（用于上下文使用率计算）
    pub(crate) last_prompt_tokens: u64,
    /// 每次 LLM 调用的详细 usage 信息
    pub(crate) usage_records: Vec<brain_llm::types::TokenUsage>,
    /// 上下文溢出标记：tool_loop 因上下文过大而中断，调用方应压缩后重入
    pub(crate) context_overflow: bool,
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
        None,
    )
    .await
}

/// 带配置参数的 tool_loop
///
/// `cancel` — 可选的取消令牌，调用 `cancel()` 后 tool_loop 会在下一轮 LLM 调用前退出，
/// 返回已执行的部分结果（已完成的工具调用和 LLM 回复不会丢失）。
pub async fn run_tool_loop_with_config(
    llm: &dyn LlmProvider,
    tool_executor: &dyn ToolExecutor,
    messages: &mut Vec<ChatMessage>,
    tools: &[ToolDefinition],
    progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
    hook_runner: Option<&HookRunner>,
    max_tokens: u32,
    temperature: f64,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<ToolLoopResult> {
    let mut llm_calls = 0u32;
    let mut turns: Vec<TurnRecord> = Vec::new();
    let mut total_prompt_tokens = 0u64;
    let mut last_prompt_tokens = 0u64; // 在循环内赋值
    let mut usage_records: Vec<brain_llm::types::TokenUsage> = Vec::new();
    // 记录最后一次 LLM 响应，用于取消时构建结果
    let mut last_response: Option<ChatResponse> = None;

    loop {
        // ── 协作取消检测：在 LLM 调用前检查，避免浪费 API 调用 ──
        if let Some(ref cancel) = cancel {
            if cancel.is_cancelled() {
                tracing::info!(
                    "tool_loop 收到取消信号，返回已执行结果（{} 轮 LLM 调用）",
                    llm_calls
                );
                // 如果已有部分 LLM 响应，构建结果返回
                if let Some(response) = last_response.take() {
                    return Ok(ToolLoopResult {
                        response,
                        llm_calls,
                        turns,
                        total_prompt_tokens,
                        last_prompt_tokens,
                        usage_records,
                        context_overflow: false,
                    });
                }
                // 还没有任何 LLM 响应，返回错误
                return Err(MainBrainError::LlmError("查询被用户取消".into()));
            }
        }
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

        // === 调用 LLM（带超时保护，防止 API 无响应永久挂起） ===
        // 重试已在 brain-llm 的 complete() 内部实现（指数退避），此处超时为兜底保护
        let llm_timeout = Duration::from_secs(360);
        let response = match tokio::time::timeout(llm_timeout, llm.complete(request)).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                let err_msg = format!("{e}");
                tracing::error!("=== LLM 调用失败 [第{llm_calls}次] === 错误: {err_msg}");
                return Err(MainBrainError::LlmError(err_msg));
            }
            Err(_) => {
                tracing::error!("=== LLM 调用超时 [第{llm_calls}次] === 等待超过360秒（含重试）");
                return Err(MainBrainError::LlmError(
                    "LLM 请求超时（360秒，含重试），请检查网络连接或模型服务状态".into(),
                ));
            }
        };

        // 处理 usage 信息：如果 API 不返回（如小米 mimo），使用估算值
        let estimated_input = estimate_input_tokens(&messages);
        let estimated_output = estimate_output_tokens(&response);

        // 如果 API 返回的 prompt_tokens 为 0，使用估算值
        let prompt_tokens = if response.usage.prompt_tokens > 0 {
            response.usage.prompt_tokens
        } else {
            tracing::info!("API 未返回 prompt_tokens，使用估算值: {}", estimated_input);
            estimated_input
        };

        // 如果 API 返回的 completion_tokens 为 0，使用估算值
        let completion_tokens = if response.usage.completion_tokens > 0 {
            response.usage.completion_tokens
        } else {
            tracing::info!(
                "API 未返回 completion_tokens，使用估算值: {}",
                estimated_output
            );
            estimated_output
        };

        // 构建修正后的 usage（用于计费和显示）
        let corrected_usage = TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
            cache_read_input_tokens: response.usage.cache_read_input_tokens,
        };

        // 累计 prompt_tokens
        last_prompt_tokens = prompt_tokens;
        total_prompt_tokens += last_prompt_tokens;

        // 记录本次 LLM 调用的 usage 信息（使用修正后的值）
        usage_records.push(corrected_usage.clone());

        // 日志：显示 usage 来源
        if response.usage.prompt_tokens > 0 {
            tracing::info!(
                "=== LLM Usage (API 返回) === prompt={}, completion={}, total={}",
                prompt_tokens,
                completion_tokens,
                corrected_usage.total_tokens
            );
        } else {
            tracing::info!(
                "=== LLM Usage (估算) === prompt={}, completion={}, total={}",
                prompt_tokens,
                completion_tokens,
                corrected_usage.total_tokens
            );
        }

        // === 日志：LLM 响应 ===
        let resp_text = response.text();
        let tool_calls = response.tool_calls();

        // 保存最后一次 LLM 响应，用于取消时构建部分结果
        last_response = Some(response.clone());

        // 将 LLM 的文本推理通过 TextDelta 发送到 TUI（让用户看到思考过程）
        // 遍历所有 content blocks，Text 和 Thinking 都发送
        for block in &response.content {
            match block {
                ContentBlock::Text { text } if !text.is_empty() => {
                    send_progress(progress_tx, ProgressEvent::TextDelta { text: text.clone() })
                        .await;
                }
                ContentBlock::Thinking { content } if !content.is_empty() => {
                    // 走专用 ThinkingDelta 通道，TUI 默认不显示，Ctrl+E 切换
                    send_progress(
                        progress_tx,
                        ProgressEvent::ThinkingDelta {
                            content: content.clone(),
                        },
                    )
                    .await;
                }
                _ => {}
            }
        }
        tracing::info!(
            "=== LLM 响应 [第{llm_calls}次] === text({}字){}{} finish_reason={:?}",
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
            response.finish_reason,
        );

        // 空响应检测：无文本 + 无工具调用 → 可能是上下文超限或模型异常
        if resp_text.is_empty() && tool_calls.is_empty() {
            let reason = match response.finish_reason {
                Some(FinishReason::MaxTokens) => {
                    "模型因上下文长度限制截断，返回了空响应。请尝试缩短对话或开启压缩。"
                }
                Some(FinishReason::ToolUse) => {
                    "模型返回了工具调用标记但无实际内容（API 响应格式异常）。"
                }
                _ => "模型返回了空响应（无文本无工具调用），可能是上下文过长或模型服务异常。",
            };
            tracing::warn!("LLM 空响应警告: {reason}");
            send_progress(
                progress_tx,
                ProgressEvent::TextDelta {
                    text: format!("\n⚠ {reason}\n"),
                },
            )
            .await;
        }
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
                last_prompt_tokens,
                usage_records,
                context_overflow: false,
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

        // === 上下文溢出检测：每次 LLM 返回后检查 ===
        let health = check_context_health(messages);
        match health {
            ContextHealth::DangerFull => {
                // 超过 80%：中断 tool_loop，让 main_brain 执行整体压缩后重入
                tracing::warn!(
                    "⚠ 上下文溢出检测 [第{llm_calls}次] 超过80%阈值，中断 tool_loop 等待压缩"
                );
                return Ok(ToolLoopResult {
                    response: last_response.clone().unwrap_or(response),
                    llm_calls,
                    turns,
                    total_prompt_tokens,
                    last_prompt_tokens,
                    usage_records,
                    context_overflow: true,
                });
            }
            ContextHealth::WarningToolResults => {
                // 超过 60%：记录警告，tool_loop 结束后 main_brain 会触发后台工具结果压缩
                tracing::info!(
                    "上下文使用率超过60% [第{llm_calls}次]，待 tool_loop 结束后触发工具结果压缩"
                );
            }
            ContextHealth::Healthy => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 上下文健康检测（公共函数）
// ---------------------------------------------------------------------------

/// 上下文窗口 token 上限（与 ConversationHistory 的 max_context_tokens 对齐）
const CONTEXT_MAX_TOKENS: usize = 131_072;

/// 上下文健康状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextHealth {
    /// 正常（< 60%）
    Healthy,
    /// 超过 60%，建议压缩工具结果
    WarningToolResults,
    /// 超过 80%，需要整体压缩
    DangerFull,
}

/// 公共上下文健康检测函数
///
/// 每次 LLM 返回响应后调用，估算当前 messages 的 token 使用率，
/// 返回应采取的行动建议。
///
/// 这个函数可以被任何使用 LLM 的地方调用，不限于 tool_loop。
pub fn check_context_health(messages: &[ChatMessage]) -> ContextHealth {
    let estimated_tokens: usize = messages
        .iter()
        .map(|m| {
            let chars = m.text_content().chars().count();
            // 中文友好估算：每字符约 0.75 token
            chars * 3 / 4
        })
        .sum();

    #[allow(clippy::cast_precision_loss)]
    let usage_ratio = estimated_tokens as f64 / CONTEXT_MAX_TOKENS as f64;

    if usage_ratio >= 0.80 {
        tracing::warn!(
            "上下文健康检测: 估算 {} tokens, 使用率 {:.0}%, 状态=DangerFull",
            estimated_tokens,
            usage_ratio * 100.0
        );
        ContextHealth::DangerFull
    } else if usage_ratio >= 0.60 {
        tracing::info!(
            "上下文健康检测: 估算 {} tokens, 使用率 {:.0}%, 状态=WarningToolResults",
            estimated_tokens,
            usage_ratio * 100.0
        );
        ContextHealth::WarningToolResults
    } else {
        ContextHealth::Healthy
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

            // === AskUserQuestion 特殊处理：阻塞等待用户响应 ===
            if name == "AskUserQuestion" {
                let question = input
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let options: Option<Vec<String>> =
                    input.get("options").and_then(|v| v.as_array()).map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    });
                let multi_select = input
                    .get("multiSelect")
                    .or_else(|| input.get("multi_select"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let (response_tx, response_rx) = tokio::sync::oneshot::channel();

                send_progress(
                    progress_tx,
                    ProgressEvent::AskUser {
                        question: question.clone(),
                        options: options.clone(),
                        multi_select,
                        response_tx: UserResponseSender(response_tx),
                    },
                )
                .await;

                // 阻塞等待用户响应
                let user_response = response_rx
                    .await
                    .unwrap_or_else(|_| "用户未响应".to_string());

                let duration_ms = 0u64;
                messages.push(ChatMessage::tool_result(id, &user_response, false));
                turns.push(TurnRecord {
                    role: TurnRole::ToolCall,
                    content: String::new(),
                    tool_call: Some(ToolCallRecord {
                        tool_name: name.clone(),
                        input: input.clone(),
                        output: user_response.clone(),
                        duration_ms,
                        is_error: false,
                    }),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });
                send_progress(
                    progress_tx,
                    ProgressEvent::ToolDone {
                        brain: "main".into(),
                        tool_name: name.clone(),
                        duration_ms,
                        output_preview: user_response,
                        is_error: false,
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
                truncate_tool_output_for_log(&result.output, 1000),
            );

            // 工具输出截断：超过 50K 字符的内容截断后写入对话历史
            // 防止 grep_search 等工具返回巨大结果撑爆上下文窗口
            const MAX_TOOL_OUTPUT_CHARS: usize = 50_000;
            let output_for_llm = if result.output.chars().count() > MAX_TOOL_OUTPUT_CHARS {
                let truncated_chars = result.output.chars().count();
                let kept: String = result.output.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
                tracing::warn!(
                    "工具 {name} 输出过大({}字)，截断至 {} 字符",
                    truncated_chars,
                    MAX_TOOL_OUTPUT_CHARS,
                );
                format!(
                    "{kept}\n\n[⚠ 输出已截断：原始 {} 字符，保留前 {} 字符。请使用更精确的搜索条件或 head_limit 参数]",
                    truncated_chars, MAX_TOOL_OUTPUT_CHARS,
                )
            } else {
                result.output.clone()
            };

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
                output_for_llm.clone(),
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
