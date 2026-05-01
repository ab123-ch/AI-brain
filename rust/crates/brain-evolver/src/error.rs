use thiserror::Error;

#[derive(Error, Debug)]
pub enum EvolverError {
    #[error("Sandbox error: {0}")]
    Sandbox(String),

    #[error("TDD failure: {0}")]
    Tdd(String),

    #[error("Guard violation: {0}")]
    GuardViolation(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Git error: {0}")]
    Git(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Evolution already in progress")]
    AlreadyInProgress,

    #[error("Evolution not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, EvolverError>;
