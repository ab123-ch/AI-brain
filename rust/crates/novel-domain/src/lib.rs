//! Novel aggregate contracts and transition authority.

mod canon;
mod consistency;
mod error;
mod knowledge;
mod model;
mod recall;
mod review;
mod state;

pub use canon::{apply_canon_delta, resolve_conflict, CanonCommitOutcome};
pub use consistency::{check_consistency, ConsistencyIssue, ConsistencyReport, IssueSeverity};
pub use error::{NovelDomainError, Result};
pub use knowledge::{
    NovelEntityKind, NovelKnowledgeEntity, NovelKnowledgeEvent, NovelKnowledgeRelation,
    NovelKnowledgeStatus, NovelRelationKind,
};
pub use model::*;
pub use recall::build_recall_pack;
pub use review::{validate_main_review, validate_self_review};
pub use state::NovelTaskState;
