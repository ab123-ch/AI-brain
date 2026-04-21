use thiserror::Error;

/// 主脑统一错误
#[derive(Debug, Error)]
pub enum MainBrainError {
    #[error("LLM 调用失败: {0}")]
    LlmError(String),

    #[error("超过最大重试次数 ({0})")]
    MaxRetriesExceeded(u32),

    #[error("序列化错误: {0}")]
    SerializationError(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, MainBrainError>;
