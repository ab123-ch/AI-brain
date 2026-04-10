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

    #[error("API key not found: env var {0} not set")]
    ApiKeyNotFound(String),

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("brain not configured for LLM: {0}")]
    BrainNotConfigured(String),

    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, LlmError>;
