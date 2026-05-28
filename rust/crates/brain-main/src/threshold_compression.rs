//! 阈值触发智能压缩模块
//!
//! 当上下文使用率超过 80% 时，同步调用 LLM 进行智能压缩，
//! 保留决策链路（用户目标→执行步骤→最终结果），替代粗暴截断。

use brain_core::types::ConversationMessage;
#[allow(unused_imports)]
use brain_llm::LlmProvider;

/// 阈值压缩配置
#[derive(Debug, Clone)]
pub struct ThresholdCompactionConfig {
    /// 保留最近 N 轮对话（默认 4）
    pub preserve_recent_turns: usize,
    /// 摘要最大 token 数（默认 2048）
    pub max_summary_tokens: u32,
    /// 决策链路保留强度（0.0-1.0，默认 0.8）
    pub decision_chain_weight: f64,
    /// 压缩超时时间（毫秒，默认 30000）
    pub timeout_ms: u64,
}

impl Default for ThresholdCompactionConfig {
    fn default() -> Self {
        Self {
            preserve_recent_turns: 4,
            max_summary_tokens: 2048,
            decision_chain_weight: 0.8,
            timeout_ms: 30_000,
        }
    }
}

/// 压缩后的上下文
#[derive(Debug, Clone)]
pub struct CompressedContext {
    /// 决策链路摘要
    pub decision_summary: String,
    /// 保留的最近消息
    pub recent_messages: Vec<ConversationMessage>,
    /// 压缩元数据
    pub metadata: CompressionMetadata,
}

/// 压缩元数据
#[derive(Debug, Clone)]
pub struct CompressionMetadata {
    /// 原始消息总数
    pub original_count: usize,
    /// 压缩的消息数
    pub compressed_count: usize,
    /// 保留的消息数
    pub preserved_count: usize,
    /// 压缩时间（毫秒）
    pub duration_ms: u64,
}

/// 压缩错误类型
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("LLM 调用失败: {0}")]
    LlmError(String),
    #[error("消息分割失败: 消息数不足")]
    InsufficientMessages,
    #[error("压缩超时（超过 {0}ms）")]
    Timeout(u64),
}

/// 阈值压缩器
pub struct ThresholdCompressor {
    config: ThresholdCompactionConfig,
}

impl ThresholdCompressor {
    pub fn new(config: ThresholdCompactionConfig) -> Self {
        Self { config }
    }

