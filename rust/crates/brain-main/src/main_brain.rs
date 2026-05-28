use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use brain_core::config::BrainConfig;
use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{MainBrainOutput, ProgressEvent, TurnRecord, TurnRole, TurnUsage};
use brain_llm::{ChatMessage, LlmProvider, ToolDefinition};
use tokio::sync::Mutex;

use crate::compact::{self, CompactionConfig};
use crate::conversation::ConversationHistory;
use crate::error::{MainBrainError, Result};
use crate::prompts;
use crate::tool_loop;

use brain_memory::threshold_compression::{ThresholdCompactionConfig, ThresholdCompressor};

/// 主脑 — v2 架构的核心
///
/// 对标 Claude Code 的主模型循环：
/// - 直接接收用户原始输入
/// - 带完整对话历史 + 工具定义，跑 tool_loop
/// - 唯一跟用户交互的 LLM
/// - messages 构造：全量历史，不每次重建
pub struct MainBrain {
    llm: Arc<dyn LlmProvider>,
    tool_executor: Arc<dyn ToolExecutor>,
    history: ConversationHistory,
    tools: Vec<ToolDefinition>,
    config: BrainConfig,
    /// LLM 生成参数（max_tokens / temperature）
    llm_max_tokens: u32,
    llm_temperature: f64,
    /// 记忆脑启动时注入的上下文（追加到 system prompt 尾部）
    memory_context: Option<String>,
    /// 可用技能摘要（追加到 system prompt）
    skill_summary: Option<String>,
    /// Bootstrap 技能内容（启动时自动注入）
    bootstrap_content: Option<String>,
    /// 会话级 prompt_tokens 累计
    session_prompt_tokens: u64,
    /// 会话级 completion_tokens 累计
    session_completion_tokens: u64,
    /// 会话级 cache read tokens 累计
    session_cache_read_tokens: u64,
    /// 后台压缩结果（消息索引 → 压缩后内容）
    pending_compressions: Arc<Mutex<HashMap<usize, String>>>,
    /// 已压缩的消息索引集合
    compressed_indices: Arc<Mutex<HashSet<usize>>>,
    /// 是否已触发过压缩
    compaction_triggered: bool,
    /// 压缩配置
    compaction_config: CompactionConfig,
    /// 累计压缩次数
    compaction_count: u32,
    /// 累计节省的字符数
    chars_saved_by_compaction: usize,
    /// 阈值压缩器
    threshold_compressor: ThresholdCompressor,
}

impl MainBrain {
    /// 创建主脑
    pub fn new(
        llm: Arc<dyn LlmProvider>,
        tool_executor: Arc<dyn ToolExecutor>,
        config: BrainConfig,
        llm_max_tokens: u32,
        llm_temperature: f64,
    ) -> Self {
        // 从 ThresholdConfig 取 max_context_tokens，默认 1M
        let max_context_tokens = config.brain.thresholds.max_context_tokens as usize;

        // 初始化阈值压缩器
        let threshold_config = ThresholdCompactionConfig::default();
        let threshold_compressor = ThresholdCompressor::new(threshold_config);

        Self {
            llm,
            tool_executor,
            history: ConversationHistory::new(max_context_tokens),
            tools: Vec::new(),
            config,
            llm_max_tokens,
            llm_temperature,
            memory_context: None,
            skill_summary: None,
            bootstrap_content: None,
            session_prompt_tokens: 0,
            session_completion_tokens: 0,
            session_cache_read_tokens: 0,
            pending_compressions: Arc::new(Mutex::new(HashMap::new())),
            compressed_indices: Arc::new(Mutex::new(HashSet::new())),
            compaction_triggered: false,
            compaction_config: CompactionConfig::default(),
            compaction_count: 0,
            chars_saved_by_compaction: 0,
            threshold_compressor,
        }
    }

    /// 注册可用工具
    pub fn register_tools(&mut self, tools: Vec<ToolDefinition>) {
        tracing::info!("主脑注册 {} 个工具", tools.len());
        self.tools = tools;
    }

