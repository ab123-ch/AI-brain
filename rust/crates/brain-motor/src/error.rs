/// 执行脑统一错误
#[derive(Debug, thiserror::Error)]
pub enum MotorError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("工具未注册: {0}")]
    ToolNotRegistered(String),

    #[error("校验未通过: {0}")]
    ValidationFailed(String),

    #[error("执行超时: {0}")]
    Timeout(String),
}

pub type Result<T> = std::result::Result<T, MotorError>;
