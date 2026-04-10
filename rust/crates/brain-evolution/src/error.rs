use thiserror::Error;

#[derive(Error, Debug)]
pub enum EvolutionError {
    #[error("brain not found: {0}")]
    BrainNotFound(String),

    #[error("brain already active: {0}")]
    BrainAlreadyActive(String),

    #[error("brain already dormant: {0}")]
    BrainAlreadyDormant(String),

    #[error("template not found: {0}")]
    TemplateNotFound(String),

    #[error("template name conflict: {0}")]
    TemplateNameConflict(String),

    #[error("persistence failed: {0}")]
    PersistenceFailed(String),

    #[error("load failed: {0}")]
    LoadFailed(String),

    #[error("registration failed: {0}")]
    RegistrationFailed(String),

    #[error("suggestion failed: {0}")]
    SuggestionFailed(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, EvolutionError>;
