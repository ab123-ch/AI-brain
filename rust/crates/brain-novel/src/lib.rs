//! Resident, project-scoped NovelBrain runtime.
//!
//! This crate owns the in-process writing state machine and LLM session continuity.
//! Durable state and artifact access are available only through typed ports.

mod actor;
mod error;
mod handle;
mod ports;
mod review;
mod runtime;
mod state;
mod types;

pub use actor::{spawn_novel_brain, NovelBrainConfig};
pub use error::{NovelBrainError, Result};
pub use handle::NovelBrainHandle;
pub use ports::{
    ContextDocument, NovelMemoryPort, NovelPortError, NovelResourcePort, NovelWorkspaceSnapshot,
};
pub use review::{parse_novel_response, validate_main_review, validate_self_review};
pub use state::NovelTaskState;
pub use types::*;
