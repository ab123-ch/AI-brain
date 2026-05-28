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
}
