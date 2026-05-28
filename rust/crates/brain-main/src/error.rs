use thiserror::Error;

/// 主脑统一错误
#[derive(Debug, Error)]
pub enum MainBrainError {
    #[error("LLM 调用失败: {0}")]
    LlmError(String),

    #[error("序列化错误: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("上下文压缩失败: {0}")]
    ContextCompactionFailed(String),
}

pub type Result<T> = std::result::Result<T, MainBrainError>;
