use thiserror::Error;

#[derive(Debug, Error)]
pub enum MasterError {
    #[error("bus error: {0}")]
    BusError(#[from] brain_bus::BusError),
    #[error("no task context — run_loop called without broadcast")]
    NoTaskContext,
    #[error("fast think timeout — no responses within {0:?}")]
    FastThinkTimeout(std::time::Duration),
    #[error("no relevant brain responses")]
    NoRelevantResponses,
}
