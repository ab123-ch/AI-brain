/// 推理脑统一错误
#[derive(Debug, thiserror::Error)]
pub enum ReasoningError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("经验库错误: {0}")]
    Experience(String),

    #[error("记忆脑错误: {0}")]
    Memory(String),
}

pub type Result<T> = std::result::Result<T, ReasoningError>;
