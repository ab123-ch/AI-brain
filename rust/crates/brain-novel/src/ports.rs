use std::path::Path;

use async_trait::async_trait;
use brain_memory::novel::{
    CommitReport, ConsistencyReport, NovelArtifactReceipt, NovelProject, NovelPublicationRecord,
    NovelRecallPack, NovelTaskCheckpoint, NovelTaskEvent, NovelTaskType,
};

use crate::ContextRef;

#[derive(Debug, thiserror::Error)]
pub enum NovelPortError {
    #[error("memory port: {0}")]
    Memory(String),
    #[error("resource port: {0}")]
    Resource(String),
    #[error("context changed: {0}")]
    ContextChanged(String),
    #[error("resource denied: {0}")]
    ResourceDenied(String),
}

#[derive(Debug, Clone)]
pub struct NovelWorkspaceSnapshot {
    pub project: NovelProject,
    pub active_checkpoint: Option<NovelTaskCheckpoint>,
}

#[derive(Debug, Clone)]
pub struct ContextDocument {
    pub reference: ContextRef,
    pub content: String,
}

#[async_trait]
pub trait NovelMemoryPort: Send + Sync {
    async fn load_workspace(
        &self,
        project_id: &str,
    ) -> std::result::Result<NovelWorkspaceSnapshot, NovelPortError>;

    async fn active_checkpoints(
        &self,
    ) -> std::result::Result<Vec<NovelTaskCheckpoint>, NovelPortError>;

    async fn load_checkpoint(
        &self,
        task_id: &str,
    ) -> std::result::Result<Option<NovelTaskCheckpoint>, NovelPortError>;

    async fn recall_project(
        &self,
        project_id: &str,
        task_type: NovelTaskType,
    ) -> std::result::Result<NovelRecallPack, NovelPortError>;

    async fn check_consistency(
        &self,
        project_id: &str,
    ) -> std::result::Result<ConsistencyReport, NovelPortError>;

    async fn append_task_event(
        &self,
        event: NovelTaskEvent,
    ) -> std::result::Result<(), NovelPortError>;

    async fn save_checkpoint(
        &self,
        checkpoint: NovelTaskCheckpoint,
    ) -> std::result::Result<(), NovelPortError>;

    async fn begin_publication(
        &self,
        record: NovelPublicationRecord,
    ) -> std::result::Result<(), NovelPortError>;

    async fn load_publication(
        &self,
        publication_id: &str,
    ) -> std::result::Result<NovelPublicationRecord, NovelPortError>;

    async fn complete_publication(
        &self,
        publication_id: &str,
        artifact: NovelArtifactReceipt,
    ) -> std::result::Result<CommitReport, NovelPortError>;

    async fn abort_publication(
        &self,
        publication_id: &str,
        reason: &str,
    ) -> std::result::Result<(), NovelPortError>;

    async fn pending_publications(
        &self,
    ) -> std::result::Result<Vec<NovelPublicationRecord>, NovelPortError>;
}

#[async_trait]
pub trait NovelResourcePort: Send + Sync {
    async fn resolve_artifact_path(
        &self,
        path: &Path,
    ) -> std::result::Result<std::path::PathBuf, NovelPortError>;

    async fn read_context(
        &self,
        reference: &ContextRef,
    ) -> std::result::Result<ContextDocument, NovelPortError>;

    async fn write_artifact_atomic(
        &self,
        path: &Path,
        exact_content: &str,
    ) -> std::result::Result<NovelArtifactReceipt, NovelPortError>;

    async fn verify_artifact(
        &self,
        path: &Path,
        expected_sha256: &str,
    ) -> std::result::Result<Option<NovelArtifactReceipt>, NovelPortError>;
}
