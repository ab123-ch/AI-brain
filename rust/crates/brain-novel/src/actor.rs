use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use brain_llm::{ChatMessage, LlmProvider};
use brain_memory::novel::{
    NovelLifecycleActor, NovelProject, NovelPublicationRecord, NovelPublicationStatus,
    NovelTaskEvent, NovelTaskPhase,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, watch};

use crate::handle::NovelCommand;
use crate::runtime::NovelModelRuntime;
use crate::{
    MainReviewRecord, NovelBrainError, NovelBrainEvent, NovelBrainEventKind, NovelBrainHandle,
    NovelBrainStatus, NovelConversationSource, NovelMemoryPort, NovelOutcome,
    NovelProjectStatusView, NovelResourcePort, NovelResumeInput, NovelTaskRequest, NovelTaskState,
    NovelTransition, PublicationReceipt, ResidentBrainStatus, Result, UserDecision,
    UserDecisionRecord,
};

#[derive(Debug, Clone)]
pub struct NovelBrainConfig {
    pub command_capacity: usize,
    pub event_capacity: usize,
    pub max_history_messages: usize,
    pub max_hot_projects: usize,
    /// Keeps the resident service observable when its configured LLM cannot be created.
    pub startup_error: Option<String>,
}

impl Default for NovelBrainConfig {
    fn default() -> Self {
        Self {
            command_capacity: 32,
            event_capacity: 128,
            max_history_messages: 12,
            max_hot_projects: 8,
            startup_error: None,
        }
    }
}

struct ProjectWorkspace {
    project: NovelProject,
    history: VecDeque<ChatMessage>,
    active_task: Option<NovelTaskState>,
    last_accessed_at: i64,
}

pub fn spawn_novel_brain(
    llm: Arc<dyn LlmProvider>,
    memory: Arc<dyn NovelMemoryPort>,
    resources: Arc<dyn NovelResourcePort>,
    config: NovelBrainConfig,
) -> (NovelBrainHandle, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(config.command_capacity.max(1));
    let (events, _) = broadcast::channel(config.event_capacity.max(1));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let handle = NovelBrainHandle::new(tx, events.clone(), shutdown);
    let actor = NovelBrain {
        runtime: NovelModelRuntime::new(llm),
        memory,
        resources,
        projects: HashMap::new(),
        events,
        status: ResidentBrainStatus::Starting,
        last_error: None,
        config,
    };
    let task = tokio::spawn(actor.run(rx, shutdown_rx));
    (handle, task)
}

struct NovelBrain {
    runtime: NovelModelRuntime,
    memory: Arc<dyn NovelMemoryPort>,
    resources: Arc<dyn NovelResourcePort>,
    projects: HashMap<String, ProjectWorkspace>,
    events: broadcast::Sender<NovelBrainEvent>,
    status: ResidentBrainStatus,
    last_error: Option<String>,
    config: NovelBrainConfig,
}

impl NovelBrain {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<NovelCommand>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        match self.recover().await {
            Ok(()) => {
                if let Some(error) = self.config.startup_error.clone() {
                    self.status = ResidentBrainStatus::Degraded;
                    self.last_error = Some(error.clone());
                    self.emit(
                        NovelBrainEventKind::Degraded,
                        None,
                        None,
                        &format!("常驻小说脑已启动，但 LLM 不可用: {error}"),
                    );
                } else {
                    self.status = ResidentBrainStatus::Ready;
                    self.emit(
                        NovelBrainEventKind::Activated,
                        None,
                        None,
                        "常驻小说脑已启动",
                    );
                }
            }
            Err(error) => {
                self.status = ResidentBrainStatus::Degraded;
                self.last_error = Some(error.to_string());
                self.emit(
                    NovelBrainEventKind::Degraded,
                    None,
                    None,
                    &format!("小说脑以降级状态启动: {error}"),
                );
            }
        }

