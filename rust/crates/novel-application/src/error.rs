#[derive(Debug, thiserror::Error)]
pub enum NovelApplicationError {
    #[error("Novel application storage failed: {0}")]
    Storage(String),
    #[error("Novel legacy import failed: {0}")]
    Migration(String),
    #[error("Novel record conflict: {0}")]
    Conflict(String),
    #[error("Novel record not found: {0}")]
    NotFound(String),
    #[error("Novel resource failed: {0}")]
    Resource(String),
    #[error("Novel resource access denied: {0}")]
    ResourceDenied(String),
    #[error("Novel resource content changed: {0}")]
    ContextChanged(String),
    #[error("Novel serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Novel knowledge projection failed: {0}")]
    Knowledge(#[from] knowledge_core::KnowledgeError),
    #[error("Novel domain operation failed: {0}")]
    Domain(#[from] novel_domain::NovelDomainError),
    #[error("Novel workflow operation failed: {0}")]
    Workflow(String),
    #[error("Novel Writer is unavailable")]
    WriterUnavailable,
}

impl From<novel_workflow::NovelWorkflowPortError> for NovelApplicationError {
    fn from(error: novel_workflow::NovelWorkflowPortError) -> Self {
        match error {
            novel_workflow::NovelWorkflowPortError::ContextChanged(message) => {
                Self::ContextChanged(message)
            }
            other => Self::Workflow(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for NovelApplicationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<std::io::Error> for NovelApplicationError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, NovelApplicationError>;

#[cfg(test)]
mod tests {
    use novel_workflow::NovelWorkflowPortError;

    use super::NovelApplicationError;

    #[test]
    fn workflow_context_changed_conversion_preserves_application_variant() {
        let error: NovelApplicationError =
            NovelWorkflowPortError::ContextChanged("expected=old, actual=new".into()).into();

        assert!(matches!(
            error,
            NovelApplicationError::ContextChanged(message)
                if message == "expected=old, actual=new"
        ));
    }
}
