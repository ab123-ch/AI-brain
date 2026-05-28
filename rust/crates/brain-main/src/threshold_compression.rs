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
}