        loop {
            let command = tokio::select! {
                biased;
                _ = shutdown_rx.changed() => {
                    self.shutdown().await;
                    break;
                }
                command = rx.recv() => {
                    let Some(command) = command else {
                        self.shutdown().await;
                        break;
                    };
                    command
                }
            };
            match command {
                NovelCommand::StartTask { request, reply } => {
                    let _ = reply.send(self.start_task(request).await);
                }
                NovelCommand::ResumeTask {
                    task_id,
                    input,
                    reply,
                } => {
                    let _ = reply.send(self.resume_task(&task_id, input).await);
                }
                NovelCommand::ReviewDraft { review, reply } => {
                    let _ = reply.send(self.review_draft(review).await);
                }
                NovelCommand::UserDecision { decision, reply } => {
                    let _ = reply.send(self.user_decision(decision).await);
                }
                NovelCommand::Publish {
                    task_id,
                    draft_version,
                    reply,
                } => {
                    let _ = reply.send(self.publish(&task_id, draft_version).await);
                }
                NovelCommand::Status { project_id, reply } => {
                    let _ = reply.send(self.status(project_id.as_deref()).await);
                }
                NovelCommand::InvalidateConversation {
                    conversation_id,
                    generation_ids,
                    include_unscoped,
                    reply,
                } => {
                    let _ = reply.send(
                        self.invalidate_conversation_generations(
                            &conversation_id,
                            &generation_ids,
                            include_unscoped,
                        )
                        .await,
                    );
                }
                NovelCommand::AssociateConversationSource {
                    task_id,
                    source,
                    reply,
                } => {
                    let _ = reply.send(self.associate_conversation_source(&task_id, source).await);
                }
                NovelCommand::Shutdown { reply } => {
                    self.shutdown().await;
                    let _ = reply.send(());
                    break;
                }
            }
        }
    }

    async fn recover(&mut self) -> Result<()> {
        for checkpoint in self.memory.active_checkpoints().await? {
            let state = NovelTaskState::from_checkpoint(&checkpoint)?;
            let snapshot = self.memory.load_workspace(&checkpoint.project_id).await?;
            self.projects.insert(
                checkpoint.project_id.clone(),
                ProjectWorkspace {
                    project: snapshot.project,
                    history: VecDeque::new(),
                    active_task: Some(state),
                    last_accessed_at: checkpoint.updated_at,
                },
            );
        }

        let checkpointed_publications = self
            .projects
            .iter()
            .filter_map(|(project_id, workspace)| {
                let state = workspace.active_task.as_ref()?;
                matches!(
                    state.phase,
                    NovelTaskPhase::PublicationPending | NovelTaskPhase::ArtifactSavedMemoryPending
                )
                .then(|| {
                    state.publication_id.as_ref().map(|publication_id| {
                        (
                            project_id.clone(),
                            state.request.task_id.clone(),
                            publication_id.clone(),
                        )
                    })
                })?
            })
            .collect::<Vec<_>>();
        for (project_id, task_id, publication_id) in checkpointed_publications {
            let publication = self.memory.load_publication(&publication_id).await?;
            if publication.status != NovelPublicationStatus::Completed {
                continue;
            }
            let report = publication.commit_report.ok_or_else(|| {
                NovelBrainError::InvalidTransition(format!(
                    "completed publication {publication_id} 缺少 commit report"
                ))
            })?;
            let state = self.task_mut(&project_id, &task_id)?;
            if let Some(artifact) = publication.artifact {
                state.mark_artifact_saved(artifact);
            }
            state.mark_completed(report.clone());
            let snapshot = state.clone();
            self.persist_state(
                &snapshot,
                NovelLifecycleActor::Memory,
                "恢复已完成 publication 的任务 checkpoint",
                serde_json::to_value(&report)?,
            )
            .await?;
            if let Some(workspace) = self.projects.get_mut(&project_id) {
                workspace.project.canon_revision = report.new_revision;
            }
        }

        for publication in self.memory.pending_publications().await? {
            let checkpoint = self
                .memory
                .load_checkpoint(&publication.task_id)
                .await?
                .ok_or_else(|| NovelBrainError::TaskNotFound(publication.task_id.clone()))?;
            let mut state = NovelTaskState::from_checkpoint(&checkpoint)?;
            match state.publication_id.as_deref() {
                Some(publication_id) if publication_id == publication.publication_id => {}
                None => {
                    self.reconcile_publication_start(&mut state, &publication)
                        .await?;
                    self.persist_state(
                        &state,
                        NovelLifecycleActor::Memory,
                        "恢复已建立 publication 但尚未保存的 pending checkpoint",
                        json!({"publication_id": publication.publication_id}),
                    )
                    .await?;
                    let workspace =
                        self.projects
                            .get_mut(&publication.project_id)
                            .ok_or_else(|| {
                                NovelBrainError::InvalidTransition(format!(
                                    "pending publication {} 对应项目未加载",
                                    publication.publication_id
                                ))
                            })?;
                    workspace.active_task = Some(state.clone());
                }
                Some(_) => {
                    return Err(NovelBrainError::InvalidTransition(format!(
                        "pending publication {} 与 checkpoint publication_id 不一致",
                        publication.publication_id
                    )));
                }
            }
            let draft = state.draft.as_ref().ok_or_else(|| {
                NovelBrainError::InvalidTransition(format!(
                    "pending publication {} 缺少可恢复草稿",
                    publication.publication_id
                ))
            })?;
            if draft.draft_version != publication.draft_version
                || sha256(draft.content.as_bytes()) != publication.content_sha256
            {
                return Err(NovelBrainError::InvalidTransition(format!(
                    "pending publication {} 与 checkpoint 草稿不一致",
                    publication.publication_id
                )));
            }
            let path = PathBuf::from(&publication.output_path);
            let artifact = if let Some(artifact) = self
                .resources
                .verify_artifact(&path, &publication.content_sha256)
                .await?
            {
                artifact
            } else {
                self.resources
                    .write_artifact_atomic(&path, &draft.content)
                    .await?
            };
            let report = self
                .memory
                .complete_publication(&publication.publication_id, artifact.clone())
                .await?;
            state.mark_artifact_saved(artifact);
            state.mark_completed(report.clone());
            self.persist_state(
                &state,
                NovelLifecycleActor::Memory,
                "启动恢复完成 pending publication",
                serde_json::to_value(&report)?,
            )
            .await?;
            if let Some(workspace) = self.projects.get_mut(&publication.project_id) {
                workspace.project.canon_revision = report.new_revision;
                workspace.active_task = Some(state);
            }
        }
        Ok(())
    }

    async fn reconcile_publication_start(
        &self,
        state: &mut NovelTaskState,
        publication: &NovelPublicationRecord,
    ) -> Result<()> {
        if publication.status != NovelPublicationStatus::Pending
            || publication.artifact.is_some()
            || publication.commit_report.is_some()
            || publication.error.is_some()
            || publication.task_id != state.request.task_id
            || publication.project_id != state.request.project_id
            || publication.expected_revision != state.request.expected_revision
        {
            return Err(NovelBrainError::InvalidTransition(format!(
                "pending publication {} 不能与发布前 checkpoint 对账",
                publication.publication_id
            )));
        }

        let draft = state.ensure_publishable(publication.draft_version)?;
        let canonical_path = self
            .resources
            .resolve_artifact_path(&state.request.output_path)
            .await?;
        let delta_matches = serde_json::to_value(&draft.proposed_delta)?
            == serde_json::to_value(&publication.delta)?;
        if canonical_path != PathBuf::from(&publication.output_path)
            || sha256(draft.content.as_bytes()) != publication.content_sha256
            || !delta_matches
        {
            return Err(NovelBrainError::InvalidTransition(format!(
                "pending publication {} 与审核草稿内容不一致",
                publication.publication_id
            )));
        }

        state.mark_publication_pending(publication.publication_id.clone());
        Ok(())
    }

    async fn start_task(&mut self, request: NovelTaskRequest) -> Result<NovelOutcome> {
        if self.find_task_project(&request.task_id).is_some()
            || self
                .memory
                .load_checkpoint(&request.task_id)
                .await?
                .is_some()
        {
            return Err(NovelBrainError::InvalidRequest(format!(
                "task_id 已存在: {}",
                request.task_id
            )));
        }
        self.ensure_project_loaded(&request.project_id).await?;
        let workspace = self
            .projects
            .get_mut(&request.project_id)
            .ok_or_else(|| NovelBrainError::InvalidRequest("项目工作区加载失败".into()))?;
        if workspace.project.canon_revision != request.expected_revision {
            return Err(NovelBrainError::StaleRevision {
                expected: request.expected_revision,
                actual: workspace.project.canon_revision,
            });
        }
        if workspace
            .active_task
            .as_ref()
            .is_some_and(|state| !state.phase.is_terminal())
        {
            return Err(NovelBrainError::ProjectBusy(request.project_id));
        }
        let mut state = NovelTaskState::new(request)?;
        state.begin_drafting()?;
        let project_id = state.request.project_id.clone();
        workspace.active_task = Some(state.clone());
        workspace.last_accessed_at = crate::types::now_millis();
        self.persist_state(
            &state,
            NovelLifecycleActor::Main,
            "主脑创建小说任务合同",
            json!({"task_brief": state.request.task_brief}),
        )
        .await?;
        self.emit(
            NovelBrainEventKind::TaskStarted,
            Some(&project_id),
            Some(&state.request.task_id),
            "小说任务已进入常驻工作区",
        );
        self.generate(&project_id, None).await
    }

    async fn resume_task(
        &mut self,
        task_id: &str,
        input: NovelResumeInput,
    ) -> Result<NovelOutcome> {
        if input.input.trim().is_empty() {
            return Err(NovelBrainError::InvalidRequest(
                "resume input 不能为空".into(),
            ));
        }
        let project_id = self.ensure_task_loaded(task_id).await?;
        let state = self.task_mut(&project_id, task_id)?;
        state.begin_drafting()?;
        let snapshot = state.clone();
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::Main,
            "主脑转交用户澄清或补充信息",
            json!({"input": input.input}),
        )
        .await?;
        self.emit(
            NovelBrainEventKind::TaskResumed,
            Some(&project_id),
            Some(task_id),
            "小说任务在原常驻会话中继续",
        );
        self.generate(&project_id, Some(&input.input)).await
    }

    async fn review_draft(&mut self, review: MainReviewRecord) -> Result<NovelTransition> {
        let project_id = self.ensure_task_loaded(&review.task_id).await?;
        let state = self.task_mut(&project_id, &review.task_id)?;
        let transition = state.record_main_review(review.clone())?;
        let snapshot = state.clone();
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::Main,
            "主脑复审已记录",
            serde_json::to_value(&review)?,
        )
        .await?;
        self.emit(
            NovelBrainEventKind::MainReviewRecorded,
            Some(&project_id),
            Some(&review.task_id),
            "主脑独立复审已写入记忆脑",
        );
        if review.verdict == crate::MainReviewVerdict::Revise {
            self.emit(
                NovelBrainEventKind::RevisionStarted,
                Some(&project_id),
                Some(&review.task_id),
                "小说脑按主脑意见修订",
            );
            let instruction = serde_json::to_string_pretty(&review)?;
            return self
                .generate_transition(&project_id, Some(&instruction))
                .await;
        }
        Ok(transition)
    }

    async fn user_decision(&mut self, decision: UserDecisionRecord) -> Result<NovelTransition> {
        let project_id = self.ensure_task_loaded(&decision.task_id).await?;
        let state = self.task_mut(&project_id, &decision.task_id)?;
        let transition = state.record_user_decision(decision.clone())?;
        let snapshot = state.clone();
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::User,
            "用户候选稿决策已记录",
            serde_json::to_value(&decision)?,
        )
        .await?;
        self.emit(
            NovelBrainEventKind::UserDecisionRecorded,
            Some(&project_id),
            Some(&decision.task_id),
            "用户决策已写入记忆脑",
        );
        if decision.decision == UserDecision::Revise {
            self.emit(
                NovelBrainEventKind::RevisionStarted,
                Some(&project_id),
                Some(&decision.task_id),
                "小说脑按用户意见修订",
            );
            return self
                .generate_transition(&project_id, decision.feedback.as_deref())
                .await;
        }
        Ok(transition)
    }

    async fn publish(&mut self, task_id: &str, draft_version: u32) -> Result<PublicationReceipt> {
        let project_id = self.ensure_task_loaded(task_id).await?;
        let phase = self.task(&project_id, task_id)?.phase;
        if matches!(
            phase,
            NovelTaskPhase::PublicationPending | NovelTaskPhase::ArtifactSavedMemoryPending
        ) {
            return self
                .resume_pending_publication(&project_id, task_id, draft_version)
                .await;
        }
        let (draft, requested_path) = {
            let state = self.task(&project_id, task_id)?;
            (
                state.ensure_publishable(draft_version)?.clone(),
                state.request.output_path.clone(),
            )
        };
        let canonical_path = self
            .resources
            .resolve_artifact_path(&requested_path)
            .await?;
        let content_sha256 = sha256(draft.content.as_bytes());
        let publication_id = uuid::Uuid::new_v4().to_string();
        let record = NovelPublicationRecord::pending(
            &publication_id,
            task_id,
            draft_version,
            canonical_path.to_string_lossy(),
            &content_sha256,
            draft.proposed_delta.clone(),
        );
        self.memory.begin_publication(record).await?;
        let state = self.task_mut(&project_id, task_id)?;
        state.mark_publication_pending(publication_id.clone());
        let snapshot = state.clone();
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::Memory,
            "发布事务已开始",
            json!({"publication_id": publication_id, "sha256": content_sha256}),
        )
        .await?;
        self.emit(
            NovelBrainEventKind::PublicationPending,
            Some(&project_id),
            Some(task_id),
            "MemoryBrain 已建立发布事务",
        );

        let artifact = match self
            .resources
            .write_artifact_atomic(&canonical_path, &draft.content)
            .await
        {
            Ok(artifact) => artifact,
            Err(error) => {
                let _ = self
                    .memory
                    .abort_publication(&publication_id, &error.to_string())
                    .await;
                let state = self.task_mut(&project_id, task_id)?;
                state.restore_approved();
                let snapshot = state.clone();
                self.memory.save_checkpoint(snapshot.checkpoint()?).await?;
                return Err(error.into());
            }
        };
        let state = self.task_mut(&project_id, task_id)?;
        state.mark_artifact_saved(artifact.clone());
        let snapshot = state.clone();
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::System,
            "已原子写入审核过的作品文件",
            serde_json::to_value(&artifact)?,
        )
        .await?;
        self.emit(
            NovelBrainEventKind::ArtifactSaved,
            Some(&project_id),
            Some(task_id),
            "审核正文已原子保存",
        );

        let report = self
            .memory
            .complete_publication(&publication_id, artifact.clone())
            .await?;
        let state = self.task_mut(&project_id, task_id)?;
        state.mark_completed(report.clone());
        let snapshot = state.clone();
        if let Some(workspace) = self.projects.get_mut(&project_id) {
            workspace.project.canon_revision = report.new_revision;
        }
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::Memory,
            "发布完成并提交 Confirmed Canon",
            serde_json::to_value(&report)?,
        )
        .await?;
        self.emit(
            NovelBrainEventKind::CanonCommitted,
            Some(&project_id),
            Some(task_id),
            "MemoryBrain 已完成 Canon 提交",
        );
        Ok(PublicationReceipt {
            publication_id,
            task_id: task_id.into(),
            project_id,
            draft_version,
            artifact,
            commit_report: report,
        })
    }

    async fn resume_pending_publication(
        &mut self,
        project_id: &str,
        task_id: &str,
        draft_version: u32,
    ) -> Result<PublicationReceipt> {
        let (publication_id, draft) = {
            let state = self.task(project_id, task_id)?;
            if state.draft_version != draft_version {
                return Err(NovelBrainError::InvalidTransition(format!(
                    "发布草稿版本已过期: expected={}, actual={draft_version}",
                    state.draft_version
                )));
            }
            let publication_id = state.publication_id.clone().ok_or_else(|| {
                NovelBrainError::InvalidTransition("发布恢复状态缺少 publication_id".into())
            })?;
            let draft = state.draft.clone().ok_or_else(|| {
                NovelBrainError::InvalidTransition("发布恢复状态缺少已审核草稿".into())
            })?;
            (publication_id, draft)
        };
        let publication = self.memory.load_publication(&publication_id).await?;
        if publication.task_id != task_id
            || publication.draft_version != draft_version
            || publication.content_sha256 != sha256(draft.content.as_bytes())
        {
            return Err(NovelBrainError::InvalidTransition(
                "publication journal 与待恢复草稿不一致".into(),
            ));
        }
        if publication.status == NovelPublicationStatus::Completed {
            let artifact = publication.artifact.ok_or_else(|| {
                NovelBrainError::InvalidTransition("已完成 publication 缺少 artifact".into())
            })?;
            let report = publication.commit_report.ok_or_else(|| {
                NovelBrainError::InvalidTransition("已完成 publication 缺少 commit report".into())
            })?;
            let state = self.task_mut(project_id, task_id)?;
            state.mark_artifact_saved(artifact.clone());
            state.mark_completed(report.clone());
            let snapshot = state.clone();
            self.persist_state(
                &snapshot,
                NovelLifecycleActor::Memory,
                "同步已完成 publication 状态",
                serde_json::to_value(&report)?,
            )
            .await?;
            return Ok(PublicationReceipt {
                publication_id,
                task_id: task_id.into(),
                project_id: project_id.into(),
                draft_version,
                artifact,
                commit_report: report,
            });
        }
        if !matches!(
            publication.status,
            NovelPublicationStatus::Pending | NovelPublicationStatus::ArtifactSaved
        ) {
            return Err(NovelBrainError::InvalidTransition(format!(
                "publication {} 当前状态不可恢复: {:?}",
                publication.publication_id, publication.status
            )));
        }
        let path = PathBuf::from(&publication.output_path);
        let artifact = if let Some(artifact) = self
            .resources
            .verify_artifact(&path, &publication.content_sha256)
            .await?
        {
            artifact
        } else {
            self.resources
                .write_artifact_atomic(&path, &draft.content)
                .await?
        };
        let state = self.task_mut(project_id, task_id)?;
        state.mark_artifact_saved(artifact.clone());
        let snapshot = state.clone();
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::System,
            "恢复发布事务的 artifact",
            serde_json::to_value(&artifact)?,
        )
        .await?;
        let report = self
            .memory
            .complete_publication(&publication_id, artifact.clone())
            .await?;
        let state = self.task_mut(project_id, task_id)?;
        state.mark_completed(report.clone());
        let snapshot = state.clone();
        if let Some(workspace) = self.projects.get_mut(project_id) {
            workspace.project.canon_revision = report.new_revision;
        }
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::Memory,
            "恢复发布事务并提交 Confirmed Canon",
            serde_json::to_value(&report)?,
        )
        .await?;
        Ok(PublicationReceipt {
            publication_id,
            task_id: task_id.into(),
            project_id: project_id.into(),
            draft_version,
            artifact,
            commit_report: report,
        })
    }

    async fn status(&self, project_filter: Option<&str>) -> Result<NovelBrainStatus> {
        let mut projects = self
            .projects
            .values()
            .filter(|workspace| {
                project_filter.is_none_or(|filter| workspace.project.project_id == filter)
            })
            .map(|workspace| NovelProjectStatusView {
                project_id: workspace.project.project_id.clone(),
                task_id: workspace
                    .active_task
                    .as_ref()
                    .map(|state| state.request.task_id.clone()),
                phase: workspace.active_task.as_ref().map(|state| state.phase),
                draft_version: workspace
                    .active_task
                    .as_ref()
                    .map_or(0, |state| state.draft_version),
                canon_revision: workspace.project.canon_revision,
                history_messages: workspace.history.len(),
            })
            .collect::<Vec<_>>();
        projects.sort_by(|left, right| left.project_id.cmp(&right.project_id));
        Ok(NovelBrainStatus {
            brain_id: "novel".into(),
            status: self.status,
            projects,
            pending_publications: self.memory.pending_publications().await?,
            last_error: self.last_error.clone(),
        })
    }

    async fn invalidate_conversation_generations(
        &mut self,
        conversation_id: &str,
        generation_ids: &[String],
        include_unscoped: bool,
    ) -> Result<Vec<String>> {
        let generations = generation_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let project_ids = self
            .projects
            .iter()
            .filter_map(|(project_id, workspace)| {
                let state = workspace.active_task.as_ref()?;
                if state.phase.is_terminal() {
                    return None;
                }
                state
                    .matches_conversation_generations(
                        conversation_id,
                        &generations,
                        include_unscoped,
                    )
                    .then(|| project_id.clone())
            })
            .collect::<Vec<_>>();

        let mut cancelled = Vec::new();
        for project_id in project_ids {
            let mut state = self
                .projects
                .get_mut(&project_id)
                .and_then(|workspace| workspace.active_task.take())
                .ok_or_else(|| NovelBrainError::TaskNotFound(project_id.clone()))?;
            if let Err(error) = state.cancel_for_conversation_fork() {
                self.projects
                    .get_mut(&project_id)
                    .expect("project exists")
                    .active_task = Some(state);
                return Err(error);
            }
            let task_id = state.request.task_id.clone();
            if let Err(error) = self
                .persist_state(
                    &state,
                    NovelLifecycleActor::System,
                    "Web 会话分叉，旧分支小说任务已取消",
                    json!({
                        "conversation_id": conversation_id,
                        "invalidated_generations": generation_ids,
                    }),
                )
                .await
            {
                self.projects
                    .get_mut(&project_id)
                    .expect("project exists")
                    .active_task = Some(state);
                return Err(error);
            }
            self.emit(
                NovelBrainEventKind::TaskCancelled,
                Some(&project_id),
                Some(&task_id),
                "旧会话分支对应的未发布小说任务已取消",
            );
            cancelled.push(task_id);
        }
        Ok(cancelled)
    }

    async fn associate_conversation_source(
        &mut self,
        task_id: &str,
        source: NovelConversationSource,
    ) -> Result<()> {
        let project_id = self.ensure_task_loaded(task_id).await?;
        let state = self.task_mut(&project_id, task_id)?;
        if state.phase.is_terminal() {
            return Ok(());
        }
        state.associate_conversation_source(source.clone());
        let snapshot = state.clone();
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::System,
            "记录小说任务对应的 Web 会话代次",
            serde_json::to_value(source)?,
        )
        .await
    }

    async fn generate(
        &mut self,
        project_id: &str,
        revision_instruction: Option<&str>,
    ) -> Result<NovelOutcome> {
        if let Some(error) = &self.config.startup_error {
            return Err(NovelBrainError::Model(format!(
                "常驻小说脑 LLM 不可用: {error}"
            )));
        }
        let (task, project, history) = {
            let workspace = self.projects.get(project_id).ok_or_else(|| {
                NovelBrainError::InvalidRequest(format!("项目工作区不存在: {project_id}"))
            })?;
            let task = workspace
                .active_task
                .clone()
                .ok_or_else(|| NovelBrainError::TaskNotFound(project_id.into()))?;
            (
                task,
                workspace.project.clone(),
                workspace.history.iter().cloned().collect::<Vec<_>>(),
            )
        };
        let recall = self
            .memory
            .recall_project(project_id, task.request.task_type.clone())
            .await?;
        self.emit(
            NovelBrainEventKind::MemoryRecalled,
            Some(project_id),
            Some(&task.request.task_id),
            "小说脑已从 MemoryBrain 召回 Canon",
        );
        let consistency = self.memory.check_consistency(project_id).await?;
        self.emit(
            NovelBrainEventKind::ConsistencyChecked,
            Some(project_id),
            Some(&task.request.task_id),
            "小说脑已读取一致性报告",
        );
        let mut documents = Vec::with_capacity(task.request.context_refs.len());
        for reference in &task.request.context_refs {
            documents.push(self.resources.read_context(reference).await?);
        }
        let generated = self
            .runtime
            .generate(
                &task,
                &project,
                &recall,
                &consistency,
                &documents,
                &history,
                revision_instruction,
            )
            .await?;
        let outcome = generated.outcome;
        let snapshot = {
            let workspace = self.projects.get_mut(project_id).ok_or_else(|| {
                NovelBrainError::InvalidRequest(format!("项目工作区不存在: {project_id}"))
            })?;
            workspace.history.push_back(generated.user_message);
            workspace.history.push_back(generated.assistant_message);
            while workspace.history.len() > self.config.max_history_messages.max(2) {
                workspace.history.pop_front();
            }
            workspace.last_accessed_at = crate::types::now_millis();
            let state = workspace
                .active_task
                .as_mut()
                .ok_or_else(|| NovelBrainError::TaskNotFound(task.request.task_id.clone()))?;
            state.apply_outcome(outcome.clone())?;
            state.clone()
        };
        let (summary, kind) = match &outcome {
            NovelOutcome::NeedsClarification(_) => (
                "小说脑需要用户补充关键信息",
                NovelBrainEventKind::ClarificationRequested,
            ),
            NovelOutcome::DraftReady(_) => {
                ("小说脑完成草稿与六项自检", NovelBrainEventKind::DraftReady)
            }
        };
        self.persist_state(
            &snapshot,
            NovelLifecycleActor::Novel,
            summary,
            serde_json::to_value(&outcome)?,
        )
        .await?;
        self.emit(
            kind,
            Some(project_id),
            Some(&snapshot.request.task_id),
            summary,
        );
        Ok(outcome)
    }

    async fn generate_transition(
        &mut self,
        project_id: &str,
        revision_instruction: Option<&str>,
    ) -> Result<NovelTransition> {
        match self.generate(project_id, revision_instruction).await? {
            NovelOutcome::DraftReady(draft) => Ok(NovelTransition::DraftReady { draft }),
            NovelOutcome::NeedsClarification(clarification) => {
                Ok(NovelTransition::NeedsClarification { clarification })
            }
        }
    }

    async fn ensure_project_loaded(&mut self, project_id: &str) -> Result<()> {
        if self.projects.contains_key(project_id) {
            return Ok(());
        }
        self.evict_if_needed(project_id)?;
        let snapshot = self.memory.load_workspace(project_id).await?;
        let active_task = snapshot
            .active_checkpoint
            .as_ref()
            .map(NovelTaskState::from_checkpoint)
            .transpose()?;
        self.projects.insert(
            project_id.into(),
            ProjectWorkspace {
                project: snapshot.project,
                history: VecDeque::new(),
                active_task,
                last_accessed_at: crate::types::now_millis(),
            },
        );
        self.emit(
            NovelBrainEventKind::ProjectLoaded,
            Some(project_id),
            None,
            "小说项目工作区已加载",
        );
        Ok(())
    }

    async fn ensure_task_loaded(&mut self, task_id: &str) -> Result<String> {
        if let Some(project_id) = self.find_task_project(task_id) {
            return Ok(project_id);
        }
        let checkpoint = self
            .memory
            .load_checkpoint(task_id)
            .await?
            .ok_or_else(|| NovelBrainError::TaskNotFound(task_id.into()))?;
        let project_id = checkpoint.project_id.clone();
        self.ensure_project_loaded(&project_id).await?;
        let state = NovelTaskState::from_checkpoint(&checkpoint)?;
        let workspace = self
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| NovelBrainError::TaskNotFound(task_id.into()))?;
        workspace.active_task = Some(state);
        Ok(project_id)
    }

    fn find_task_project(&self, task_id: &str) -> Option<String> {
        self.projects.iter().find_map(|(project_id, workspace)| {
            workspace
                .active_task
                .as_ref()
                .is_some_and(|state| state.request.task_id == task_id)
                .then(|| project_id.clone())
        })
    }

    fn task(&self, project_id: &str, task_id: &str) -> Result<&NovelTaskState> {
        self.projects
            .get(project_id)
            .and_then(|workspace| workspace.active_task.as_ref())
            .filter(|state| state.request.task_id == task_id)
            .ok_or_else(|| NovelBrainError::TaskNotFound(task_id.into()))
    }

    fn task_mut(&mut self, project_id: &str, task_id: &str) -> Result<&mut NovelTaskState> {
        self.projects
            .get_mut(project_id)
            .and_then(|workspace| workspace.active_task.as_mut())
            .filter(|state| state.request.task_id == task_id)
            .ok_or_else(|| NovelBrainError::TaskNotFound(task_id.into()))
    }

    fn evict_if_needed(&mut self, incoming_project_id: &str) -> Result<()> {
        if self.projects.len() < self.config.max_hot_projects.max(1) {
            return Ok(());
        }
        let candidate = self
            .projects
            .iter()
            .filter(|(project_id, workspace)| {
                project_id.as_str() != incoming_project_id
                    && workspace
                        .active_task
                        .as_ref()
                        .is_none_or(|task| task.phase.is_terminal())
            })
            .min_by_key(|(_, workspace)| workspace.last_accessed_at)
            .map(|(project_id, _)| project_id.clone())
            .ok_or_else(|| {
                NovelBrainError::InvalidTransition("常驻小说项目缓存已被活动任务占满".into())
            })?;
        self.projects.remove(&candidate);
        self.emit(
            NovelBrainEventKind::ProjectEvicted,
            Some(&candidate),
            None,
            "冷项目已从热上下文逐出，持久化状态仍在 MemoryBrain",
        );
        Ok(())
    }

    async fn persist_state(
        &self,
        state: &NovelTaskState,
        actor: NovelLifecycleActor,
        summary: &str,
        details: serde_json::Value,
    ) -> Result<()> {
        let event = NovelTaskEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            task_id: state.request.task_id.clone(),
            project_id: state.request.project_id.clone(),
            actor,
            phase: state.phase,
            summary: summary.into(),
            details,
            created_at: crate::types::now_millis(),
        };
        self.memory.append_task_event(event).await?;
        self.memory.save_checkpoint(state.checkpoint()?).await?;
        Ok(())
    }

    async fn shutdown(&mut self) {
        self.status = ResidentBrainStatus::ShuttingDown;
        for workspace in self.projects.values() {
            if let Some(state) = &workspace.active_task {
                if let Ok(checkpoint) = state.checkpoint() {
                    let _ = self.memory.save_checkpoint(checkpoint).await;
                }
            }
        }
        self.emit(
            NovelBrainEventKind::Deactivated,
            None,
            None,
            "常驻小说脑已保存检查点并停止",
        );
        self.status = ResidentBrainStatus::Stopped;
    }

    fn emit(
        &self,
        kind: NovelBrainEventKind,
        project_id: Option<&str>,
        task_id: Option<&str>,
        summary: &str,
    ) {
        let _ = self.events.send(NovelBrainEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            kind,
            project_id: project_id.map(str::to_string),
            task_id: task_id.map(str::to_string),
            summary: summary.into(),
            created_at: crate::types::now_millis(),
        });
    }
}

fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use brain_llm::{ChatRequest, ChatResponse, ContentBlock, TokenUsage};
    use brain_memory::novel::{
        CommitReport, ConsistencyReport, NovelArtifactReceipt, NovelProjectProgress,
        NovelPublicationStatus, NovelRecallPack, NovelTaskCheckpoint, NovelTaskPhase,
        NovelTaskType,
    };

    use super::*;
    use crate::{
        ContextDocument, MainReviewChecks, MainReviewVerdict, NovelPortError,
        NovelWorkspaceSnapshot, PublicationPolicy, ReviewCheckStatus,
    };

    #[derive(Default)]
    struct QueueLlm {
        responses: Mutex<VecDeque<String>>,
        request_message_counts: Mutex<Vec<usize>>,
    }

    impl QueueLlm {
        fn with_responses(responses: impl IntoIterator<Item = String>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                request_message_counts: Mutex::new(Vec::new()),
            }
        }
    }

    impl LlmProvider for QueueLlm {
        fn model(&self) -> &str {
            "novel-test"
        }

        fn complete(
            &self,
            request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            Box::pin(async move {
                self.request_message_counts
                    .lock()
                    .unwrap()
                    .push(request.messages.len());
                let response = self
                    .responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("queued Novel response");
                Ok(ChatResponse {
                    content: vec![ContentBlock::text(response)],
                    model: "novel-test".into(),
                    usage: TokenUsage::default(),
                    finish_reason: None,
                })
            })
        }
    }

    #[derive(Default)]
    struct FakeMemory {
        projects: Mutex<HashMap<String, NovelProject>>,
        checkpoints: Mutex<HashMap<String, NovelTaskCheckpoint>>,
        events: Mutex<Vec<NovelTaskEvent>>,
        publications: Mutex<HashMap<String, NovelPublicationRecord>>,
        complete_failures: AtomicUsize,
    }

    impl FakeMemory {
        fn with_projects(projects: impl IntoIterator<Item = NovelProject>) -> Self {
            Self {
                projects: Mutex::new(
                    projects
                        .into_iter()
                        .map(|project| (project.project_id.clone(), project))
                        .collect(),
                ),
                ..Self::default()
            }
        }

        fn fail_next_complete(&self) {
            self.complete_failures.store(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl NovelMemoryPort for FakeMemory {
        async fn load_workspace(
            &self,
            project_id: &str,
        ) -> std::result::Result<NovelWorkspaceSnapshot, NovelPortError> {
            let project = self
                .projects
                .lock()
                .unwrap()
                .get(project_id)
                .cloned()
                .ok_or_else(|| NovelPortError::Memory("project not found".into()))?;
            let active_checkpoint = self
                .checkpoints
                .lock()
                .unwrap()
                .values()
                .find(|checkpoint| {
                    checkpoint.project_id == project_id && !checkpoint.phase.is_terminal()
                })
                .cloned();
            Ok(NovelWorkspaceSnapshot {
                project,
                active_checkpoint,
            })
        }

        async fn active_checkpoints(
            &self,
        ) -> std::result::Result<Vec<NovelTaskCheckpoint>, NovelPortError> {
            Ok(self
                .checkpoints
                .lock()
                .unwrap()
                .values()
                .filter(|checkpoint| !checkpoint.phase.is_terminal())
                .cloned()
                .collect())
        }

        async fn load_checkpoint(
            &self,
            task_id: &str,
        ) -> std::result::Result<Option<NovelTaskCheckpoint>, NovelPortError> {
            Ok(self.checkpoints.lock().unwrap().get(task_id).cloned())
        }

        async fn recall_project(
            &self,
            project_id: &str,
            task_type: NovelTaskType,
        ) -> std::result::Result<NovelRecallPack, NovelPortError> {
            let project = self
                .projects
                .lock()
                .unwrap()
                .get(project_id)
                .cloned()
                .ok_or_else(|| NovelPortError::Memory("project not found".into()))?;
            Ok(NovelRecallPack {
                project_id: project.project_id,
                project_title: project.title,
                revision: project.canon_revision,
                task_type,
                branch_id: project.active_branch,
                current_chapter: project.current_chapter,
                facts: project.facts,
                rendered_context: "测试 Canon".into(),
            })
        }

        async fn check_consistency(
            &self,
            project_id: &str,
        ) -> std::result::Result<ConsistencyReport, NovelPortError> {
            let revision = self
                .projects
                .lock()
                .unwrap()
                .get(project_id)
                .map_or(0, |project| project.canon_revision);
            Ok(ConsistencyReport {
                project_id: project_id.into(),
                revision,
                checked_chapter: None,
                issues: Vec::new(),
            })
        }

        async fn append_task_event(
            &self,
            event: NovelTaskEvent,
        ) -> std::result::Result<(), NovelPortError> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }

        async fn save_checkpoint(
            &self,
            checkpoint: NovelTaskCheckpoint,
        ) -> std::result::Result<(), NovelPortError> {
            self.checkpoints
                .lock()
                .unwrap()
                .insert(checkpoint.task_id.clone(), checkpoint);
            Ok(())
        }

        async fn begin_publication(
            &self,
            record: NovelPublicationRecord,
        ) -> std::result::Result<(), NovelPortError> {
            let mut publications = self.publications.lock().unwrap();
            if publications.values().any(|candidate| {
                candidate.project_id == record.project_id
                    && matches!(
                        candidate.status,
                        NovelPublicationStatus::Pending | NovelPublicationStatus::ArtifactSaved
                    )
            }) {
                return Err(NovelPortError::Memory("project publication busy".into()));
            }
            publications.insert(record.publication_id.clone(), record);
            Ok(())
        }

        async fn load_publication(
            &self,
            publication_id: &str,
        ) -> std::result::Result<NovelPublicationRecord, NovelPortError> {
            self.publications
                .lock()
                .unwrap()
                .get(publication_id)
                .cloned()
                .ok_or_else(|| NovelPortError::Memory("publication not found".into()))
        }

        async fn complete_publication(
            &self,
            publication_id: &str,
            artifact: NovelArtifactReceipt,
        ) -> std::result::Result<CommitReport, NovelPortError> {
            if self
                .complete_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(NovelPortError::Memory("injected completion failure".into()));
            }
            let mut publications = self.publications.lock().unwrap();
            let publication = publications
                .get_mut(publication_id)
                .ok_or_else(|| NovelPortError::Memory("publication not found".into()))?;
            if let Some(report) = &publication.commit_report {
                return Ok(report.clone());
            }
            if publication.output_path != artifact.canonical_path
                || publication.content_sha256 != artifact.sha256
            {
                return Err(NovelPortError::Memory("artifact mismatch".into()));
            }
            let mut projects = self.projects.lock().unwrap();
            let project = projects
                .get_mut(&publication.project_id)
                .ok_or_else(|| NovelPortError::Memory("project not found".into()))?;
            let previous_revision = project.canon_revision;
            project.canon_revision += 1;
            project
                .applied_publications
                .push(publication_id.to_string());
            let report = CommitReport {
                project_id: project.project_id.clone(),
                previous_revision,
                new_revision: project.canon_revision,
                accepted_fact_ids: Vec::new(),
                conflicts: Vec::new(),
                graph_mirrored: true,
            };
            publication.artifact = Some(artifact);
            publication.status = NovelPublicationStatus::Completed;
            publication.commit_report = Some(report.clone());
            Ok(report)
        }

        async fn abort_publication(
            &self,
            publication_id: &str,
            reason: &str,
        ) -> std::result::Result<(), NovelPortError> {
            let mut publications = self.publications.lock().unwrap();
            let publication = publications
                .get_mut(publication_id)
                .ok_or_else(|| NovelPortError::Memory("publication not found".into()))?;
            publication.status = NovelPublicationStatus::Aborted;
            publication.error = Some(reason.into());
            Ok(())
        }

        async fn pending_publications(
            &self,
        ) -> std::result::Result<Vec<NovelPublicationRecord>, NovelPortError> {
            Ok(self
                .publications
                .lock()
                .unwrap()
                .values()
                .filter(|publication| {
                    matches!(
                        publication.status,
                        NovelPublicationStatus::Pending | NovelPublicationStatus::ArtifactSaved
                    )
                })
                .cloned()
                .collect())
        }
    }

    #[derive(Default)]
    struct FakeResources {
        writes: Mutex<Vec<(PathBuf, String)>>,
    }

    #[async_trait]
    impl NovelResourcePort for FakeResources {
        async fn resolve_artifact_path(
            &self,
            path: &std::path::Path,
        ) -> std::result::Result<PathBuf, NovelPortError> {
            Ok(if path.is_absolute() {
                path.to_path_buf()
            } else {
                PathBuf::from("/workspace").join(path)
            })
        }

        async fn read_context(
            &self,
            _reference: &crate::ContextRef,
        ) -> std::result::Result<ContextDocument, NovelPortError> {
            Err(NovelPortError::Resource(
                "test has no context documents".into(),
            ))
        }

        async fn write_artifact_atomic(
            &self,
            path: &std::path::Path,
            exact_content: &str,
        ) -> std::result::Result<NovelArtifactReceipt, NovelPortError> {
            self.writes
                .lock()
                .unwrap()
                .push((path.to_path_buf(), exact_content.into()));
            Ok(NovelArtifactReceipt {
                canonical_path: path.to_string_lossy().into_owned(),
                sha256: sha256(exact_content.as_bytes()),
                bytes: exact_content.len() as u64,
                written_at: crate::types::now_millis(),
            })
        }

        async fn verify_artifact(
            &self,
            path: &std::path::Path,
            expected_sha256: &str,
        ) -> std::result::Result<Option<NovelArtifactReceipt>, NovelPortError> {
            let writes = self.writes.lock().unwrap();
            let Some((written_path, content)) =
                writes.iter().find(|(written_path, _)| written_path == path)
            else {
                return Ok(None);
            };
            let actual = sha256(content.as_bytes());
            if actual != expected_sha256 {
                return Err(NovelPortError::ContextChanged("artifact mismatch".into()));
            }
            Ok(Some(NovelArtifactReceipt {
                canonical_path: written_path.to_string_lossy().into_owned(),
                sha256: actual,
                bytes: content.len() as u64,
                written_at: crate::types::now_millis(),
            }))
        }
    }

    fn task_request(task_id: &str, project_id: &str) -> NovelTaskRequest {
        NovelTaskRequest {
            task_id: task_id.into(),
            project_id: project_id.into(),
            task_type: NovelTaskType::Body,
            task_brief: "写第一章".into(),
            target_chapter: Some(1),
            expected_revision: 0,
            output_path: PathBuf::from(format!("chapters/{task_id}.md")),
            context_refs: Vec::new(),
            must_happen: vec!["主角进入旧港".into()],
            must_not_change: vec!["主角姓名".into()],
            acceptance_criteria: vec!["完成正文".into()],
            allow_web_research: false,
            publication_policy: PublicationPolicy::RequireUserAcceptance,
            parent_task_id: None,
            source_conversation_id: None,
            source_generation_id: None,
        }
    }

    fn pass_review(task_id: &str, version: u32) -> MainReviewRecord {
        MainReviewRecord {
            task_id: task_id.into(),
            draft_version: version,
            reviewed_canon_revision: 0,
            verdict: MainReviewVerdict::Pass,
            checks: MainReviewChecks {
                user_requirements: ReviewCheckStatus::Pass,
                outline_alignment: ReviewCheckStatus::Pass,
                canon_consistency: ReviewCheckStatus::Pass,
                character_consistency: ReviewCheckStatus::Pass,
                timeline_consistency: ReviewCheckStatus::Pass,
                plot_and_foreshadowing: ReviewCheckStatus::Pass,
                style_quality: ReviewCheckStatus::Pass,
                pacing_and_hook: ReviewCheckStatus::Pass,
            },
            issues: Vec::new(),
            evidence_refs: vec!["user".into(), "outline".into(), "canon".into()],
            summary: "主脑复审通过".into(),
        }
    }

    fn model_draft(project_id: &str, task_id: &str, content: &str) -> String {
        serde_json::json!({
            "outcome": "draft_ready",
            "content": content,
            "self_review": {
                "verdict": "pass",
                "checks": {
                    "outline_alignment": "pass",
                    "canon_consistency": "pass",
                    "character_consistency": "pass",
                    "timeline_consistency": "pass",
                    "plot_and_foreshadowing": "pass",
                    "style_and_repetition": "pass"
                },
                "issues": [],
                "unverified_assumptions": [],
                "summary": "六项自检通过"
            },
            "proposed_delta": {
                "project_id": project_id,
                "branch_id": "main",
                "expected_revision": 0,
                "task_type": "body",
                "source_ref": format!("chapters/{task_id}.md"),
                "progress": NovelProjectProgress {
                    current_volume: Some("第一卷".into()),
                    current_chapter: Some(1),
                },
                "proposed_facts": [],
                "state_changes": [],
                "plot_updates": [],
                "foreshadowing_updates": [],
                "feedback": [],
                "experience_candidates": []
            },
            "evidence_refs": ["user", "outline", "canon"]
        })
        .to_string()
    }

    #[tokio::test]
    async fn resident_actor_keeps_history_across_revision_and_publishes_exact_draft() {
        let llm = Arc::new(QueueLlm::with_responses([
            model_draft("project-1", "task-1", "第一版正文"),
            model_draft("project-1", "task-1", "用户修订后的第二版正文"),
        ]));
        let memory = Arc::new(FakeMemory::with_projects([NovelProject::new(
            "project-1",
            "暗城",
        )]));
        let resources = Arc::new(FakeResources::default());
        let (handle, task) = spawn_novel_brain(
            llm.clone(),
            memory.clone(),
            resources.clone(),
            NovelBrainConfig::default(),
        );

        let first = handle
            .start_task(task_request("task-1", "project-1"))
            .await
            .unwrap();
        assert!(matches!(first, NovelOutcome::DraftReady(_)));
        handle.review_draft(pass_review("task-1", 1)).await.unwrap();
        let revised = handle
            .user_decision(UserDecisionRecord {
                task_id: "task-1".into(),
                draft_version: 1,
                decision: UserDecision::Revise,
                feedback: Some("加强结尾悬念".into()),
                decided_at: 1,
            })
            .await
            .unwrap();
        let NovelTransition::DraftReady { draft } = revised else {
            panic!("expected revised draft");
        };
        assert_eq!(draft.draft_version, 2);
        assert_eq!(draft.content, "用户修订后的第二版正文");

        handle.review_draft(pass_review("task-1", 2)).await.unwrap();
        handle
            .user_decision(UserDecisionRecord {
                task_id: "task-1".into(),
                draft_version: 2,
                decision: UserDecision::Accept,
                feedback: None,
                decided_at: 2,
            })
            .await
            .unwrap();
        let receipt = handle.publish("task-1", 2).await.unwrap();
        assert_eq!(receipt.draft_version, 2);
        assert_eq!(resources.writes.lock().unwrap().len(), 1);
        assert_eq!(
            resources.writes.lock().unwrap()[0].1,
            "用户修订后的第二版正文"
        );
        let status = handle.status(Some("project-1".into())).await.unwrap();
        assert_eq!(status.projects[0].history_messages, 4);
        assert_eq!(status.projects[0].phase, Some(NovelTaskPhase::Completed));
        assert_eq!(*llm.request_message_counts.lock().unwrap(), vec![2, 4]);

        handle.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn conversation_fork_cancels_only_matching_unpublished_task() {
        let llm = Arc::new(QueueLlm::with_responses([model_draft(
            "project-1",
            "task-branch",
            "旧分支候选正文",
        )]));
        let memory = Arc::new(FakeMemory::with_projects([NovelProject::new(
            "project-1",
            "暗城",
        )]));
        let resources = Arc::new(FakeResources::default());
        let (handle, task) =
            spawn_novel_brain(llm, memory.clone(), resources, NovelBrainConfig::default());
        let mut request = task_request("task-branch", "project-1");
        request.source_conversation_id = Some("chat_1".into());
        request.source_generation_id = Some("generation_start".into());
        handle.start_task(request).await.unwrap();
        handle
            .associate_conversation_source(
                "task-branch",
                NovelConversationSource {
                    conversation_id: "chat_1".into(),
                    generation_id: "generation_old".into(),
                },
            )
            .await
            .unwrap();

        let cancelled = handle
            .invalidate_conversation_generations("chat_1", vec!["generation_old".into()], false)
            .await
            .unwrap();

        assert_eq!(cancelled, vec!["task-branch"]);
        let status = handle.status(Some("project-1".into())).await.unwrap();
        assert_eq!(status.projects[0].task_id, None);
        assert_eq!(status.projects[0].phase, None);
        assert_eq!(
            memory
                .checkpoints
                .lock()
                .unwrap()
                .get("task-branch")
                .unwrap()
                .phase,
            NovelTaskPhase::Cancelled
        );

        handle.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn publish_is_rejected_without_user_acceptance() {
        let llm = Arc::new(QueueLlm::with_responses([model_draft(
            "project-1",
            "task-1",
            "候选正文",
        )]));
        let memory = Arc::new(FakeMemory::with_projects([NovelProject::new(
            "project-1",
            "暗城",
        )]));
        let resources = Arc::new(FakeResources::default());
        let (handle, task) =
            spawn_novel_brain(llm, memory, resources.clone(), NovelBrainConfig::default());

        handle
            .start_task(task_request("task-1", "project-1"))
            .await
            .unwrap();
        handle.review_draft(pass_review("task-1", 1)).await.unwrap();
        let error = handle.publish("task-1", 1).await.unwrap_err();
        assert!(matches!(error, NovelBrainError::InvalidTransition(_)));
        assert!(resources.writes.lock().unwrap().is_empty());

        handle.shutdown().await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn restart_recovers_artifact_saved_before_memory_completion() {
        let memory = Arc::new(FakeMemory::with_projects([NovelProject::new(
            "project-1",
            "暗城",
        )]));
        memory.fail_next_complete();
        let resources = Arc::new(FakeResources::default());
        let (first_handle, first_task) = spawn_novel_brain(
            Arc::new(QueueLlm::with_responses([model_draft(
                "project-1",
                "task-1",
                "待恢复正文",
            )])),
            memory.clone(),
            resources.clone(),
            NovelBrainConfig::default(),
        );
        first_handle
            .start_task(task_request("task-1", "project-1"))
            .await
            .unwrap();
        first_handle
            .review_draft(pass_review("task-1", 1))
            .await
            .unwrap();
        first_handle
            .user_decision(UserDecisionRecord {
                task_id: "task-1".into(),
                draft_version: 1,
                decision: UserDecision::Accept,
                feedback: None,
                decided_at: 1,
            })
            .await
            .unwrap();
        let error = first_handle.publish("task-1", 1).await.unwrap_err();
        assert!(error.to_string().contains("injected completion failure"));
        assert_eq!(resources.writes.lock().unwrap().len(), 1);
        assert_eq!(
            first_handle.status(None).await.unwrap().projects[0].phase,
            Some(NovelTaskPhase::ArtifactSavedMemoryPending)
        );
        first_handle.shutdown().await.unwrap();
        first_task.await.unwrap();

        let (second_handle, second_task) = spawn_novel_brain(
            Arc::new(QueueLlm::default()),
            memory.clone(),
            resources.clone(),
            NovelBrainConfig::default(),
        );
        let status = second_handle.status(None).await.unwrap();
        assert_eq!(status.projects[0].phase, Some(NovelTaskPhase::Completed));
        assert!(status.pending_publications.is_empty());
        assert_eq!(resources.writes.lock().unwrap().len(), 1);
        assert_eq!(
            memory
                .projects
                .lock()
                .unwrap()
                .get("project-1")
                .unwrap()
                .canon_revision,
            1
        );
        second_handle.shutdown().await.unwrap();
        second_task.await.unwrap();
    }

    #[tokio::test]
    async fn restart_recovers_publication_started_before_pending_checkpoint() {
        let memory = Arc::new(FakeMemory::with_projects([NovelProject::new(
            "project-1",
            "暗城",
        )]));
        let resources = Arc::new(FakeResources::default());
        let (first_handle, first_task) = spawn_novel_brain(
            Arc::new(QueueLlm::with_responses([model_draft(
                "project-1",
                "task-1",
                "journal 已写但 checkpoint 未写的正文",
            )])),
            memory.clone(),
            resources.clone(),
            NovelBrainConfig::default(),
        );
        let outcome = first_handle
            .start_task(task_request("task-1", "project-1"))
            .await
            .unwrap();
        let NovelOutcome::DraftReady(draft) = outcome else {
            panic!("expected draft");
        };
        first_handle
            .review_draft(pass_review("task-1", 1))
            .await
            .unwrap();
        first_handle
            .user_decision(UserDecisionRecord {
                task_id: "task-1".into(),
                draft_version: 1,
                decision: UserDecision::Accept,
                feedback: None,
                decided_at: 1,
            })
            .await
            .unwrap();

        let publication_id = "publication-pre-checkpoint";
        memory
            .begin_publication(NovelPublicationRecord::pending(
                publication_id,
                "task-1",
                1,
                "/workspace/chapters/task-1.md",
                sha256(draft.content.as_bytes()),
                draft.proposed_delta,
            ))
            .await
            .unwrap();
        first_handle.shutdown().await.unwrap();
        first_task.await.unwrap();

        let (second_handle, second_task) = spawn_novel_brain(
            Arc::new(QueueLlm::default()),
            memory.clone(),
            resources.clone(),
            NovelBrainConfig::default(),
        );
        let status = second_handle.status(None).await.unwrap();
        assert_eq!(status.projects[0].phase, Some(NovelTaskPhase::Completed));
        assert!(status.pending_publications.is_empty());
        assert_eq!(resources.writes.lock().unwrap().len(), 1);
        assert_eq!(
            memory
                .load_publication(publication_id)
                .await
                .unwrap()
                .status,
            NovelPublicationStatus::Completed
        );
        assert_eq!(
            memory
                .projects
                .lock()
                .unwrap()
                .get("project-1")
                .unwrap()
                .canon_revision,
            1
        );
        second_handle.shutdown().await.unwrap();
        second_task.await.unwrap();
    }

    #[tokio::test]
    async fn restart_rejects_pre_checkpoint_publication_with_changed_content() {
        let memory = Arc::new(FakeMemory::with_projects([NovelProject::new(
            "project-1",
            "暗城",
        )]));
        let resources = Arc::new(FakeResources::default());
        let (first_handle, first_task) = spawn_novel_brain(
            Arc::new(QueueLlm::with_responses([model_draft(
                "project-1",
                "task-1",
                "不应发布的正文",
            )])),
            memory.clone(),
            resources.clone(),
            NovelBrainConfig::default(),
        );
        let outcome = first_handle
            .start_task(task_request("task-1", "project-1"))
            .await
            .unwrap();
        let NovelOutcome::DraftReady(draft) = outcome else {
            panic!("expected draft");
        };
        first_handle
            .review_draft(pass_review("task-1", 1))
            .await
            .unwrap();
        first_handle
            .user_decision(UserDecisionRecord {
                task_id: "task-1".into(),
                draft_version: 1,
                decision: UserDecision::Accept,
                feedback: None,
                decided_at: 1,
            })
            .await
            .unwrap();
        memory
            .begin_publication(NovelPublicationRecord::pending(
                "publication-corrupt",
                "task-1",
                1,
                "/workspace/chapters/task-1.md",
                sha256(b"different content"),
                draft.proposed_delta,
            ))
            .await
            .unwrap();
        first_handle.shutdown().await.unwrap();
        first_task.await.unwrap();

        let (second_handle, second_task) = spawn_novel_brain(
            Arc::new(QueueLlm::default()),
            memory.clone(),
            resources.clone(),
            NovelBrainConfig::default(),
        );
        let status = second_handle.status(None).await.unwrap();
        assert_eq!(status.status, ResidentBrainStatus::Degraded);
        assert!(status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("与审核草稿内容不一致")));
        assert_eq!(status.pending_publications.len(), 1);
        assert!(resources.writes.lock().unwrap().is_empty());
        assert_eq!(
            memory
                .projects
                .lock()
                .unwrap()
                .get("project-1")
                .unwrap()
                .canon_revision,
            0
        );
        second_handle.shutdown().await.unwrap();
        second_task.await.unwrap();
    }

    #[tokio::test]
    async fn project_histories_are_isolated_in_one_resident_actor() {
        let llm = Arc::new(QueueLlm::with_responses([
            model_draft("project-1", "task-1", "甲项目正文"),
            model_draft("project-2", "task-2", "乙项目正文"),
        ]));
        let memory = Arc::new(FakeMemory::with_projects([
            NovelProject::new("project-1", "甲"),
            NovelProject::new("project-2", "乙"),
        ]));
        let resources = Arc::new(FakeResources::default());
        let (handle, task) = spawn_novel_brain(llm, memory, resources, NovelBrainConfig::default());

        handle
            .start_task(task_request("task-1", "project-1"))
            .await
            .unwrap();
        handle
            .start_task(task_request("task-2", "project-2"))
            .await
            .unwrap();
        let status = handle.status(None).await.unwrap();
        assert_eq!(status.projects.len(), 2);
        assert!(status
            .projects
            .iter()
            .all(|project| project.history_messages == 2));
        assert_ne!(status.projects[0].task_id, status.projects[1].task_id);

        handle.shutdown().await.unwrap();
        task.await.unwrap();
    }
}
