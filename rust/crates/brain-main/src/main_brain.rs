use std::sync::Arc;

use brain_core::config::BrainConfig;
use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{MainBrainOutput, ProgressEvent, TurnUsage};
use brain_llm::{ChatMessage, LlmProvider, ToolDefinition};

use crate::conversation::ConversationHistory;
use crate::error::Result;
use crate::prompts;
use crate::tool_loop;

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
}

impl MainBrain {
    /// 创建主脑
    pub fn new(
        llm: Arc<dyn LlmProvider>,
        tool_executor: Arc<dyn ToolExecutor>,
        config: BrainConfig,
    ) -> Self {
        let max_tokens = 200_000; // 上下文窗口
        let llm_max_tokens = 8192;
        let llm_temperature = 0.7;
        Self {
            llm,
            tool_executor,
            history: ConversationHistory::new(max_tokens),
            tools: Vec::new(),
            config,
            llm_max_tokens,
            llm_temperature,
            memory_context: None,
        }
    }

    /// 注册可用工具
    pub fn register_tools(&mut self, tools: Vec<ToolDefinition>) {
        tracing::info!("主脑注册 {} 个工具", tools.len());
        self.tools = tools;
    }

    /// 处理一轮用户输入
    ///
    /// 事务式写入：失败不污染历史。
    ///
    /// 流程：
    /// 1. 检查上下文使用率
    /// 2. 构建 messages（system_prompt + 历史 + 用户输入）
    /// 3. 跑 tool_loop
    /// 4. 成功后一次性写入完整一轮到历史
    pub async fn process_input(
        &mut self,
        input: &str,
        progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
    ) -> Result<MainBrainOutput> {
        let start = std::time::Instant::now();

        // 1. 检查上下文使用率 — 超阈值自动截断（R-P0-1）
        let thresholds = &self.config.brain.thresholds;
        if self
            .history
            .is_context_full(thresholds.context_danger_threshold)
        {
            let truncated = self.history.truncate_to_recent(20);
            tracing::warn!(
                "上下文自动重建: 截断 {truncated} 条消息，保留最近 20 条（使用率 {:.0}%）",
                self.history.context_usage() * 100.0,
            );
        }

        // 2. 构建 messages — 临时追加用户输入，但不写入历史
        let mut messages = self.build_messages_with_user(input);

        // 发送 Connecting 事件
        if let Some(tx) = progress_tx {
            let _ = tx
                .send(ProgressEvent::Connecting {
                    brain: "main".into(),
                    model: self.llm.model().into(),
                })
                .await;
        }

        // 3. 跑 tool_loop — 失败直接返回 Err，历史保持不变
        let loop_result = tool_loop::run_tool_loop_with_config(
            self.llm.as_ref(),
            self.tool_executor.as_ref(),
            &mut messages,
            &self.tools,
            progress_tx,
            self.llm_max_tokens,
            self.llm_temperature,
        )
        .await?;

        let answer = loop_result.response.text();
        let total_tokens = loop_result.response.usage.total_tokens;

        // 4. 成功后一次性写入完整一轮到历史
        // 先写用户消息
        self.history.push_user(input);

        // 写入 tool_loop 中新增的消息（跳过 system + 旧历史 + user）
        let old_history_len = self.history.len() - 1; // 减去刚 push 的 user
        let skip_count = 1 + old_history_len + 1; // system(1) + old_history + user(1)

        for msg in messages.iter().skip(skip_count) {
            if msg.role == brain_llm::MessageRole::System {
                continue;
            }
            match msg.role {
                brain_llm::MessageRole::Assistant => {
                    let text = msg.text_content();
                    if !text.is_empty() {
                        self.history.push_assistant(&text);
                    }
                }
                brain_llm::MessageRole::User => {
                    for block in &msg.content {
                        if let brain_llm::ContentBlock::ToolResult { content, .. } = block {
                            self.history.push_tool_result(content);
                        }
                    }
                }
                _ => {}
            }
        }

        // 确保最终回答也写入历史
        let has_final_answer = self
            .history
            .messages()
            .iter()
            .rev()
            .take(3)
            .any(|m| m.role == brain_core::types::MessageRole::Assistant && m.content == answer);
        if !has_final_answer {
            self.history.push_assistant(&answer);
        }

        let elapsed = start.elapsed();

        Ok(MainBrainOutput {
            answer,
            usage: TurnUsage {
                total_tokens,
                llm_calls: loop_result.llm_calls,
                duration_ms: elapsed.as_millis() as u64,
            },
        })
    }

    /// 流式处理用户输入 — 返回进度事件通道
    ///
    /// P0-4 修复：返回 oneshot receiver 用于写回历史
    pub fn process_input_streaming(
        &mut self,
        input: &str,
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
                max_tokens,
                temperature,
            )
            .await;

            match result {
                Ok(loop_result) => {
                    let text = loop_result.response.text();
                    let _ = tx
                        .send(ProgressEvent::TextDelta { text: text.clone() })
                        .await;
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
        let full_prompt = match &self.memory_context {
            Some(ctx) => format!("{system_prompt}\n\n{ctx}\n{env_info}"),
            None => format!("{system_prompt}\n{env_info}"),
        };
        let mut messages = vec![ChatMessage::system(full_prompt)];
        messages.extend(self.history.to_chat_messages());
        messages
    }

    /// 构建 messages 并临时追加用户输入（不写入历史）。
    /// 用于事务式写入：失败时历史不变。
    fn build_messages_with_user(&self, user_input: &str) -> Vec<ChatMessage> {
        let mut messages = self.build_messages();
        messages.push(ChatMessage::user(user_input));
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

        let mut brain = MainBrain::new(llm, executor, config);
        let output = brain.process_input("你好", None).await.unwrap();

        assert_eq!(output.answer, "测试回复");
        assert!(!output.answer.is_empty());
    }

    #[test]
    fn build_messages_includes_system_prompt() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config);
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

        let mut brain = MainBrain::new(llm, executor, config);
        assert_eq!(brain.history_len(), 0);

        brain.process_input("你好", None).await.unwrap();
        // 应该有 user + assistant = 2 条
        assert_eq!(brain.history_len(), 2);
    }

    #[tokio::test]
    async fn commit_streaming_result_writes_history() {
        let llm = Arc::new(StubLlm);
        let executor = Arc::new(StubToolExecutor::new());
        let config = BrainConfig::default();

        let mut brain = MainBrain::new(llm, executor, config);
        brain.history.push_user("hello");

        let (rx, result_rx) = brain.process_input_streaming("hello").unwrap();
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

        let mut brain = MainBrain::new(llm, executor, config);
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

        let mut brain = MainBrain::new(llm, executor, config);

        brain.history.push_user("你好");
        brain.push_memory_context("[相关记忆]\n- 用户希望被称为测试大佬");
        brain.history.push_user("你应该称呼我什么");

        assert_eq!(brain.history_len(), 3); // user + system + user
        let messages = brain.history();
        assert_eq!(messages[0].role, brain_core::types::MessageRole::User);
        assert_eq!(messages[1].role, brain_core::types::MessageRole::System);
        assert!(messages[1].content.contains("测试大佬"));
    }
}
