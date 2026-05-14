use thiserror::Error;

/// 评估脑错误类型
#[derive(Error, Debug)]
pub enum EvalError {
    /// LLM 调用失败
    #[error("LLM evaluation failed: {0}")]
    LlmError(String),

    /// 输入无效
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// 工具执行失败
    #[error("tool execution failed: {0}")]
    ToolError(String),
}

pub type Result<T> = std::result::Result<T, EvalError>;
