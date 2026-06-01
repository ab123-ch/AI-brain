use brain_core::types::{BrainState, ConversationMessage, MessageRole};
use brain_llm::{ChatMessage, MessageRole as LlmRole};

/// 对话历史管理
///
/// 维护完整的对话消息列表，支持 token 估算和上下文重建。
/// 对标 Claude Code 的 messages 构造：全量历史，不每次重建。
pub struct ConversationHistory {
    messages: Vec<ConversationMessage>,
    /// 上下文窗口 token 上限
    max_context_tokens: usize,
    /// 真实 prompt_tokens（从 LLM usage 累加），0 表示未设置
    tracked_prompt_tokens: u64,
}

impl ConversationHistory {
    pub fn new(max_context_tokens: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_context_tokens,
            tracked_prompt_tokens: 0,
        }
    }

    /// 追加用户消息
    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ConversationMessage::user(content));
    }

    /// 追加助手消息
    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ConversationMessage::assistant(content));
    }

    /// 追加助手消息（含工具调用）
    pub fn push_assistant_blocks(&mut self, blocks: Vec<brain_core::types::ContentBlock>) {
        self.messages
            .push(ConversationMessage::assistant_blocks(blocks));
    }

    /// 追加工具结果消息（完整版，保留 tool_use_id）
    pub fn push_tool_result(&mut self, tool_use_id: String, content: String, is_error: bool) {
        self.messages.push(ConversationMessage::tool_result(
            tool_use_id,
            content,
            is_error,
        ));
    }

    /// 追加评估脑反馈消息
    pub fn push_evaluator(&mut self, content: impl Into<String>) {
        self.messages.push(ConversationMessage::evaluator(content));
    }

    /// 追加系统消息（如记忆注入）
    pub fn push_system(&mut self, content: impl Into<String>) {
        self.messages.push(ConversationMessage {
            role: MessageRole::System,
            content: vec![brain_core::types::ContentBlock::text(content)],
            timestamp: chrono::Utc::now(),
        });
    }

    /// 获取消息列表
    pub fn messages(&self) -> &[ConversationMessage] {
        &self.messages
    }

    /// 消息数量
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// 估算当前 token 数（R-P2-2: 中文友好估算）
    ///
    /// 中文通常 1 字 ≈ 1.5 token，英文约 4 字符 ≈ 1 token。
    /// 混合场景下用 chars * 3 / 4 作为折中估算。
    pub fn estimate_tokens(&self) -> usize {
        self.messages
            .iter()
            .map(|m| {
                let chars = m.text_content().chars().count();
                // 对中文友好的估算：每字符约 0.75 token
                chars * 3 / 4
            })
            .sum()
    }

    /// 上下文使用率 (0.0 ~ 1.0)
    ///
    /// 优先用真实 prompt_tokens（从 LLM usage 累加），回退到估算值。
    pub fn context_usage(&self) -> f64 {
        let tokens = if self.tracked_prompt_tokens > 0 {
            self.tracked_prompt_tokens as usize
        } else {
            self.estimate_tokens()
        };
        #[allow(clippy::cast_precision_loss)]
        let ratio = tokens as f64 / self.max_context_tokens as f64;
        ratio
    }

    /// 设置真实 prompt_tokens（从 LLM usage 累加）
    pub fn set_tracked_tokens(&mut self, tokens: u64) {
        self.tracked_prompt_tokens = tokens;
    }

    /// 获取 tracked_prompt_tokens
    #[allow(dead_code)]
    pub fn tracked_prompt_tokens(&self) -> u64 {
        self.tracked_prompt_tokens
    }

    /// 追加消息
    pub fn push(&mut self, message: ConversationMessage) {
        self.messages.push(message);
    }

    /// 判断是否超过危险阈值（需要重建）
    pub fn is_context_full(&self, threshold: f64) -> bool {
        self.context_usage() >= threshold
    }

    /// 清空所有对话历史
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// 自动截断 — 保留最近 N 条消息（R-P0-1）
    ///
    /// 返回被截断的消息数量。用于上下文超过危险阈值时的自动重建，
    /// 不需要 BrainState，仅保留最近消息。
    pub fn truncate_to_recent(&mut self, keep_recent: usize) -> usize {
        if self.messages.len() <= keep_recent {
            return 0;
        }
        let truncated = self.messages.len() - keep_recent;
        self.messages = self.messages.split_off(truncated);
        // 截断后重置 tracked_prompt_tokens，使用估算值
        self.tracked_prompt_tokens = 0;
        truncated
    }

    /// 上下文重建 — 清空后注入记忆脑快照 + 最近 N 轮
    ///
    /// 返回被截断的消息数量。
    pub fn clear_and_rebuild(
        &mut self,
        brain_state: &BrainState,
        keep_recent_turns: usize,
    ) -> usize {
        let old_len = self.messages.len();

        // 保留最近 N 条
        let recent: Vec<ConversationMessage> = self
            .messages
            .iter()
            .rev()
            .take(keep_recent_turns)
            .cloned()
            .collect();

        // 清空
        self.messages.clear();

        // 注入记忆脑快照作为"上一轮会话内容"
        let summary = crate::prompts::build_context_rebuild_prompt(&format!(
            "事实总结: {}\n用户偏好: {:?}\n注意事项: {}",
            brain_state.fact_summary,
            brain_state
                .user_profile
                .explicit_preferences
                .iter()
                .chain(brain_state.user_profile.implicit_preferences.iter())
                .take(5)
                .collect::<Vec<_>>(),
            brain_state
                .active_pitfalls
                .iter()
                .map(|p| format!("- [{:?}] {}", p.category, p.description))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        self.messages.push(ConversationMessage::user(summary));

        // 追加最近消息（逆序恢复）
        for msg in recent.into_iter().rev() {
            self.messages.push(msg);
        }

        old_len - self.messages.len()
    }

    /// 从压缩结果重建上下文
    ///
    /// 清空现有消息，注入压缩摘要作为系统消息，然后追加最近的消息。
    /// 用于 decision_chain_threshold 压缩后的上下文重建。
    pub fn clear_and_rebuild_from_compressed(
        &mut self,
        summary: &str,
        recent_messages: &[ConversationMessage],
    ) {
        // 1. 清空现有消息
        self.messages.clear();

        // 2. 添加压缩摘要作为系统消息
        let summary_message = ConversationMessage {
            role: MessageRole::System,
            content: vec![brain_core::types::ContentBlock::text(format!(
                r#"以下是之前对话的决策链路摘要：

{summary}

请基于这个摘要继续对话，不要重复已经完成的工作。"#,
                summary = summary
            ))],
            timestamp: chrono::Utc::now(),
        };
        self.messages.push(summary_message);

        // 3. 追加最近的消息
        self.messages.extend(recent_messages.iter().cloned());

        // 4. 重置 token 追踪
        self.tracked_prompt_tokens = 0;
    }

    /// 转换为 LLM ChatMessage 格式
    ///
    /// 直接 clone 原始 ContentBlock，保留 ToolUse/ToolResult 的结构信息。
    /// ToolResult 块以 User 角色发送（OpenAI API 规范）。
    pub fn to_chat_messages(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::Assistant => LlmRole::Assistant,
                    // ToolResult 在 API 中以 User 角色发送
                    // System/Evaluator 也映射为 User（API 只接受 System/User/Assistant）
                    MessageRole::Tool
                    | MessageRole::System
                    | MessageRole::User
                    | MessageRole::Evaluator => LlmRole::User,
                };
                ChatMessage {
                    role,
                    content: m.content.clone(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_count() {
        let mut history = ConversationHistory::new(100_000);
        assert!(history.len() == 0);

        history.push_user("你好");
        history.push_assistant("你好！有什么可以帮你的？");
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn estimate_tokens() {
        let mut history = ConversationHistory::new(100_000);
        history.push_user("Hello World"); // 11 chars → 11*3/4 = 8 tokens
        let tokens = history.estimate_tokens();
        assert!(tokens > 0);
        assert!(tokens < 100);
    }

    #[test]
    fn context_usage() {
        let mut history = ConversationHistory::new(100);
        // 填充大量内容使其超过 60%
        history.push_user("x".repeat(200)); // 200 chars → 100 tokens → 100%
        assert!(history.context_usage() > 0.9);
    }

    #[test]
    fn clear_and_rebuild() {
        let mut history = ConversationHistory::new(100_000);
        for i in 0..20 {
            history.push_user(format!("消息 {i}"));
        }
        assert_eq!(history.len(), 20);

        let state = BrainState {
            fact_summary: "测试总结".into(),
            user_profile: brain_core::types::UserProfile::default(),
            active_pitfalls: Vec::new(),
            evolution_rules: Vec::new(),
            index_entries: Vec::new(),
            snapshot_at: chrono::Utc::now(),
        };

        let truncated = history.clear_and_rebuild(&state, 3);
        assert!(truncated > 0);
        // 总结消息 + 3 条最近消息 = 4
        assert_eq!(history.len(), 4);
    }

    #[test]
    fn to_chat_messages_role_mapping() {
        let mut history = ConversationHistory::new(100_000);
        history.push_user("用户消息");
        history.push_assistant("助手回复");

        let chat = history.to_chat_messages();
        assert_eq!(chat.len(), 2);
        assert_eq!(chat[0].role, LlmRole::User);
        assert_eq!(chat[1].role, LlmRole::Assistant);
    }

    #[test]
    fn to_chat_messages_preserves_tool_use() {
        let mut history = ConversationHistory::new(100_000);
        history.push_user("搜索 firecrawl");
        history.push_assistant_blocks(vec![
            brain_core::types::ContentBlock::text("我来搜索"),
            brain_core::types::ContentBlock::ToolUse {
                id: "toolu_01".into(),
                name: "WebFetch".into(),
                input: serde_json::json!({"url": "https://github.com/firecrawl"}),
            },
        ]);
        history.push_tool_result("toolu_01".into(), "firecrawl 数据".into(), false);
        history.push_assistant("搜索结果如下...");

        let chat = history.to_chat_messages();
        assert_eq!(chat.len(), 4);
        // msg[0] = user (搜索 firecrawl)
        assert_eq!(chat[0].role, LlmRole::User);
        // msg[1] = assistant (含 ToolUse)
        assert_eq!(chat[1].role, LlmRole::Assistant);
        assert!(chat[1].content.iter().any(|b| b.is_tool_use()));
        // msg[2] = user (ToolResult → User role)
        assert_eq!(chat[2].role, LlmRole::User);
        assert!(chat[2].content.iter().any(|b| b.is_tool_result()));
        // msg[3] = assistant (搜索结果如下...)
        assert_eq!(chat[3].role, LlmRole::Assistant);
    }

    #[test]
    fn push_tool_result_stores_structured_data() {
        let mut history = ConversationHistory::new(100_000);
        history.push_tool_result("toolu_42".into(), "error: not found".into(), true);

        let chat = history.to_chat_messages();
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].role, LlmRole::User);
        assert!(chat[0].content.iter().any(|b| b.is_tool_result()));

        // 验证结构化字段保留
        if let brain_core::types::ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &chat[0].content[0]
        {
            assert_eq!(tool_use_id, "toolu_42");
            assert_eq!(content, "error: not found");
            assert!(*is_error);
        } else {
            panic!("expected ToolResult block");
        }
    }

    #[test]
    fn push_assistant_blocks_preserves_multiple_blocks() {
        let mut history = ConversationHistory::new(100_000);
        history.push_assistant_blocks(vec![
            brain_core::types::ContentBlock::text("分析中..."),
            brain_core::types::ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            },
            brain_core::types::ContentBlock::text("继续执行"),
            brain_core::types::ContentBlock::ToolUse {
                id: "call_2".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/test.txt"}),
            },
        ]);

        let chat = history.to_chat_messages();
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].role, LlmRole::Assistant);
        assert_eq!(chat[0].content.len(), 4);
        // 2 个 text + 2 个 tool_use
        let tool_uses: Vec<_> = chat[0].content.iter().filter(|b| b.is_tool_use()).collect();
        assert_eq!(tool_uses.len(), 2);
        let texts: Vec<_> = chat[0].content.iter().filter_map(|b| b.as_text()).collect();
        assert_eq!(texts.len(), 2);
    }

    #[test]
    fn test_clear_and_rebuild_from_compressed() {
        let mut history = ConversationHistory::new(131072);

        // 添加一些消息
        for i in 0..10 {
            history.push(if i % 2 == 0 {
                ConversationMessage::user(format!("消息 {}", i))
            } else {
                ConversationMessage::assistant(format!("消息 {}", i))
            });
        }

        assert_eq!(history.messages().len(), 10);

        // 重建上下文
        let summary = "## 用户目标\n测试重建\n\n## 最终结论\n测试完成";
        let recent_messages = vec![
            ConversationMessage::user("最近消息1"),
            ConversationMessage::assistant("最近回复1"),
        ];

        history.clear_and_rebuild_from_compressed(summary, &recent_messages);

        // 验证：应该有 3 条消息（摘要 + 2 条最近消息）
        assert_eq!(history.messages().len(), 3);

        // 验证：第一条是系统消息（摘要）
        assert_eq!(history.messages()[0].role, MessageRole::System);
        assert!(history.messages()[0].text_content().contains("决策链路摘要"));
        assert!(history.messages()[0].text_content().contains("测试重建"));

        // 验证：后面是最近的消息
        assert_eq!(history.messages()[1].text_content(), "最近消息1");
        assert_eq!(history.messages()[2].text_content(), "最近回复1");

        // 验证：tracked_prompt_tokens 被重置
        assert_eq!(history.tracked_prompt_tokens(), 0);
    }
}
