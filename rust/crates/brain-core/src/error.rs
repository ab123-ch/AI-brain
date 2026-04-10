use thiserror::Error;

/// brain-core 层统一错误
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid weight value: {0}, expected [0.1, 1.0]")]
    InvalidWeight(f64),

    #[error("unknown brain id: {0}")]
    UnknownBrainId(String),

    #[error("serialization failed: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type CoreResult<T> = Result<T, CoreError>;
