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
}

impl ConversationHistory {
    pub fn new(max_context_tokens: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_context_tokens,
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

    /// 追加工具结果消息
    pub fn push_tool_result(&mut self, content: impl Into<String>) {
        self.messages.push(ConversationMessage::tool(content));
    }

    /// 追加评估脑反馈消息
    pub fn push_evaluator(&mut self, content: impl Into<String>) {
        self.messages.push(ConversationMessage::evaluator(content));
    }

    /// 追加系统消息（如记忆注入）
    pub fn push_system(&mut self, content: impl Into<String>) {
        self.messages.push(ConversationMessage {
            role: MessageRole::System,
            content: content.into(),
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
                let chars = m.content.chars().count();
                // 对中文友好的估算：每字符约 0.75 token
                chars * 3 / 4
            })
            .sum()
    }

    /// 上下文使用率 (0.0 ~ 1.0)
    pub fn context_usage(&self) -> f64 {
        let tokens = self.estimate_tokens();
        #[allow(clippy::cast_precision_loss)]
        let ratio = tokens as f64 / self.max_context_tokens as f64;
        ratio
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

    /// 转换为 LLM ChatMessage 格式
    pub fn to_chat_messages(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::Assistant => LlmRole::Assistant,
                    // R-P2-1: Tool/System/Evaluator 均映射为 User
                    // 原因: ConversationMessage 不携带 tool_call_id，不能映射为 LlmRole::Tool（否则 API 400）
                    // System 消息（如记忆注入）和 Evaluator 消息也映射为 User，
                    // 因为 API 通常只接受 System/User/Assistant 三种角色，
                    // 且 System 只允许出现在第一条消息中。
                    // 长期方案: S4 升级 ContentBlock 模型后，Tool 消息可正确映射。
                    MessageRole::Tool
                    | MessageRole::System
                    | MessageRole::User
                    | MessageRole::Evaluator => LlmRole::User,
                };
                ChatMessage {
                    role,
                    content: vec![brain_llm::ContentBlock::text(&m.content)],
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
}