    /// 分割消息：旧消息 vs 最近 N 轮
    pub(crate) fn split_messages<'a>(
        &self,
        messages: &'a [ConversationMessage],
    ) -> (&'a [ConversationMessage], &'a [ConversationMessage]) {
        let preserve_count = self.config.preserve_recent_turns * 2; // 每轮 = user + assistant
        let split_point = messages.len().saturating_sub(preserve_count);
        (&messages[..split_point], &messages[split_point..])
    }

    /// 构建决策链路保留 prompt
    pub(crate) fn build_decision_chain_prompt(&self, messages: &[ConversationMessage]) -> String {
        let weight_desc = match self.config.decision_chain_weight {
            w if w >= 0.8 => "详细保留每一步决策过程和关键转折点",
            w if w >= 0.5 => "保留主要决策节点和关键结论",
            _ => "只保留最终决策和核心结论",
        };

        format!(
            r#"你是一个对话历史压缩专家。请将以下对话历史压缩成一个结构化的决策链路摘要。

## 压缩要求

**保留重点**（决策链路保留强度：{weight}）：
1. **用户目标** — 用户最初想要什么，需求是否有变化
2. **用户反馈和指令** — ⭐ 重点保留
   - 用户的纠正："这个不对，应该要 xxxxx"
   - 用户的认可和改进建议："这个对了，但是可以 xxxx"
   - 用户的新需求/指令："很好，继续下一个需求，需求：xxxx"
   - 用户表达的偏好、标准、风格要求
3. **执行步骤** — 按时间顺序，做了哪些关键操作
4. **决策转折点** — 遇到了什么问题，如何调整方案的
5. **最终结果** — 得到了什么结论，完成了什么
6. **关键上下文** — 重要的文件路径、代码位置、配置信息

**丢弃内容**：
- 完整的代码输出、grep 结果、文件内容
- 中间过程的详细日志
- 重复的信息、确认性对话
- 工具调用的原始返回（只保留从中得出的结论）

## 输出格式

请严格按照以下格式输出：

```
## 用户目标
[一句话描述用户的核心需求]

## 用户反馈和指令
- [纠正] "这个不对，应该要 xxxxx"
- [认可+改进] "这个对了，但是可以 xxxx"
- [新需求] "继续下一个需求：xxxx"
- [偏好/标准] "我喜欢 xxx 风格"、"要求 xxx 标准"

## 执行过程
1. [第一步操作] → [结果/发现]
2. [第二步操作] → [结果/发现]
3. ...（按时间顺序）

## 关键决策
- [遇到的问题] → [采取的解决方案] → [原因]

## 最终结论
[完成情况、核心成果、待办事项（如有）]

## 关键上下文
- 文件：[重要文件路径和修改内容]
- 配置：[关键配置项]
- 其他：[需要记住的重要信息]
```

## 对话历史

{formatted_messages}

## 开始压缩

请提取决策链路，生成结构化摘要："#,
            weight = weight_desc,
            formatted_messages = self.format_messages_for_prompt(messages),
        )
    }

    /// 格式化消息用于 prompt
    pub(crate) fn format_messages_for_prompt(&self, messages: &[ConversationMessage]) -> String {
        messages
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                let role = match msg.role {
                    brain_core::types::MessageRole::User => "用户",
                    brain_core::types::MessageRole::Assistant => "助手",
                    brain_core::types::MessageRole::Tool => "工具",
                    _ => "系统",
                };
                let content = msg.text_content();
                let content_str = if content.len() > 500 {
                    format!(
                        "{}...[中间省略 {} 字符]...{}",
                        &content[..200],
                        content.len() - 400,
                        &content[content.len() - 200..]
                    )
                } else {
                    content.to_string()
                };
                format!("【{}】{}\n{}", role, i + 1, content_str)
            })
            .collect::<Vec<_>>()
            .join("\n---\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_messages(count: usize) -> Vec<ConversationMessage> {
        (0..count)
            .map(|i| {
                if i % 2 == 0 {
                    ConversationMessage::user(format!("消息 {}", i))
                } else {
                    ConversationMessage::assistant(format!("消息 {}", i))
                }
            })
            .collect()
    }

    #[test]
    fn test_split_messages_basic() {
        let config = ThresholdCompactionConfig {
            preserve_recent_turns: 2,
            ..Default::default()
        };
        let compressor = ThresholdCompressor::new(config);

        let messages = create_test_messages(10);
        let (old, recent) = compressor.split_messages(&messages);

        // preserve_recent_turns=2, 保留最近 2*2=4 条消息
        assert_eq!(old.len(), 6);
        assert_eq!(recent.len(), 4);
    }

    #[test]
    fn test_split_messages_exact_boundary() {
        let config = ThresholdCompactionConfig {
            preserve_recent_turns: 2,
            ..Default::default()
        };
        let compressor = ThresholdCompressor::new(config);

        // 消息数刚好等于 preserve_recent_turns * 2
        let messages = create_test_messages(4);
        let (old, recent) = compressor.split_messages(&messages);

        assert_eq!(old.len(), 0);
        assert_eq!(recent.len(), 4);
    }

    #[test]
    fn test_split_messages_fewer_than_preserve() {
        let config = ThresholdCompactionConfig {
            preserve_recent_turns: 4,
            ..Default::default()
        };
        let compressor = ThresholdCompressor::new(config);

        // 消息数少于 preserve_recent_turns * 2
        let messages = create_test_messages(5);
        let (old, recent) = compressor.split_messages(&messages);

        assert_eq!(old.len(), 0);
        assert_eq!(recent.len(), 5);
    }

    #[test]
    fn test_build_decision_chain_prompt() {
        let config = ThresholdCompactionConfig {
            decision_chain_weight: 0.8,
            ..Default::default()
        };
        let compressor = ThresholdCompressor::new(config);

        let messages = create_test_messages(5);
        let prompt = compressor.build_decision_chain_prompt(&messages);

        // 验证 prompt 包含关键元素
        assert!(prompt.contains("决策链路"));
        assert!(prompt.contains("用户目标"));
        assert!(prompt.contains("用户反馈和指令"));
        assert!(prompt.contains("执行过程"));
        assert!(prompt.contains("关键决策"));
        assert!(prompt.contains("最终结论"));
    }

    #[test]
    fn test_format_messages_for_prompt() {
        let config = ThresholdCompactionConfig::default();
        let compressor = ThresholdCompressor::new(config);

        let messages = create_test_messages(4);
        let formatted = compressor.format_messages_for_prompt(&messages);

        // 验证格式化结果
        assert!(formatted.contains("【用户】"));
        assert!(formatted.contains("【助手】"));
        assert!(formatted.contains("---"));
    }
}
