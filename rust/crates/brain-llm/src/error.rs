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
            Self::ApiError { status, message } => {
                !is_deterministic_model_route_error(message)
                    && matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
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

fn is_deterministic_model_route_error(message: &str) -> bool {
    let normalized = message.to_lowercase();
    normalized.contains("model_not_found")
        || normalized.contains("model not found")
        || normalized.contains("no available channel")
        || normalized.contains("没有可用渠道")
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
}
