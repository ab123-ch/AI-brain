use thiserror::Error;

use crate::retry::is_retryable_http_status;

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
            // RequestFailed 已失去底层错误类型，不能根据展示字符串猜测。
            // 网络错误必须在仍持有 reqwest::Error 的 Provider 边界分类。
            // API 返回可重试的 HTTP 状态码（408/429/500/502/503/504）
            Self::ApiError { status, message } => is_retryable_http_status(*status, message),
            // 以下不可重试
            Self::RequestFailed(_)
            | Self::Config(_)
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

#[cfg(test)]
mod tests {
    use super::LlmError;

    #[test]
    fn model_route_503_is_not_retryable() {
        for message in [
            "model_not_found: No available channel for gemini-3.5-flash",
            "Model not found in the default group",
            "模型没有可用渠道",
        ] {
            let error = LlmError::ApiError {
                status: 503,
                message: message.into(),
            };
            assert!(!error.is_retryable(), "unexpected retry for {message}");
        }
    }

    #[test]
    fn ordinary_503_remains_retryable() {
        let error = LlmError::ApiError {
            status: 503,
            message: "temporary upstream overload".into(),
        };

        assert!(error.is_retryable());
    }

    #[test]
    fn request_failed_display_text_never_drives_retry_classification() {
        for message in [
            "timeout",
            "connection reset",
            "error sending request for url",
            "hyper tcp socket failure",
        ] {
            let error = LlmError::RequestFailed(message.into());
            assert!(!error.is_retryable(), "展示字符串不应触发重试: {message}");
        }
    }
}
