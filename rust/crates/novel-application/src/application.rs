use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use novel_domain::{
    build_recall_pack, check_consistency as evaluate_consistency,
    resolve_conflict as resolve_domain_conflict, sha256_hex, ConsistencyReport, MainReviewRecord,
    NovelArtifactReceipt, NovelConversationSource, NovelLifecycleActor, NovelOutcome, NovelProject,
    NovelPublicationRecord, NovelPublicationStatus, NovelRecallPack, NovelResumeInput,
    NovelTaskEvent, NovelTaskPhase, NovelTaskRequest, NovelTaskState, NovelTaskType,
    NovelTransition, PublicationReceipt, UserDecision, UserDecisionRecord,
};
use novel_workflow::{
    NovelContextDocument, NovelStartWorkflow, NovelWorkflowEnvironmentPort, NovelWorkflowPortError,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::{NovelApplicationError, NovelDomainStore, NovelProjectionWorker, Result};

#[async_trait]
pub trait NovelResourcePort: Send + Sync {
    async fn read_context(
        &self,
        reference: &novel_domain::ContextRef,
    ) -> Result<NovelContextDocument>;

    async fn resolve_artifact_path(&self, path: &Path) -> Result<PathBuf>;

    async fn write_artifact_atomic(
        &self,
        path: &Path,
        exact_content: &str,
    ) -> Result<NovelArtifactReceipt>;

    async fn verify_artifact(
        &self,
        path: &Path,
        expected_sha256: &str,
    ) -> Result<Option<NovelArtifactReceipt>>;
}

pub struct StoreWorkflowEnvironment {
    store: Arc<NovelDomainStore>,
    resources: Arc<dyn NovelResourcePort>,
}

impl StoreWorkflowEnvironment {
    #[must_use]
    pub fn new(store: Arc<NovelDomainStore>, resources: Arc<dyn NovelResourcePort>) -> Self {
        Self { store, resources }
    }
}

#[async_trait]
impl NovelWorkflowEnvironmentPort for StoreWorkflowEnvironment {
    async fn load_project(
        &self,
        project_id: &str,
    ) -> std::result::Result<NovelProject, NovelWorkflowPortError> {
        self.store.load_project(project_id).map_err(workflow_error)
    }

    async fn load_checkpoint(
        &self,
        task_id: &str,
    ) -> std::result::Result<Option<novel_domain::NovelTaskCheckpoint>, NovelWorkflowPortError>
    {
        self.store.load_checkpoint(task_id).map_err(workflow_error)
    }

    async fn active_checkpoint_for_project(
        &self,
        project_id: &str,
    ) -> std::result::Result<Option<novel_domain::NovelTaskCheckpoint>, NovelWorkflowPortError>
    {
        self.store
            .active_checkpoint_for_project(project_id)
            .map_err(workflow_error)
    }

    async fn read_context(
        &self,
        reference: &novel_domain::ContextRef,
    ) -> std::result::Result<NovelContextDocument, NovelWorkflowPortError> {
        self.resources
            .read_context(reference)
            .await
            .map_err(workflow_error)
    }

    async fn save_checkpoint(
        &self,
        checkpoint: novel_domain::NovelTaskCheckpoint,
    ) -> std::result::Result<(), NovelWorkflowPortError> {
        self.store
            .save_checkpoint(&checkpoint)
            .map_err(workflow_error)
    }

    async fn append_task_event(
        &self,
        event: NovelTaskEvent,
    ) -> std::result::Result<(), NovelWorkflowPortError> {
        self.store.append_task_event(&event).map_err(workflow_error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelProjectStatusView {
    pub project_id: String,
    pub task_id: Option<String>,
    pub phase: Option<NovelTaskPhase>,
    pub draft_version: u32,
    pub canon_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelApplicationStatus {
    pub projects: Vec<NovelProjectStatusView>,
    pub pending_publications: Vec<NovelPublicationRecord>,
}

#[async_trait]
pub trait TaskApplicationPort: Send + Sync {
    async fn create_project(&self, project: NovelProject) -> Result<NovelProject>;
    async fn list_projects(&self) -> Result<Vec<NovelProject>>;
    async fn recall_project(
        &self,
        project_id: &str,
        task_type: NovelTaskType,
    ) -> Result<NovelRecallPack>;
    async fn check_consistency(&self, project_id: &str) -> Result<ConsistencyReport>;
    async fn resolve_conflict(
        &self,
        project_id: &str,
        conflict_id: &str,
        resolution: &str,
    ) -> Result<NovelProject>;
    async fn start_task(&self, request: NovelTaskRequest) -> Result<NovelOutcome>;
    async fn resume_task(&self, task_id: &str, input: NovelResumeInput) -> Result<NovelOutcome>;
    async fn review_draft(&self, review: MainReviewRecord) -> Result<NovelTransition>;
    async fn user_decision(&self, decision: UserDecisionRecord) -> Result<NovelTransition>;
    async fn publish(&self, task_id: &str, draft_version: u32) -> Result<PublicationReceipt>;
    async fn status(&self, project_id: Option<&str>) -> Result<NovelApplicationStatus>;
    async fn invalidate_conversation_generations(
        &self,
        conversation_id: &str,
        generation_ids: &[String],
        include_unscoped: bool,
    ) -> Result<Vec<String>>;
    async fn associate_conversation_source(
        &self,
        task_id: &str,
        source: NovelConversationSource,
    ) -> Result<()>;
}

pub struct NovelApplicationService {
    store: Arc<NovelDomainStore>,
    workflow: Option<Arc<NovelStartWorkflow>>,
    resources: Arc<dyn NovelResourcePort>,
    projection_worker: Mutex<Option<Arc<NovelProjectionWorker>>>,
    task_locks: Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
}

impl NovelApplicationService {
    #[must_use]
    pub fn new(
        store: Arc<NovelDomainStore>,
        workflow: Option<Arc<NovelStartWorkflow>>,
        resources: Arc<dyn NovelResourcePort>,
    ) -> Self {
        Self {
            store,
            workflow,
            resources,
            projection_worker: Mutex::new(None),
            task_locks: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn attach_projection_worker(&self, worker: Arc<NovelProjectionWorker>) {
        *self
            .projection_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
    }

    pub fn create_project(&self, project: NovelProject) -> Result<NovelProject> {
        self.store.create_project(&project)?;
        self.drain_projections_best_effort();
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<NovelProject>> {
        self.store.list_projects()
    }

    pub fn recall_project(
        &self,
        project_id: &str,
        task_type: NovelTaskType,
    ) -> Result<NovelRecallPack> {
        Ok(build_recall_pack(
            &self.store.load_project(project_id)?,
            task_type,
        ))
    }

    pub fn check_consistency(&self, project_id: &str) -> Result<ConsistencyReport> {
        Ok(evaluate_consistency(&self.store.load_project(project_id)?))
    }

    pub async fn resolve_conflict(
        &self,
        project_id: &str,
        conflict_id: &str,
        resolution: &str,
    ) -> Result<NovelProject> {
        let lock_key = format!("project:{project_id}");
        let project_lock = self.task_lock(&lock_key);
        let _guard = project_lock.lock().await;
        let previous = self.store.load_project(project_id)?;
        let next =
            resolve_domain_conflict(&previous, conflict_id, resolution, current_time_millis())?;
        self.store.update_project(&previous, &next)?;
        self.drain_projections_best_effort();
        Ok(next)
    }

    pub async fn start_task(&self, request: NovelTaskRequest) -> Result<NovelOutcome> {
        let workflow = self.workflow()?;
        Ok(workflow.start_task(request).await?.outcome)
    }

    pub async fn resume_task(
        &self,
        task_id: &str,
        input: NovelResumeInput,
    ) -> Result<NovelOutcome> {
        if input.input.trim().is_empty() {
            return Err(NovelApplicationError::Conflict(
                "resume input must not be empty".into(),
            ));
        }
        let task_lock = self.task_lock(task_id);
        let _guard = task_lock.lock().await;
        let mut state = self.load_state(task_id)?;
        if state.phase != novel_domain::NovelTaskPhase::NeedsClarification {
            return Err(NovelApplicationError::Conflict(format!(
                "resume is allowed only from needs_clarification, current phase={:?}",
                state.phase
            )));
        }
        if let Some(context_refs) = input.context_refs {
            state.refresh_context_refs(context_refs)?;
            self.persist(
                &state,
                NovelLifecycleActor::User,
                "context_refs_refreshed",
                "User accepted refreshed hashes for the same frozen context paths",
                serde_json::json!({
                    "context_refs": state.request.context_refs,
                }),
            )?;
        }
        let workflow = self.workflow()?;
        Ok(Box::pin(workflow.continue_task(task_id, &input.input))
            .await?
            .outcome)
    }

    pub async fn review_draft(&self, review: MainReviewRecord) -> Result<NovelTransition> {
        let task_lock = self.task_lock(&review.task_id);
        let _guard = task_lock.lock().await;
        let mut state = self.load_state(&review.task_id)?;
        let transition = state.record_main_review(review.clone())?;
        self.persist(
            &state,
            NovelLifecycleActor::Main,
            "main_review",
            "Main review recorded",
            serde_json::to_value(&review)?,
        )?;
        if review.verdict == novel_domain::MainReviewVerdict::Revise {
            let instruction = serde_json::to_string(&review)?;
            return Box::pin(self.continue_transition(&review.task_id, &instruction)).await;
        }
        Ok(transition)
    }

    pub async fn user_decision(&self, decision: UserDecisionRecord) -> Result<NovelTransition> {
        let task_lock = self.task_lock(&decision.task_id);
        let _guard = task_lock.lock().await;
        let mut state = self.load_state(&decision.task_id)?;
        let transition = state.record_user_decision(decision.clone())?;
        self.persist(
            &state,
            NovelLifecycleActor::User,
            "user_decision",
            "User decision recorded",
            serde_json::to_value(&decision)?,
        )?;
        if decision.decision == UserDecision::Revise {
            return Box::pin(self.continue_transition(
                &decision.task_id,
                decision.feedback.as_deref().unwrap_or_default(),
            ))
            .await;
        }
        Ok(transition)
    }

    pub async fn publish(&self, task_id: &str, draft_version: u32) -> Result<PublicationReceipt> {
        let task_lock = self.task_lock(task_id);
        let _guard = task_lock.lock().await;
        let mut state = self.load_state(task_id)?;
        if state.phase == NovelTaskPhase::Completed {
            return self.completed_receipt(&state);
        }
        if matches!(
            state.phase,
            NovelTaskPhase::PublicationPending | NovelTaskPhase::ArtifactSavedMemoryPending
        ) {
            return self
                .resume_pending_publication(&mut state, draft_version)
                .await;
        }
        let draft = state.ensure_publishable(draft_version)?.clone();
        let canonical_path = self
            .resources
            .resolve_artifact_path(&state.request.output_path)
            .await?;
        let content_sha256 = sha256_hex(draft.content.as_bytes());
        let publication_id = uuid::Uuid::new_v4().to_string();
        let publication = NovelPublicationRecord::pending(
            &publication_id,
            task_id,
            draft_version,
            canonical_path.to_string_lossy(),
            &content_sha256,
            draft.proposed_delta.clone(),
        );
        self.store.begin_publication(&publication)?;
        state.mark_publication_pending(publication_id.clone());
        self.persist(
            &state,
            NovelLifecycleActor::Memory,
            "publication_pending",
            "Publication journal created",
            serde_json::json!({"publication_id": publication_id, "sha256": content_sha256}),
        )?;

        let artifact = match self
            .resources
            .write_artifact_atomic(&canonical_path, &draft.content)
            .await
        {
            Ok(artifact) => artifact,
            Err(error) => {
                let _ = self
                    .store
                    .abort_publication(&publication_id, &error.to_string());
                state.restore_approved();
                self.store.save_checkpoint(&state.checkpoint()?)?;
                return Err(error);
            }
        };
        state.mark_artifact_saved(artifact.clone());
        self.persist(
            &state,
            NovelLifecycleActor::System,
            "artifact_saved",
            "Reviewed artifact saved atomically",
            serde_json::to_value(&artifact)?,
        )?;
        let report = self
            .store
            .complete_publication(&publication_id, artifact.clone())?;
        state.mark_completed(report.clone());
        self.persist(
            &state,
            NovelLifecycleActor::Memory,
            "canon_committed",
            "Publication completed and Canon committed",
            serde_json::to_value(&report)?,
        )?;
        self.drain_projections_best_effort();
        Ok(PublicationReceipt {
            publication_id,
            task_id: task_id.to_owned(),
            project_id: state.request.project_id,
            draft_version,
            artifact,
            commit_report: report,
        })
    }

    pub fn status(&self, project_filter: Option<&str>) -> Result<NovelApplicationStatus> {
        let mut projects = Vec::new();
        for project in self.store.list_projects()? {
            if project_filter.is_some_and(|filter| filter != project.project_id) {
                continue;
            }
            let checkpoint = self
                .store
                .active_checkpoint_for_project(&project.project_id)?;
            projects.push(NovelProjectStatusView {
                project_id: project.project_id,
                task_id: checkpoint.as_ref().map(|item| item.task_id.clone()),
                phase: checkpoint.as_ref().map(|item| item.phase),
                draft_version: checkpoint.as_ref().map_or(0, |item| item.draft_version),
                canon_revision: project.canon_revision,
            });
        }
        projects.sort_by(|left, right| left.project_id.cmp(&right.project_id));
        Ok(NovelApplicationStatus {
            projects,
            pending_publications: self.store.pending_publications()?,
        })
    }

    pub async fn invalidate_conversation_generations(
        &self,
        conversation_id: &str,
        generation_ids: &[String],
        include_unscoped: bool,
    ) -> Result<Vec<String>> {
        let generations = generation_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut cancelled = Vec::new();
        for checkpoint in self.store.active_checkpoints()? {
            let task_lock = self.task_lock(&checkpoint.task_id);
            let _guard = task_lock.lock().await;
            let mut state = NovelTaskState::from_checkpoint(&checkpoint)?;
            if !state.matches_conversation_generations(
                conversation_id,
                &generations,
                include_unscoped,
            ) {
                continue;
            }
            state.cancel_for_conversation_fork()?;
            self.persist(
                &state,
                NovelLifecycleActor::System,
                "conversation_invalidated",
                "Conversation branch invalidated; unpublished task cancelled",
                serde_json::json!({
                    "conversation_id": conversation_id,
                    "generation_ids": generation_ids,
                }),
            )?;
            cancelled.push(checkpoint.task_id);
        }
        Ok(cancelled)
    }

    pub async fn associate_conversation_source(
        &self,
        task_id: &str,
        source: NovelConversationSource,
    ) -> Result<()> {
        let task_lock = self.task_lock(task_id);
        let _guard = task_lock.lock().await;
        let mut state = self.load_state(task_id)?;
        if state.phase.is_terminal() {
            return Ok(());
        }
        state.associate_conversation_source(source.clone());
        self.persist(
            &state,
            NovelLifecycleActor::System,
            "conversation_associated",
            "Conversation generation associated",
            serde_json::to_value(source)?,
        )
    }

    fn workflow(&self) -> Result<Arc<NovelStartWorkflow>> {
        self.workflow
            .clone()
            .ok_or(NovelApplicationError::WriterUnavailable)
    }

    async fn continue_transition(
        &self,
        task_id: &str,
        instruction: &str,
    ) -> Result<NovelTransition> {
        let workflow = self.workflow()?;
        let outcome = Box::pin(workflow.continue_task(task_id, instruction))
            .await?
            .outcome;
        Ok(match outcome {
            NovelOutcome::DraftReady(draft) => NovelTransition::DraftReady { draft },
            NovelOutcome::NeedsClarification(clarification) => {
                NovelTransition::NeedsClarification { clarification }
            }
        })
    }

    async fn resume_pending_publication(
        &self,
        state: &mut NovelTaskState,
        draft_version: u32,
    ) -> Result<PublicationReceipt> {
        if state.draft_version != draft_version {
            return Err(NovelApplicationError::Conflict(format!(
                "stale publication draft: expected={}, actual={draft_version}",
                state.draft_version
            )));
        }
        let publication_id = state.publication_id.clone().ok_or_else(|| {
            NovelApplicationError::Conflict("publication recovery is missing its id".into())
        })?;
        let draft = state.draft.clone().ok_or_else(|| {
            NovelApplicationError::Conflict("publication recovery is missing its draft".into())
        })?;
        let publication = self.store.load_publication(&publication_id)?;
        if publication.task_id != state.request.task_id
            || publication.draft_version != draft_version
            || publication.content_sha256 != sha256_hex(draft.content.as_bytes())
        {
            return Err(NovelApplicationError::Conflict(
                "publication journal does not match the reviewed draft".into(),
            ));
        }
        if publication.status == NovelPublicationStatus::Completed {
            let artifact = publication.artifact.ok_or_else(|| {
                NovelApplicationError::Conflict("completed publication has no artifact".into())
            })?;
            let report = publication.commit_report.ok_or_else(|| {
                NovelApplicationError::Conflict("completed publication has no report".into())
            })?;
            state.mark_artifact_saved(artifact.clone());
            state.mark_completed(report.clone());
            self.store.save_checkpoint(&state.checkpoint()?)?;
            return Ok(PublicationReceipt {
                publication_id,
                task_id: state.request.task_id.clone(),
                project_id: state.request.project_id.clone(),
                draft_version,
                artifact,
                commit_report: report,
            });
        }
        if !matches!(
            publication.status,
            NovelPublicationStatus::Pending | NovelPublicationStatus::ArtifactSaved
        ) {
            return Err(NovelApplicationError::Conflict(format!(
                "publication {} cannot resume from {:?}",
                publication.publication_id, publication.status
            )));
        }
        let path = PathBuf::from(&publication.output_path);
        let artifact = match self
            .resources
            .verify_artifact(&path, &publication.content_sha256)
            .await?
        {
            Some(artifact) => artifact,
            None => {
                self.resources
                    .write_artifact_atomic(&path, &draft.content)
                    .await?
            }
        };
        state.mark_artifact_saved(artifact.clone());
        self.store.save_checkpoint(&state.checkpoint()?)?;
        let report = self
            .store
            .complete_publication(&publication_id, artifact.clone())?;
        state.mark_completed(report.clone());
        self.store.save_checkpoint(&state.checkpoint()?)?;
        Ok(PublicationReceipt {
            publication_id,
            task_id: state.request.task_id.clone(),
            project_id: state.request.project_id.clone(),
            draft_version,
            artifact,
            commit_report: report,
        })
    }

    fn completed_receipt(&self, state: &NovelTaskState) -> Result<PublicationReceipt> {
        let publication_id = state.publication_id.as_deref().ok_or_else(|| {
            NovelApplicationError::Conflict("completed task has no publication id".into())
        })?;
        let publication = self.store.load_publication(publication_id)?;
        let artifact = publication.artifact.ok_or_else(|| {
            NovelApplicationError::Conflict("completed publication has no artifact".into())
        })?;
        let commit_report = publication.commit_report.ok_or_else(|| {
            NovelApplicationError::Conflict("completed publication has no report".into())
        })?;
        Ok(PublicationReceipt {
            publication_id: publication_id.to_owned(),
            task_id: state.request.task_id.clone(),
            project_id: state.request.project_id.clone(),
            draft_version: state.draft_version,
            artifact,
            commit_report,
        })
    }

    fn persist(
        &self,
        state: &NovelTaskState,
        actor: NovelLifecycleActor,
        event_kind: &str,
        summary: &str,
        details: serde_json::Value,
    ) -> Result<()> {
        let event = NovelTaskEvent {
            event_id: format!(
                "novel-application-{}-{event_kind}-{}",
                state.request.task_id,
                uuid::Uuid::new_v4()
            ),
            task_id: state.request.task_id.clone(),
            project_id: state.request.project_id.clone(),
            actor,
            phase: state.phase,
            summary: summary.to_owned(),
            details,
            created_at: state.updated_at,
        };
        self.store.persist_state_event(state, &event)
    }

    fn load_state(&self, task_id: &str) -> Result<NovelTaskState> {
        let checkpoint = self
            .store
            .load_checkpoint(task_id)?
            .ok_or_else(|| NovelApplicationError::NotFound(format!("task {task_id}")))?;
        NovelTaskState::from_checkpoint(&checkpoint).map_err(Into::into)
    }

    fn task_lock(&self, task_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .task_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            locks
                .entry(task_id.to_owned())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    fn drain_projections_best_effort(&self) {
        let worker = self
            .projection_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(worker) = worker {
            let _result = worker.drain(128);
        }
    }
}

#[async_trait]
impl TaskApplicationPort for NovelApplicationService {
    async fn create_project(&self, project: NovelProject) -> Result<NovelProject> {
        NovelApplicationService::create_project(self, project)
    }

    async fn list_projects(&self) -> Result<Vec<NovelProject>> {
        NovelApplicationService::list_projects(self)
    }

    async fn recall_project(
        &self,
        project_id: &str,
        task_type: NovelTaskType,
    ) -> Result<NovelRecallPack> {
        NovelApplicationService::recall_project(self, project_id, task_type)
    }

    async fn check_consistency(&self, project_id: &str) -> Result<ConsistencyReport> {
        NovelApplicationService::check_consistency(self, project_id)
    }

    async fn resolve_conflict(
        &self,
        project_id: &str,
        conflict_id: &str,
        resolution: &str,
    ) -> Result<NovelProject> {
        NovelApplicationService::resolve_conflict(self, project_id, conflict_id, resolution).await
    }

    async fn start_task(&self, request: NovelTaskRequest) -> Result<NovelOutcome> {
        NovelApplicationService::start_task(self, request).await
    }

    async fn resume_task(&self, task_id: &str, input: NovelResumeInput) -> Result<NovelOutcome> {
        Box::pin(NovelApplicationService::resume_task(self, task_id, input)).await
    }

    async fn review_draft(&self, review: MainReviewRecord) -> Result<NovelTransition> {
        Box::pin(NovelApplicationService::review_draft(self, review)).await
    }

    async fn user_decision(&self, decision: UserDecisionRecord) -> Result<NovelTransition> {
        Box::pin(NovelApplicationService::user_decision(self, decision)).await
    }

    async fn publish(&self, task_id: &str, draft_version: u32) -> Result<PublicationReceipt> {
        NovelApplicationService::publish(self, task_id, draft_version).await
    }

    async fn status(&self, project_id: Option<&str>) -> Result<NovelApplicationStatus> {
        NovelApplicationService::status(self, project_id)
    }

    async fn invalidate_conversation_generations(
        &self,
        conversation_id: &str,
        generation_ids: &[String],
        include_unscoped: bool,
    ) -> Result<Vec<String>> {
        NovelApplicationService::invalidate_conversation_generations(
            self,
            conversation_id,
            generation_ids,
            include_unscoped,
        )
        .await
    }

    async fn associate_conversation_source(
        &self,
        task_id: &str,
        source: NovelConversationSource,
    ) -> Result<()> {
        NovelApplicationService::associate_conversation_source(self, task_id, source).await
    }
}

fn workflow_error(error: NovelApplicationError) -> NovelWorkflowPortError {
    match error {
        NovelApplicationError::ContextChanged(message) => {
            NovelWorkflowPortError::ContextChanged(message)
        }
        NovelApplicationError::ContextHashChanged {
            path,
            expected,
            actual,
        } => NovelWorkflowPortError::ContextHashChanged {
            path,
            expected,
            actual,
        },
        other => NovelWorkflowPortError::Storage(other.to_string()),
    }
}

fn current_time_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_error_preserves_context_changed_variant() {
        let error = workflow_error(NovelApplicationError::ContextChanged(
            "expected=old, actual=new".into(),
        ));

        assert!(matches!(
            error,
            NovelWorkflowPortError::ContextChanged(message)
                if message == "expected=old, actual=new"
        ));
    }

    #[test]
    fn workflow_error_preserves_typed_context_hash_variant() {
        let error = workflow_error(NovelApplicationError::ContextHashChanged {
            path: "outline.md".into(),
            expected: "old".into(),
            actual: "new".into(),
        });

        assert!(matches!(
            error,
            NovelWorkflowPortError::ContextHashChanged { path, expected, actual }
                if path == "outline.md" && expected == "old" && actual == "new"
        ));
    }
}
