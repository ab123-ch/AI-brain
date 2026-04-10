/// 校验脑统一错误
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("安全校验未通过: {0}")]
    SafetyCheckFailed(String),

    #[error("真实性不足: {0}")]
    TruthfulnessTooLow(String),
}

pub type Result<T> = std::result::Result<T, ValidationError>;
