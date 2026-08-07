use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("API request failed: {0}")]
    RequestFailed(String),

    #[error("API returned error: status={status}, message={message}")]
    ApiError { status: u16, message: String },

    #[error("stream error: {0}")]
    StreamError(String),

    #[error("API key not found: {0}")]
    ApiKeyNotFound(String),

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("brain not configured for LLM: {0}")]
    BrainNotConfigured(String),

    #[error("retries exhausted after {attempts} attempts: {last_error}")]
    RetriesExhausted { attempts: u32, last_error: String },

    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

impl LlmError {
    /// 判断错误是否可重试（网络超时、连接失败、服务端 5xx 等）
    pub fn is_retryable(&self) -> bool {
        match self {
            // 网络层失败（超时、连接断开等）— 可重试
            Self::RequestFailed(msg) => {
                let lower = msg.to_lowercase();
                lower.contains("timeout")
                    || lower.contains("connection")
                    || lower.contains("timed out")
                    || lower.contains("connect")
                    || lower.contains("hyper")
                    || lower.contains("tcp")
                    || lower.contains("socket")
            }
            // API 返回可重试的 HTTP 状态码（408/429/500/502/503/504）
            Self::ApiError { status, .. } => {
                matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
            }
            // 以下不可重试
            Self::Config(_)
            | Self::StreamError(_)
            | Self::ApiKeyNotFound(_)
            | Self::ProviderNotFound(_)
            | Self::BrainNotConfigured(_)
            | Self::RetriesExhausted { .. }
            | Self::JsonError(_)
            | Self::IoError(_) => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, LlmError>;
