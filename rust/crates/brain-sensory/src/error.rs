use thiserror::Error;

#[derive(Debug, Error)]
pub enum SensoryError {
    #[error("LLM call failed: {0}")]
    LlmFailed(String),
    #[error("broadcast send failed: {0}")]
    BroadcastFailed(#[from] brain_bus::BusError),
}
