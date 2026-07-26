pub type Result<T> = std::result::Result<T, NovelDomainError>;

#[derive(Debug, thiserror::Error)]
pub enum NovelDomainError {
    #[error("invalid Novel request: {0}")]
    InvalidRequest(String),
    #[error("invalid Novel transition: {0}")]
    InvalidTransition(String),
    #[error("invalid Novel candidate: {0}")]
    InvalidCandidate(String),
    #[error("Novel project {0} already has active work")]
    ProjectBusy(String),
    #[error("Novel Canon revision is stale: expected={expected}, actual={actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("Novel domain serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}
