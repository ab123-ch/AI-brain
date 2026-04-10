use thiserror::Error;

#[derive(Error, Debug)]
pub enum EvaluationError {
    #[error("invalid snapshot: {0}")]
    InvalidSnapshot(String),

    #[error("health check failed: {0}")]
    HealthCheckFailed(String),
}

pub type Result<T> = std::result::Result<T, EvaluationError>;