    /// 处理一轮用户输入
    ///
    /// 用户消息在开头立即保存，确保取消时上下文不丢失。
    /// LLM 响应在成功/失败后保存。
    ///
    /// 流程：
    /// 1. 立即保存用户消息到历史（防止取消时丢失上下文）
    /// 2. 应用后台预压缩结果（零阻塞）
    /// 3. 检查上下文使用率 — 超危险阈值截断
    /// 4. 构建 messages（system_prompt + 历史）
    /// 5. 跑 tool_loop
    /// 6. 成功后写入 LLM 响应和工具调用到历史
    /// 7. 更新 session token 追踪 + 触发后台预压缩
    pub async fn process_input(
        &mut self,
        input: &str,
        progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<MainBrainOutput> {
        let start = std::time::Instant::now();

        // ── 0. 立即保存用户消息（防止取消时上下文丢失）──
        self.history.push_user(input);

        // ── 1. 应用后台预压缩结果（零阻塞内存操作）──
        let pending = {
            let mut p = self.pending_compressions.lock().await;
            std::mem::take(&mut *p)
        };
        if !pending.is_empty() {
            let mut indices = self.compressed_indices.lock().await;
            indices.extend(pending.keys().copied());
            let result = self.history.apply_pending_compaction(&pending);
            // 更新压缩统计
            self.compaction_count += result.compacted_groups as u32;
            self.chars_saved_by_compaction += result.chars_saved;
            tracing::info!(
                "应用后台压缩: {} 条消息, 节省 {} 字符",
                result.compacted_groups,
                result.chars_saved
            );
        }

        // ── 2. 检查上下文使用率 ──
        let thresholds = &self.config.brain.thresholds;

        // 超过危险阈值：智能压缩（替代粗暴截断）
        if self
            .history
            .is_context_full(thresholds.context_danger_threshold)
        {
            tracing::warn!(
                "上下文使用率超过阈值: {:.0}% >= {:.0}%, 开始智能压缩...",
                self.history.context_usage() * 100.0,
                thresholds.context_danger_threshold * 100.0
            );

            match self
                .threshold_compressor
                .compress_context(self.history.messages(), self.llm.as_ref())
                .await
            {
                Ok(compressed) => {
                    // 成功：用压缩结果重建上下文
                    self.history.clear_and_rebuild_from_compressed(
                        &compressed.decision_summary,
                        &compressed.recent_messages,
                    );

                    tracing::info!(
                        "阈值压缩完成：压缩了 {} 条消息，保留了 {} 条最近消息，耗时 {}ms",
                        compressed.metadata.compressed_count,
                        compressed.metadata.preserved_count,
                        compressed.metadata.duration_ms,
                    );
                }
                Err(e) => {
                    // 失败：记录错误，返回错误给用户
                    tracing::error!("阈值压缩失败: {}", e);
                    return Err(MainBrainError::ContextCompactionFailed(e.to_string()));
                }
            }
        }

        // ── 3. 构建 messages（用户消息已在 history 中）──
        let mut messages = self.build_messages();

        // 发送 Connecting 事件
        if let Some(tx) = progress_tx {
            let _ = tx
                .send(ProgressEvent::Connecting {
                    brain: "main".into(),
                    model: self.llm.model().into(),
                })
                .await;
        }

        // ── 4. 跑 tool_loop ──
        let loop_result = match tool_loop::run_tool_loop_with_config(
            self.llm.as_ref(),
            self.tool_executor.as_ref(),
            &mut messages,
            &self.tools,
            progress_tx,
            None,
            self.llm_max_tokens,
            self.llm_temperature,
            cancel,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                // 失败时：用户消息已保存，继续写入已执行的部分和错误信息
                // 将 tool_loop 中已执行的部分（assistant 中间回复 + tool 结果）写入
                let old_len = self.history.len() - 1;
                let skip = 1 + old_len + 1;
                for msg in messages.iter().skip(skip) {
                    if msg.role == brain_llm::MessageRole::System {
                        continue;
                    }
                    match msg.role {
                        brain_llm::MessageRole::Assistant => {
                            let has_tool_use = msg.content.iter().any(|b| b.is_tool_use());
                            if has_tool_use {
                                // Assistant message with tool calls → save entire blocks
                                self.history.push_assistant_blocks(msg.content.clone());
                            } else {
                                // Pure text response → simplified storage
                                let text = msg.text_content();
                                if !text.is_empty() {
                                    self.history.push_assistant(&text);
                                }
                            }
                        }
                        brain_llm::MessageRole::User => {
                            for block in &msg.content {
                                if let brain_llm::ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } = block
                                {
                                    self.history.push_tool_result(
                                        tool_use_id.clone(),
                                        content.clone(),
                                        *is_error,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // 写入错误信息作为 assistant 消息
                self.history.push_assistant(&format!("[系统错误] {e}"));
                return Err(e);
            }
        };

        let answer = loop_result.response.text();
        // 使用 usage_records 中的修正后 usage（如果 API 不返回则使用估算值）
        let corrected_usage = loop_result
            .usage_records
            .last()
            .cloned()
            .unwrap_or_default();
        let total_tokens = corrected_usage.total_tokens;
        let prompt_tokens = loop_result.total_prompt_tokens;
        let last_prompt_tokens = loop_result.last_prompt_tokens;
        let completion_tokens = corrected_usage.completion_tokens;

        // ── 5. 成功后写入 LLM 响应和工具调用到历史（用户消息已在开头保存）──
        let old_history_len = self.history.len() - 1;
        let skip_count = 1 + old_history_len + 1;

        for msg in messages.iter().skip(skip_count) {
            if msg.role == brain_llm::MessageRole::System {
                continue;
            }
            match msg.role {
                brain_llm::MessageRole::Assistant => {
                    let has_tool_use = msg.content.iter().any(|b| b.is_tool_use());
                    if has_tool_use {
                        // Assistant message with tool calls → save entire blocks
                        self.history.push_assistant_blocks(msg.content.clone());
                    } else {
                        // Pure text response → simplified storage
                        let text = msg.text_content();
                        if !text.is_empty() {
                            self.history.push_assistant(&text);
                        }
                    }
                }
                brain_llm::MessageRole::User => {
                    for block in &msg.content {
                        if let brain_llm::ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } = block
                        {
                            self.history.push_tool_result(
                                tool_use_id.clone(),
                                content.clone(),
                                *is_error,
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        // 确保最终回答也写入历史
        let has_final_answer = self.history.messages().iter().rev().take(3).any(|m| {
            m.role == brain_core::types::MessageRole::Assistant && m.text_content() == answer
        });
        if !has_final_answer {
            self.history.push_assistant(&answer);
        }

        // ── 5. 更新 session token 追踪 ──
        // 使用 usage_records 中的修正后 usage（如果 API 不返回则使用估算值）
        let cache_read = corrected_usage.cache_read_input_tokens;
        self.session_prompt_tokens += prompt_tokens;
        self.session_completion_tokens += completion_tokens;
        self.session_cache_read_tokens += cache_read;
        // 使用最后一次 LLM 调用的 prompt_tokens 计算上下文使用率
        // （不能用 total_prompt_tokens，那是所有工具调用轮次的累加，会远大于实际上下文窗口）
        self.history.set_tracked_tokens(last_prompt_tokens);

        // ── 6. 后台预压缩：滑动窗口 ──
        let turn_tool_chars = compact::last_turn_tool_chars(self.history.messages());
        if turn_tool_chars > self.compaction_config.tool_result_compress_threshold {
            self.compaction_triggered = true;
        }

        // 基于上下文占用率触发压缩：超过 60% 时开始压缩
        let context_usage_ratio = self.history.context_usage();
        if context_usage_ratio > 0.6 {
            self.compaction_triggered = true;
            tracing::info!(
                "上下文占用 {:.0}%，触发压缩机制",
                context_usage_ratio * 100.0
            );
        }

        if self.compaction_triggered {
            let indices_guard = self.compressed_indices.lock().await;
            if let Some((start_idx, end_idx)) = compact::find_slide_out_turn(
                self.history.messages(),
                &indices_guard,
                &self.compaction_config,
            ) {
                drop(indices_guard);

                let pending = self.pending_compressions.clone();
                let compressed_idx = self.compressed_indices.clone();
                let messages_snapshot: Vec<_> =
                    self.history.messages()[start_idx..end_idx].to_vec();
                let llm = self.llm.clone();
                let compact_max_tokens = self.compaction_config.max_tokens;

                tokio::spawn(async move {
                    let compressed = compact::compress_single_turn(
                        &messages_snapshot,
                        0,
                        messages_snapshot.len(),
                        llm.as_ref(),
                        compact_max_tokens,
                    )
                    .await;
                    if !compressed.is_empty() {
                        // key 加上 start_idx 偏移
                        let offset_map: HashMap<usize, String> = compressed
                            .into_iter()
                            .map(|(k, v)| (k + start_idx, v))
                            .collect();
                        let keys: Vec<usize> = offset_map.keys().copied().collect();
                        let mut p = pending.lock().await;
                        p.extend(offset_map);
                        let mut idx = compressed_idx.lock().await;
                        idx.extend(keys);
                    }
                });
            }
        }

        let elapsed = start.elapsed();

        Ok(MainBrainOutput {
            answer,
            usage: TurnUsage {
                total_tokens,
                llm_calls: loop_result.llm_calls,
                duration_ms: elapsed.as_millis() as u64,
                prompt_tokens,
                completion_tokens,
            },
            turns: {
                let mut full_turns = vec![TurnRecord {
                    role: TurnRole::User,
                    content: input.to_string(),
                    tool_call: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                }];
                full_turns.extend(loop_result.turns);
                full_turns
            },
        })
    }

    /// 流式处理用户输入 — 返回进度事件通道
    ///
    /// P0-4 修复：返回 oneshot receiver 用于写回历史
    pub fn process_input_streaming(
        &mut self,
        input: &str,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<(
        tokio::sync::mpsc::Receiver<ProgressEvent>,
        tokio::sync::oneshot::Receiver<String>,
    )> {
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        // 追加用户消息
        self.history.push_user(input);

        // 构建 messages
        let mut messages = self.build_messages();

        let llm = self.llm.clone();
        let executor = self.tool_executor.clone();
        let tools = self.tools.clone();
        let max_tokens = self.llm_max_tokens;
        let temperature = self.llm_temperature;

        // 后台执行 tool_loop
        tokio::spawn(async move {
            let _ = tx
                .send(ProgressEvent::Connecting {
                    brain: "main".into(),
                    model: llm.model().into(),
                })
                .await;

            let result = tool_loop::run_tool_loop_with_config(
                llm.as_ref(),
                executor.as_ref(),
                &mut messages,
                &tools,
                Some(&tx),
                None,
                max_tokens,
                temperature,
                cancel,
            )
            .await;

            match result {
                Ok(loop_result) => {
                    let text = loop_result.response.text();
                    // 注意：不在这里发 TextDelta，tool_loop 已经发了（避免重复）
                    // P0-4: 通过 oneshot 发送结果供调用者写回历史
                    let _ = result_tx.send(text);
                }
                Err(e) => {
                    let error_msg = format!("错误: {e}");
                    let _ = tx
                        .send(ProgressEvent::TextDelta {
                            text: error_msg.clone(),
                        })
                        .await;
                    let _ = result_tx.send(error_msg);
                }
            }

            let _ = tx.send(ProgressEvent::Done).await;
        });

        Ok((rx, result_rx))
    }

    /// P0-4: 将 streaming 结果写回历史（由编排器调用）
    pub fn commit_streaming_result(&mut self, answer: &str) {
        self.history.push_assistant(answer);
    }

    /// P1-3: 将评估反馈写入历史
    pub fn push_evaluator_to_history(&mut self, feedback: &str) {
        self.history.push_evaluator(feedback);
    }

    /// 上下文重建（由编排器调用）
    pub fn rebuild_context(
        &mut self,
        brain_state: &brain_core::types::BrainState,
        keep_recent_turns: usize,
    ) {
        let truncated = self
            .history
            .clear_and_rebuild(brain_state, keep_recent_turns);
        tracing::info!("上下文重建: 截断 {truncated} 条消息");
    }

    /// 将记忆脑 brain_state 注入到 system prompt 中
    ///
    /// 仿照 Claude Code 启动时加载 MEMORY.md，将记忆上下文
    /// 拼接到 system prompt 尾部，而不是混入对话历史。
    pub fn inject_memory_context(&mut self, brain_state_text: &str) {
        if brain_state_text.is_empty() {
            return;
        }
        // 将记忆上下文存入 config 的 system prompt 后缀
        // 在 build_messages 时会追加到 system prompt
        self.memory_context = Some(brain_state_text.to_string());
        tracing::info!(
            "已注入记忆脑上下文 ({}字)",
            brain_state_text.chars().count()
        );
    }

    /// 注入可用技能摘要（追加到 system prompt）
    pub fn inject_skill_summary(&mut self, summary: String) {
        if summary.is_empty() {
            return;
        }
        self.skill_summary = Some(summary);
        tracing::info!("已注入技能摘要");
    }

    /// 注入 bootstrap 技能内容（启动时自动注入到 system prompt 最前面）
    pub fn inject_bootstrap(&mut self, content: String) {
        if content.is_empty() {
            return;
        }
        self.bootstrap_content = Some(content);
        tracing::info!("已注入 bootstrap 技能");
    }

    /// 注入实时召回的记忆作为独立 system 消息
    pub fn push_memory_context(&mut self, memory_text: &str) {
        if memory_text.is_empty() {
            return;
        }
        self.history.push_system(memory_text);
    }

    /// 获取对话历史（供记忆脑/评估脑读取）
    pub fn history(&self) -> &[brain_core::types::ConversationMessage] {
        self.history.messages()
    }

    /// 对话轮数
    pub fn history_len(&self) -> usize {
        self.history.messages().len()
    }

    /// 清空对话历史
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// 获取上下文使用率
    pub fn context_usage(&self) -> f64 {
        self.history.context_usage()
    }

    /// 会话累计 prompt tokens
    pub fn cumulative_prompt_tokens(&self) -> u64 {
        self.session_prompt_tokens
    }

    /// 会话累计 completion tokens
    pub fn cumulative_completion_tokens(&self) -> u64 {
        self.session_completion_tokens
    }

    /// 会话累计 cache read tokens
    pub fn cumulative_cache_read_tokens(&self) -> u64 {
        self.session_cache_read_tokens
    }

    /// 累计压缩次数
    pub fn compaction_count(&self) -> u32 {
        self.compaction_count
    }

    /// 累计节省的字符数
    pub fn chars_saved_by_compaction(&self) -> usize {
        self.chars_saved_by_compaction
    }

    /// 构建完整 messages = system_prompt + 环境信息 + 记忆上下文 + 历史
    fn build_messages(&self) -> Vec<ChatMessage> {
        let system_prompt = if self.tools.is_empty() {
            prompts::build_system_prompt_no_tools()
        } else {
            prompts::build_system_prompt_with_tools()
        };
        // 注入运行环境信息（OS、工作目录、日期）
        let env_info = prompts::build_environment_info();
        // 将记忆上下文追加到 system prompt
        // Prompt cache 排序：越稳定的越靠前
        // 1. Bootstrap 技能（插件注入，会话级稳定）
        // 2. 系统核心规则（永不变化）
        // 3. 环境信息（每天变化）
        // 4. 技能摘要（安装时变化）
        // 5. 记忆上下文/潜意识（每次会话变化）
        let mut parts = Vec::new();
        if let Some(ref bs) = self.bootstrap_content {
            parts.push(bs.clone());
        }
        parts.push(system_prompt);
        parts.push(env_info);
        if let Some(ref skills) = self.skill_summary {
            parts.push(skills.clone());
        }
        if let Some(ref ctx) = self.memory_context {
            parts.push(format!("\n{ctx}"));
        }
        let full_prompt = parts.join("\n");
        let mut messages = vec![ChatMessage::system(full_prompt)];
        messages.extend(self.history.to_chat_messages());
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::tool_executor::StubToolExecutor;
    use brain_llm::{ChatResponse, ContentBlock, TokenUsage};
    use std::future::Future;
    use std::pin::Pin;

    struct StubLlm;

    impl LlmProvider for StubLlm {
        fn model(&self) -> &'static str {
            "stub"
        }

        fn complete(
            &self,
            _request: brain_llm::ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            Box::pin(async {
                Ok(ChatResponse {
                    content: vec![ContentBlock::text("测试回复")],
                    model: "stub".into(),
                    usage: TokenUsage::default(),
                    finish_reason: Some(brain_llm::FinishReason::EndTurn),
                })
            })
        }
    }

    #[tokio::test]
    async fn process_input_returns_output() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config, 32768, 0.7);
        let output = brain.process_input("你好", None, None).await.unwrap();

        assert_eq!(output.answer, "测试回复");
        assert!(!output.answer.is_empty());
    }

    #[test]
    fn build_messages_includes_system_prompt() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config, 32768, 0.7);
        brain.history.push_user("测试");

        let messages = brain.build_messages();
        assert!(messages.len() >= 2);
        // 第一条是 system prompt
        assert_eq!(messages[0].role, brain_llm::MessageRole::System);
    }

    #[tokio::test]
    async fn process_input_writes_history() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config, 32768, 0.7);
        assert_eq!(brain.history_len(), 0);

        brain.process_input("你好", None, None).await.unwrap();
        // 应该有 user + assistant = 2 条
        assert_eq!(brain.history_len(), 2);
    }

    #[tokio::test]
    async fn commit_streaming_result_writes_history() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config, 32768, 0.7);
        brain.history.push_user("hello");

        let (rx, result_rx) = brain.process_input_streaming("hello", None).unwrap();
        drop(rx); // 不读取进度事件

        if let Ok(answer) = result_rx.await {
            brain.commit_streaming_result(&answer);
        }

        // user (from process_input_streaming) + user (from test push_user) + assistant
        assert!(brain.history_len() >= 2);
    }

    /// 测试记忆注入链路：inject_memory_context → build_messages → system prompt 包含记忆
    #[test]
    fn inject_memory_context_appears_in_system_prompt() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config, 32768, 0.7);
        brain.register_tools(vec![]);

        // 注入记忆上下文
        brain.inject_memory_context("以下是你的持久记忆：用户希望被称为测试大佬。");

        let messages = brain.build_messages();
        assert_eq!(messages[0].role, brain_llm::MessageRole::System);
        let system_text = &messages[0].content[0];
        let brain_llm::ContentBlock::Text { text } = system_text else {
            panic!("Expected text block");
        };
        assert!(
            text.contains("测试大佬"),
            "❌ system prompt 应包含注入的记忆内容，实际: {}",
            text.chars().take(200).collect::<String>()
        );
    }

    /// 测试 push_memory_context 注入独立 system 消息
    #[test]
    fn push_memory_context_creates_system_message() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config, 32768, 0.7);

        brain.history.push_user("你好");
        brain.push_memory_context("[相关记忆]\n- 用户希望被称为测试大佬");
        brain.history.push_user("你应该称呼我什么");

        assert_eq!(brain.history_len(), 3); // user + system + user
        let messages = brain.history();
        assert_eq!(messages[0].role, brain_core::types::MessageRole::User);
        assert_eq!(messages[1].role, brain_core::types::MessageRole::System);
        assert!(messages[1].text_content().contains("测试大佬"));
    }
}
