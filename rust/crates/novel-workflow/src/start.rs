use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agent_runtime::AgentProfileSnapshot;
use async_trait::async_trait;
use knowledge_core::{ContextBlock, ContextBlockInput, ContextBlockKind, ContextSnapshot};
use novel_domain::{
    check_consistency, sha256_hex, ContextRef, NovelCandidate, NovelLifecycleActor, NovelOutcome,
    NovelProject, NovelTaskCheckpoint, NovelTaskEvent, NovelTaskPhase, NovelTaskRequest,
    NovelTaskState,
};
use serde::{Deserialize, Serialize};
use task_engine::{
    ActualUsage, NodeState, TaskArtifact, TaskCoordinator, TaskEngineError, TaskRepository,
    TaskRun, TaskRunState,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::{
    writer_profile, NovelWorkflowBudget, NovelWorkflowDefinition, NovelWorkflowError,
    NovelWorkflowModels, ProfileModel, WORKFLOW_CONFIG_VERSION,
};

const START_CONTRACT_KEY: &str = "novel_start_contract";
const START_CONTRACT_VERSION: u32 = 1;
const WRITER_ARTIFACT_VERSION: u32 = 1;
const WRITER_ARTIFACT_MEDIA_TYPE: &str = "application/vnd.claw.novel-writer+json;version=1";

#[derive(Debug, thiserror::Error)]
pub enum NovelWorkflowPortError {
    #[error("Novel workflow storage failed: {0}")]
    Storage(String),
    #[error("invalid Novel workflow request: {0}")]
    InvalidRequest(String),
    #[error("Novel project {0} already has active work")]
    ProjectBusy(String),
    #[error("Novel Canon revision is stale: expected={expected}, actual={actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("invalid Novel writer output: {0}")]
    InvalidWriterOutput(String),
    #[error("Novel Writer execution failed: {message}")]
    WriterExecutionFailed { message: String, usage: ActualUsage },
    #[error("Novel workflow context changed: {0}")]
    ContextChanged(String),
    #[error(
        "Novel workflow context hash changed: path={path}, expected={expected}, actual={actual}"
    )]
    ContextHashChanged {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("Novel TaskEngine operation failed: {0}")]
    TaskEngine(String),
    #[error("Novel workflow serialization failed: {0}")]
    Serialization(String),
}

impl NovelWorkflowPortError {
    #[must_use]
    pub const fn actual_usage(&self) -> Option<ActualUsage> {
        match self {
            Self::WriterExecutionFailed { usage, .. } => Some(*usage),
            _ => None,
        }
    }
}

impl From<NovelWorkflowError> for NovelWorkflowPortError {
    fn from(error: NovelWorkflowError) -> Self {
        Self::InvalidRequest(error.to_string())
    }
}

impl From<novel_domain::NovelDomainError> for NovelWorkflowPortError {
    fn from(error: novel_domain::NovelDomainError) -> Self {
        match error {
            novel_domain::NovelDomainError::InvalidRequest(message) => {
                Self::InvalidRequest(message)
            }
            novel_domain::NovelDomainError::InvalidTransition(message)
            | novel_domain::NovelDomainError::InvalidCandidate(message) => {
                Self::InvalidWriterOutput(message)
            }
            novel_domain::NovelDomainError::ProjectBusy(project_id) => {
                Self::ProjectBusy(project_id)
            }
            novel_domain::NovelDomainError::StaleRevision { expected, actual } => {
                Self::StaleRevision { expected, actual }
            }
            novel_domain::NovelDomainError::Serialization(error) => {
                Self::Serialization(error.to_string())
            }
        }
    }
}

impl From<TaskEngineError> for NovelWorkflowPortError {
    fn from(error: TaskEngineError) -> Self {
        Self::TaskEngine(error.to_string())
    }
}

impl From<serde_json::Error> for NovelWorkflowPortError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

impl From<knowledge_core::KnowledgeError> for NovelWorkflowPortError {
    fn from(error: knowledge_core::KnowledgeError) -> Self {
        Self::ContextChanged(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelContextDocument {
    pub reference: ContextRef,
    pub content: String,
}

#[async_trait]
pub trait NovelWorkflowEnvironmentPort: Send + Sync {
    async fn load_project(&self, project_id: &str) -> Result<NovelProject, NovelWorkflowPortError>;

    async fn load_checkpoint(
        &self,
        task_id: &str,
    ) -> Result<Option<NovelTaskCheckpoint>, NovelWorkflowPortError>;

    async fn active_checkpoint_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<NovelTaskCheckpoint>, NovelWorkflowPortError>;

    async fn read_context(
        &self,
        reference: &ContextRef,
    ) -> Result<NovelContextDocument, NovelWorkflowPortError>;

    async fn save_checkpoint(
        &self,
        checkpoint: NovelTaskCheckpoint,
    ) -> Result<(), NovelWorkflowPortError>;

    async fn append_task_event(&self, event: NovelTaskEvent) -> Result<(), NovelWorkflowPortError>;
}

#[derive(Debug, Clone)]
pub struct NovelWriterInvocation {
    pub request: NovelTaskRequest,
    pub project: NovelProject,
    pub context_snapshot: ContextSnapshot,
    pub context_documents: Vec<NovelContextDocument>,
    pub next_draft_version: u32,
    pub profile: AgentProfileSnapshot,
    pub model: ProfileModel,
    pub budget: NovelWorkflowBudget,
}

#[derive(Debug, Clone)]
pub struct NovelWriterExecution {
    pub outcome: NovelOutcome,
    pub raw_output: String,
    pub usage: ActualUsage,
}

#[async_trait]
pub trait NovelWriterPort: Send + Sync {
    async fn execute(
        &self,
        invocation: NovelWriterInvocation,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelStartWorkflowResult {
    pub task_run_id: String,
    pub artifact_id: String,
    pub outcome: NovelOutcome,
    pub candidate: Option<NovelCandidate>,
    pub checkpoint: NovelTaskCheckpoint,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FrozenStartContract {
    version: u32,
    #[serde(default)]
    execution_id: String,
    #[serde(default)]
    revision_instruction: Option<String>,
    request: NovelTaskRequest,
    project: NovelProject,
    context_documents: Vec<NovelContextDocument>,
    initial_checkpoint: NovelTaskCheckpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WriterArtifactEnvelope {
    version: u32,
    outcome: NovelOutcome,
    raw_output: String,
    usage: ActualUsage,
}

pub struct NovelStartWorkflow {
    repository: Arc<TaskRepository>,
    coordinator: TaskCoordinator,
    environment: Arc<dyn NovelWorkflowEnvironmentPort>,
    writer: Arc<dyn NovelWriterPort>,
    models: NovelWorkflowModels,
    budget: NovelWorkflowBudget,
    task_locks: Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
}

impl NovelStartWorkflow {
    #[must_use]
    pub fn new(
        repository: Arc<TaskRepository>,
        coordinator: TaskCoordinator,
        environment: Arc<dyn NovelWorkflowEnvironmentPort>,
        writer: Arc<dyn NovelWriterPort>,
        models: NovelWorkflowModels,
        budget: NovelWorkflowBudget,
    ) -> Self {
        Self {
            repository,
            coordinator,
            environment,
            writer,
            models,
            budget,
            task_locks: Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn start_task(
        &self,
        request: NovelTaskRequest,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        let task_lock = self.task_lock(&request.task_id);
        let _guard = task_lock.lock().await;
        let task_run_id = task_run_id(&request.task_id);
        if let Some(task) = load_task_optional(Arc::clone(&self.repository), &task_run_id).await? {
            return self.resume_existing(task, request).await;
        }
        self.start_new(request).await
    }

    pub async fn continue_task(
        &self,
        task_id: &str,
        revision_instruction: &str,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        if task_id.trim().is_empty() || revision_instruction.trim().is_empty() {
            return Err(NovelWorkflowPortError::InvalidRequest(
                "task id and revision instruction are required".into(),
            ));
        }
        let task_lock = self.task_lock(task_id);
        let _guard = task_lock.lock().await;
        let checkpoint = self
            .environment
            .load_checkpoint(task_id)
            .await?
            .ok_or_else(|| {
                NovelWorkflowPortError::InvalidRequest(format!(
                    "Novel task does not exist: {task_id}"
                ))
            })?;
        let mut state = NovelTaskState::from_checkpoint(&checkpoint)?;
        state.begin_drafting()?;
        let execution_id = format!("{task_id}-draft-{}", state.draft_version + 1);
        let base_task_run_id = task_run_id(&execution_id);
        if let Some(task) =
            load_task_optional(Arc::clone(&self.repository), &base_task_run_id).await?
        {
            if task.state == TaskRunState::Failed {
                let mut attempt = 1_u32;
                loop {
                    let retry_execution_id = format!("{execution_id}-retry-{attempt}");
                    let retry_task_run_id = task_run_id(&retry_execution_id);
                    match load_task_optional(Arc::clone(&self.repository), &retry_task_run_id)
                        .await?
                    {
                        Some(retry_task) if retry_task.state == TaskRunState::Failed => {}
                        Some(retry_task) => {
                            let retry_frozen = frozen_contract(&retry_task)?;
                            let frozen_instruction = retry_frozen.revision_instruction.clone();
                            return self
                                .resume_existing_execution(
                                    retry_task,
                                    &state.request,
                                    &retry_execution_id,
                                    frozen_instruction.as_deref(),
                                )
                                .await;
                        }
                        None => {
                            return self
                                .start_iteration(state, retry_execution_id, revision_instruction)
                                .await;
                        }
                    }
                    attempt = attempt.checked_add(1).ok_or_else(|| {
                        NovelWorkflowPortError::TaskEngine(format!(
                            "task {task_id} exhausted writer retry identities"
                        ))
                    })?;
                }
            }
            let frozen = frozen_contract(&task)?;
            let frozen_instruction = frozen.revision_instruction.clone();
            return self
                .resume_existing_execution(
                    task,
                    &state.request,
                    &execution_id,
                    frozen_instruction.as_deref(),
                )
                .await;
        }
        self.start_iteration(state, execution_id, revision_instruction)
            .await
    }

    fn task_lock(&self, task_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .task_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            locks
                .entry(task_id.to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    async fn start_new(
        &self,
        request: NovelTaskRequest,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        let mut state = NovelTaskState::new(request.clone())?;
        let project = self.environment.load_project(&request.project_id).await?;
        project.validate()?;
        if project.project_id != request.project_id {
            return Err(NovelWorkflowPortError::InvalidRequest(format!(
                "environment returned project `{}` for `{}`",
                project.project_id, request.project_id
            )));
        }
        if project.canon_revision != request.expected_revision {
            return Err(NovelWorkflowPortError::StaleRevision {
                expected: request.expected_revision,
                actual: project.canon_revision,
            });
        }
        self.ensure_task_and_project_available(&state).await?;

        let context_documents = self.read_context_documents(&request.context_refs).await?;
        state.begin_drafting()?;
        let context_snapshot =
            build_context_snapshot(&request.task_id, &state, &project, &context_documents, None)?;
        let initial_checkpoint = state.checkpoint()?;
        let frozen = FrozenStartContract {
            version: START_CONTRACT_VERSION,
            execution_id: request.task_id.clone(),
            revision_instruction: None,
            request: request.clone(),
            project: project.clone(),
            context_documents,
            initial_checkpoint: initial_checkpoint.clone(),
        };
        let mut task_request = NovelWorkflowDefinition::writer_stage(
            &request.task_id,
            &request.task_brief,
            context_snapshot,
            self.models.clone(),
            self.budget,
        )?
        .into_task_run();
        task_request
            .resolved_config
            .as_object_mut()
            .ok_or_else(|| {
                NovelWorkflowPortError::Serialization(
                    "workflow resolved config is not an object".into(),
                )
            })?
            .insert(START_CONTRACT_KEY.into(), serde_json::to_value(&frozen)?);

        self.persist_initial(&state, &request.task_id).await?;
        let task = create_task(Arc::clone(&self.repository), task_request).await?;
        self.execute_writer(task, frozen, false).await
    }

    async fn start_iteration(
        &self,
        state: NovelTaskState,
        execution_id: String,
        revision_instruction: &str,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        let request = state.request.clone();
        let project = self.environment.load_project(&request.project_id).await?;
        project.validate()?;
        if project.project_id != request.project_id
            || project.canon_revision != request.expected_revision
        {
            return Err(NovelWorkflowPortError::StaleRevision {
                expected: request.expected_revision,
                actual: project.canon_revision,
            });
        }
        let context_documents = self.read_context_documents(&request.context_refs).await?;
        let context_snapshot = build_context_snapshot(
            &execution_id,
            &state,
            &project,
            &context_documents,
            Some(revision_instruction),
        )?;
        let initial_checkpoint = state.checkpoint()?;
        let frozen = FrozenStartContract {
            version: START_CONTRACT_VERSION,
            execution_id: execution_id.clone(),
            revision_instruction: Some(revision_instruction.to_owned()),
            request: request.clone(),
            project,
            context_documents,
            initial_checkpoint,
        };
        let objective = format!(
            "{}\nRevision instruction: {revision_instruction}",
            request.task_brief
        );
        let mut task_request = NovelWorkflowDefinition::writer_stage(
            &execution_id,
            &objective,
            context_snapshot,
            self.models.clone(),
            self.budget,
        )?
        .into_task_run();
        task_request
            .resolved_config
            .as_object_mut()
            .ok_or_else(|| {
                NovelWorkflowPortError::Serialization(
                    "workflow resolved config is not an object".into(),
                )
            })?
            .insert(START_CONTRACT_KEY.into(), serde_json::to_value(&frozen)?);
        self.persist_initial(&state, &execution_id).await?;
        let task = create_task(Arc::clone(&self.repository), task_request).await?;
        self.execute_writer(task, frozen, false).await
    }

    async fn resume_existing(
        &self,
        task: TaskRun,
        request: NovelTaskRequest,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        self.resume_existing_execution(task, &request, &request.task_id, None)
            .await
    }

    async fn resume_existing_execution(
        &self,
        task: TaskRun,
        request: &NovelTaskRequest,
        execution_id: &str,
        revision_instruction: Option<&str>,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        let frozen = frozen_contract(&task)?;
        if frozen.request != *request
            || frozen.execution_id != execution_id
            || frozen.revision_instruction.as_deref() != revision_instruction
        {
            return Err(NovelWorkflowPortError::InvalidRequest(format!(
                "task {} is already bound to a different frozen request",
                request.task_id
            )));
        }
        match task.state {
            TaskRunState::Completed => self.replay_completed(task, frozen).await,
            TaskRunState::Queued | TaskRunState::Running => {
                self.execute_writer(task, frozen, true).await
            }
            state => Err(NovelWorkflowPortError::TaskEngine(format!(
                "task {} cannot resume from {state:?}",
                task.task_run_id
            ))),
        }
    }

    async fn ensure_task_and_project_available(
        &self,
        state: &NovelTaskState,
    ) -> Result<(), NovelWorkflowPortError> {
        if let Some(existing) = self
            .environment
            .load_checkpoint(&state.request.task_id)
            .await?
        {
            let existing_state = NovelTaskState::from_checkpoint(&existing)?;
            let is_recoverable_initial = existing_state.request == state.request
                && existing_state.phase == NovelTaskPhase::Drafting
                && existing_state.draft.is_none();
            if !is_recoverable_initial {
                return Err(NovelWorkflowPortError::InvalidRequest(format!(
                    "task_id already exists: {}",
                    state.request.task_id
                )));
            }
        }
        if let Some(active) = self
            .environment
            .active_checkpoint_for_project(&state.request.project_id)
            .await?
        {
            let active_state = NovelTaskState::from_checkpoint(&active)?;
            if active.task_id != state.request.task_id && !active_state.phase.is_terminal() {
                return Err(NovelWorkflowPortError::ProjectBusy(
                    state.request.project_id.clone(),
                ));
            }
        }
        Ok(())
    }

    async fn read_context_documents(
        &self,
        references: &[ContextRef],
    ) -> Result<Vec<NovelContextDocument>, NovelWorkflowPortError> {
        let mut documents = Vec::with_capacity(references.len());
        for reference in references {
            let document = self.environment.read_context(reference).await?;
            validate_context_document(reference, &document)?;
            documents.push(document);
        }
        Ok(documents)
    }

    async fn execute_writer(
        &self,
        task: TaskRun,
        frozen: FrozenStartContract,
        replayed: bool,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        validate_task_binding(&task, &frozen)?;
        if task.state == TaskRunState::Completed {
            return self.replay_completed(task, frozen).await;
        }
        let nodes = load_nodes(Arc::clone(&self.repository), &task.task_run_id).await?;
        if nodes.len() != 1 {
            return Err(NovelWorkflowPortError::TaskEngine(format!(
                "writer task {} has {} nodes",
                task.task_run_id,
                nodes.len()
            )));
        }
        let node = &nodes[0];
        if node.state != NodeState::Ready {
            return Err(NovelWorkflowPortError::TaskEngine(format!(
                "writer node {} is {:?}; recovery must make it ready before execution",
                node.node_id, node.state
            )));
        }
        let instance_run_id = instance_run_id(&frozen.execution_id);
        let coordinated = self
            .coordinator
            .admit_node(&node.node_id, &instance_run_id, CancellationToken::new())
            .await?;
        let (started, _admission) = coordinated.into_parts();
        let context_snapshot = context_snapshot(&task)?;
        let mut state = NovelTaskState::from_checkpoint(&frozen.initial_checkpoint)?;
        let invocation = NovelWriterInvocation {
            request: frozen.request.clone(),
            project: frozen.project.clone(),
            context_snapshot,
            context_documents: frozen.context_documents.clone(),
            next_draft_version: state.draft_version + 1,
            profile: writer_profile()?,
            model: self.models.writer.clone(),
            budget: self.budget,
        };
        let execution = match self.writer.execute(invocation).await {
            Ok(execution) => execution,
            Err(error) => {
                let usage = error.actual_usage();
                self.fail_after_execution(
                    &started.instance.instance_run_id,
                    started.instance.version,
                    &error.to_string(),
                    usage,
                )
                .await?;
                return Err(error);
            }
        };
        let envelope = self
            .validate_writer_execution(
                &mut state,
                &frozen.project.active_branch,
                &started,
                execution,
            )
            .await?;
        let artifact = self
            .persist_writer_execution(&mut state, &started, &envelope)
            .await?;
        let checkpoint = self
            .persist_completed(
                &state,
                &artifact,
                &frozen.execution_id,
                frozen.initial_checkpoint.updated_at,
            )
            .await?;
        Ok(NovelStartWorkflowResult {
            task_run_id: task.task_run_id,
            artifact_id: artifact.artifact_id,
            outcome: envelope.outcome,
            candidate: state.candidate,
            checkpoint,
            replayed,
        })
    }

    async fn validate_writer_execution(
        &self,
        state: &mut NovelTaskState,
        active_branch: &str,
        started: &task_engine::StartedNode,
        execution: NovelWriterExecution,
    ) -> Result<WriterArtifactEnvelope, NovelWorkflowPortError> {
        if execution.raw_output.trim().is_empty() {
            let error = NovelWorkflowPortError::InvalidWriterOutput(
                "writer returned an empty raw response".into(),
            );
            self.fail_after_execution(
                &started.instance.instance_run_id,
                started.instance.version,
                &error.to_string(),
                Some(execution.usage),
            )
            .await?;
            return Err(error);
        }
        if let Err(error) = validate_outcome_branch(&execution.outcome, active_branch) {
            self.fail_after_execution(
                &started.instance.instance_run_id,
                started.instance.version,
                &error.to_string(),
                Some(execution.usage),
            )
            .await?;
            return Err(error);
        }
        if let Err(error) = state.apply_outcome(execution.outcome.clone()) {
            let port_error = NovelWorkflowPortError::from(error);
            self.fail_after_execution(
                &started.instance.instance_run_id,
                started.instance.version,
                &port_error.to_string(),
                Some(execution.usage),
            )
            .await?;
            return Err(port_error);
        }
        Ok(WriterArtifactEnvelope {
            version: WRITER_ARTIFACT_VERSION,
            outcome: execution.outcome,
            raw_output: execution.raw_output,
            usage: execution.usage,
        })
    }

    async fn persist_writer_execution(
        &self,
        state: &mut NovelTaskState,
        started: &task_engine::StartedNode,
        envelope: &WriterArtifactEnvelope,
    ) -> Result<TaskArtifact, NovelWorkflowPortError> {
        let artifact_content = match serde_json::to_string(envelope) {
            Ok(content) => content,
            Err(error) => {
                let port_error = NovelWorkflowPortError::from(error);
                self.fail_after_execution(
                    &started.instance.instance_run_id,
                    started.instance.version,
                    &port_error.to_string(),
                    Some(envelope.usage),
                )
                .await?;
                return Err(port_error);
            }
        };
        let artifact = match store_artifact(
            Arc::clone(&self.repository),
            &started.instance.instance_run_id,
            artifact_content,
        )
        .await
        {
            Ok(artifact) => artifact,
            Err(error) => {
                self.fail_after_execution(
                    &started.instance.instance_run_id,
                    started.instance.version,
                    &error.to_string(),
                    Some(envelope.usage),
                )
                .await?;
                return Err(error);
            }
        };
        if let Err(error) = seal_candidate(state, &envelope.outcome, &artifact) {
            self.fail_after_execution(
                &started.instance.instance_run_id,
                started.instance.version,
                &error.to_string(),
                Some(envelope.usage),
            )
            .await?;
            return Err(error);
        }
        complete_node(
            Arc::clone(&self.repository),
            &started.instance.instance_run_id,
            started.instance.version,
            envelope.usage,
            &artifact.artifact_id,
        )
        .await?;
        Ok(artifact)
    }

    async fn replay_completed(
        &self,
        task: TaskRun,
        frozen: FrozenStartContract,
    ) -> Result<NovelStartWorkflowResult, NovelWorkflowPortError> {
        validate_task_binding(&task, &frozen)?;
        let nodes = load_nodes(Arc::clone(&self.repository), &task.task_run_id).await?;
        if nodes.len() != 1 || nodes[0].state != NodeState::Completed {
            return Err(NovelWorkflowPortError::TaskEngine(format!(
                "completed writer task {} does not have one completed node",
                task.task_run_id
            )));
        }
        let artifact_id = nodes[0].output_artifact_id.as_deref().ok_or_else(|| {
            NovelWorkflowPortError::TaskEngine(format!(
                "completed writer node {} has no artifact",
                nodes[0].node_id
            ))
        })?;
        let artifact = load_artifact(Arc::clone(&self.repository), artifact_id).await?;
        validate_artifact_binding(&task, &nodes[0].node_id, &artifact)?;
        let envelope: WriterArtifactEnvelope = serde_json::from_str(&artifact.content)?;
        if envelope.version != WRITER_ARTIFACT_VERSION || envelope.raw_output.trim().is_empty() {
            return Err(NovelWorkflowPortError::InvalidWriterOutput(format!(
                "artifact {} has an unsupported or empty writer envelope",
                artifact.artifact_id
            )));
        }
        validate_outcome_branch(&envelope.outcome, &frozen.project.active_branch)?;
        let mut reconstructed = NovelTaskState::from_checkpoint(&frozen.initial_checkpoint)?;
        reconstructed.apply_outcome(envelope.outcome.clone())?;
        seal_candidate(&mut reconstructed, &envelope.outcome, &artifact)?;

        let checkpoint = match self
            .environment
            .load_checkpoint(&frozen.request.task_id)
            .await?
        {
            Some(existing) if checkpoint_contains_artifact(&existing, &frozen, &artifact)? => {
                existing
            }
            Some(existing) if checkpoint_has_progressed(&existing, &frozen)? => existing,
            _ => {
                let checkpoint = reconstructed.checkpoint()?;
                self.environment.save_checkpoint(checkpoint.clone()).await?;
                checkpoint
            }
        };
        self.append_completed_event(
            &reconstructed,
            &artifact,
            &frozen.execution_id,
            frozen.initial_checkpoint.updated_at,
        )
        .await?;
        Ok(NovelStartWorkflowResult {
            task_run_id: task.task_run_id,
            artifact_id: artifact.artifact_id,
            outcome: envelope.outcome,
            candidate: reconstructed.candidate,
            checkpoint,
            replayed: true,
        })
    }

    async fn persist_initial(
        &self,
        state: &NovelTaskState,
        execution_id: &str,
    ) -> Result<(), NovelWorkflowPortError> {
        let checkpoint = state.checkpoint()?;
        self.environment.save_checkpoint(checkpoint.clone()).await?;
        self.environment
            .append_task_event(task_event(
                state,
                &format!("drafting-{execution_id}"),
                NovelLifecycleActor::Main,
                "Novel Writer task admitted to the durable workflow",
                serde_json::json!({"task_run_id": task_run_id(execution_id)}),
                state.updated_at,
            ))
            .await
    }

    async fn append_completed_event(
        &self,
        state: &NovelTaskState,
        artifact: &TaskArtifact,
        execution_id: &str,
        created_at: i64,
    ) -> Result<(), NovelWorkflowPortError> {
        self.environment
            .append_task_event(task_event(
                state,
                &format!("writer_completed-{execution_id}"),
                NovelLifecycleActor::Novel,
                "Novel Writer produced a validated immutable artifact",
                serde_json::json!({
                    "task_run_id": artifact.task_run_id,
                    "artifact_id": artifact.artifact_id,
                    "artifact_content_hash": artifact.content_hash,
                    "candidate_id": state.candidate.as_ref().map(|candidate| &candidate.candidate_id),
                    "candidate_content_hash": state.candidate.as_ref().map(|candidate| &candidate.content_hash),
                }),
                created_at,
            ))
            .await
    }

    async fn persist_completed(
        &self,
        state: &NovelTaskState,
        artifact: &TaskArtifact,
        execution_id: &str,
        created_at: i64,
    ) -> Result<NovelTaskCheckpoint, NovelWorkflowPortError> {
        let checkpoint = state.checkpoint()?;
        self.environment.save_checkpoint(checkpoint.clone()).await?;
        self.append_completed_event(state, artifact, execution_id, created_at)
            .await?;
        Ok(checkpoint)
    }

    async fn fail_after_execution(
        &self,
        instance_run_id: &str,
        expected_version: u64,
        error: &str,
        usage: Option<ActualUsage>,
    ) -> Result<(), NovelWorkflowPortError> {
        let repository = Arc::clone(&self.repository);
        let instance_run_id = instance_run_id.to_string();
        let error = error.to_string();
        tokio::task::spawn_blocking(move || {
            repository.fail_node_after_execution(
                &instance_run_id,
                expected_version,
                &error,
                usage,
                false,
            )
        })
        .await
        .map_err(join_error)??;
        Ok(())
    }
}

fn task_run_id(task_id: &str) -> String {
    format!("novel-task-{task_id}")
}

fn instance_run_id(task_id: &str) -> String {
    format!("novel-writer-run-{task_id}")
}

fn task_event(
    state: &NovelTaskState,
    suffix: &str,
    actor: NovelLifecycleActor,
    summary: &str,
    details: serde_json::Value,
    created_at: i64,
) -> NovelTaskEvent {
    NovelTaskEvent {
        event_id: format!("novel-workflow-{}-{suffix}", state.request.task_id),
        task_id: state.request.task_id.clone(),
        project_id: state.request.project_id.clone(),
        actor,
        phase: state.phase,
        summary: summary.into(),
        details,
        created_at,
    }
}

fn validate_context_document(
    requested: &ContextRef,
    document: &NovelContextDocument,
) -> Result<(), NovelWorkflowPortError> {
    if document.content.is_empty() {
        return Err(NovelWorkflowPortError::ContextChanged(format!(
            "context {} is empty",
            requested.canonical_path.display()
        )));
    }
    if requested.role != document.reference.role
        || requested.description != document.reference.description
        || !requested
            .sha256
            .trim()
            .eq_ignore_ascii_case(document.reference.sha256.trim())
    {
        return Err(NovelWorkflowPortError::ContextChanged(format!(
            "context adapter changed the reference contract for {}",
            requested.canonical_path.display()
        )));
    }
    let actual = sha256_hex(document.content.as_bytes());
    if !actual.eq_ignore_ascii_case(requested.sha256.trim()) {
        return Err(NovelWorkflowPortError::ContextChanged(format!(
            "context {} hash mismatch: expected={}, actual={actual}",
            requested.canonical_path.display(),
            requested.sha256
        )));
    }
    Ok(())
}

fn build_context_snapshot(
    execution_id: &str,
    state: &NovelTaskState,
    project: &NovelProject,
    documents: &[NovelContextDocument],
    revision_instruction: Option<&str>,
) -> Result<ContextSnapshot, NovelWorkflowPortError> {
    let request = &state.request;
    let consistency = check_consistency(project);
    let mut blocks = vec![
        serialized_block(
            "novel-task-contract",
            ContextBlockKind::CurrentInput,
            request,
        )?,
        serialized_block("novel-project-snapshot", ContextBlockKind::Memory, project)?,
        serialized_block(
            "novel-consistency-report",
            ContextBlockKind::GraphEvidence,
            &consistency,
        )?,
        serialized_block("novel-task-state", ContextBlockKind::Memory, state)?,
    ];
    if let Some(instruction) = revision_instruction {
        blocks.push(serialized_block(
            "novel-revision-instruction",
            ContextBlockKind::CurrentInput,
            &instruction,
        )?);
    }
    for (index, document) in documents.iter().enumerate() {
        blocks.push(serialized_block(
            &format!("novel-context-{index}"),
            ContextBlockKind::Artifact,
            document,
        )?);
    }
    ContextSnapshot::new(format!("novel-context-{execution_id}"), blocks).map_err(Into::into)
}

fn serialized_block<T: Serialize>(
    block_id: &str,
    kind: ContextBlockKind,
    value: &T,
) -> Result<ContextBlock, NovelWorkflowPortError> {
    ContextBlock::from_input(ContextBlockInput::new(
        block_id,
        kind,
        serde_json::to_string(value)?,
    ))
    .map_err(Into::into)
}

fn frozen_contract(task: &TaskRun) -> Result<FrozenStartContract, NovelWorkflowPortError> {
    let mut frozen: FrozenStartContract = serde_json::from_value(
        task.resolved_config
            .get(START_CONTRACT_KEY)
            .cloned()
            .ok_or_else(|| {
                NovelWorkflowPortError::Serialization(format!(
                    "task {} has no frozen Novel start contract",
                    task.task_run_id
                ))
            })?,
    )?;
    if frozen.version != START_CONTRACT_VERSION {
        return Err(NovelWorkflowPortError::Serialization(format!(
            "task {} has unsupported Novel start contract version {}",
            task.task_run_id, frozen.version
        )));
    }
    if frozen.execution_id.is_empty() {
        frozen.execution_id.clone_from(&frozen.request.task_id);
    }
    Ok(frozen)
}

fn context_snapshot(task: &TaskRun) -> Result<ContextSnapshot, NovelWorkflowPortError> {
    let snapshot: ContextSnapshot = serde_json::from_value(
        task.resolved_config
            .get("context_snapshot")
            .cloned()
            .ok_or_else(|| {
                NovelWorkflowPortError::Serialization(format!(
                    "task {} has no frozen context snapshot",
                    task.task_run_id
                ))
            })?,
    )?;
    snapshot.validate()?;
    Ok(snapshot)
}

fn validate_task_binding(
    task: &TaskRun,
    frozen: &FrozenStartContract,
) -> Result<(), NovelWorkflowPortError> {
    if task.task_run_id != task_run_id(&frozen.execution_id)
        || task.origin_kind != "novel_task"
        || task.origin_id != frozen.execution_id
        || task.workflow != "novel.writer-stage"
        || task.config_version != WORKFLOW_CONFIG_VERSION
        || frozen.project.project_id != frozen.request.project_id
        || frozen.project.canon_revision != frozen.request.expected_revision
    {
        return Err(NovelWorkflowPortError::InvalidRequest(format!(
            "task {} does not match its frozen Novel start contract",
            task.task_run_id
        )));
    }
    let initial = NovelTaskState::from_checkpoint(&frozen.initial_checkpoint)?;
    let initial_execution = frozen.execution_id == frozen.request.task_id;
    let expected_revision_execution = format!(
        "{}-draft-{}",
        frozen.request.task_id,
        initial.draft_version + 1
    );
    let retry_execution = frozen
        .execution_id
        .strip_prefix(&format!("{expected_revision_execution}-retry-"))
        .is_some_and(|attempt| {
            !attempt.is_empty() && attempt.chars().all(|ch| ch.is_ascii_digit())
        });
    if initial.request != frozen.request
        || initial.phase != NovelTaskPhase::Drafting
        || (initial_execution && (initial.draft.is_some() || initial.draft_version != 0))
        || (!initial_execution
            && frozen.execution_id != expected_revision_execution
            && !retry_execution)
    {
        return Err(NovelWorkflowPortError::InvalidRequest(format!(
            "task {} has an invalid initial Domain checkpoint",
            task.task_run_id
        )));
    }
    let _ = context_snapshot(task)?;
    for document in &frozen.context_documents {
        validate_context_document(&document.reference, document)?;
    }
    Ok(())
}

fn validate_artifact_binding(
    task: &TaskRun,
    node_id: &str,
    artifact: &TaskArtifact,
) -> Result<(), NovelWorkflowPortError> {
    if artifact.task_run_id != task.task_run_id
        || artifact.node_id != node_id
        || sha256_hex(artifact.content.as_bytes()) != artifact.content_hash
    {
        return Err(NovelWorkflowPortError::InvalidWriterOutput(format!(
            "artifact {} is not bound to completed task {}",
            artifact.artifact_id, task.task_run_id
        )));
    }
    Ok(())
}

fn seal_candidate(
    state: &mut NovelTaskState,
    outcome: &NovelOutcome,
    artifact: &TaskArtifact,
) -> Result<Option<NovelCandidate>, NovelWorkflowPortError> {
    match outcome {
        NovelOutcome::DraftReady(draft) => state
            .seal_candidate(
                artifact.artifact_id.clone(),
                &sha256_hex(draft.content.as_bytes()),
            )
            .map(Some)
            .map_err(Into::into),
        NovelOutcome::NeedsClarification(_) => Ok(None),
    }
}

fn validate_outcome_branch(
    outcome: &NovelOutcome,
    active_branch: &str,
) -> Result<(), NovelWorkflowPortError> {
    if let NovelOutcome::DraftReady(draft) = outcome {
        if draft.proposed_delta.branch_id != active_branch {
            return Err(NovelWorkflowPortError::InvalidWriterOutput(format!(
                "writer delta branch {} does not match frozen active branch {active_branch}",
                draft.proposed_delta.branch_id
            )));
        }
    }
    Ok(())
}

fn checkpoint_contains_artifact(
    checkpoint: &NovelTaskCheckpoint,
    frozen: &FrozenStartContract,
    artifact: &TaskArtifact,
) -> Result<bool, NovelWorkflowPortError> {
    let state = NovelTaskState::from_checkpoint(checkpoint)?;
    Ok(state.request == frozen.request
        && state.candidate.as_ref().is_some_and(|candidate| {
            candidate.artifact_id == artifact.artifact_id
                && state.draft.as_ref().is_some_and(|draft| {
                    candidate.content_hash == sha256_hex(draft.content.as_bytes())
                })
        }))
}

fn checkpoint_has_progressed(
    checkpoint: &NovelTaskCheckpoint,
    frozen: &FrozenStartContract,
) -> Result<bool, NovelWorkflowPortError> {
    let state = NovelTaskState::from_checkpoint(checkpoint)?;
    if state.request != frozen.request {
        return Err(NovelWorkflowPortError::InvalidRequest(format!(
            "checkpoint {} does not match its frozen TaskRun request",
            checkpoint.task_id
        )));
    }
    Ok(checkpoint != &frozen.initial_checkpoint
        && (state.phase != NovelTaskPhase::Drafting
            || state.draft_version > frozen.initial_checkpoint.draft_version))
}

async fn load_task_optional(
    repository: Arc<TaskRepository>,
    task_run_id: &str,
) -> Result<Option<TaskRun>, NovelWorkflowPortError> {
    let task_run_id = task_run_id.to_string();
    let result = tokio::task::spawn_blocking(move || repository.task(&task_run_id))
        .await
        .map_err(join_error)?;
    match result {
        Ok(task) => Ok(Some(task)),
        Err(TaskEngineError::NotFound {
            entity: "task run", ..
        }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn create_task(
    repository: Arc<TaskRepository>,
    request: task_engine::NewTaskRun,
) -> Result<TaskRun, NovelWorkflowPortError> {
    tokio::task::spawn_blocking(move || repository.create_task(request))
        .await
        .map_err(join_error)?
        .map_err(Into::into)
}

async fn load_nodes(
    repository: Arc<TaskRepository>,
    task_run_id: &str,
) -> Result<Vec<task_engine::TaskNode>, NovelWorkflowPortError> {
    let task_run_id = task_run_id.to_string();
    tokio::task::spawn_blocking(move || repository.nodes(&task_run_id))
        .await
        .map_err(join_error)?
        .map_err(Into::into)
}

async fn store_artifact(
    repository: Arc<TaskRepository>,
    instance_run_id: &str,
    content: String,
) -> Result<TaskArtifact, NovelWorkflowPortError> {
    let instance_run_id = instance_run_id.to_string();
    tokio::task::spawn_blocking(move || {
        repository.store_artifact(&instance_run_id, &content, WRITER_ARTIFACT_MEDIA_TYPE)
    })
    .await
    .map_err(join_error)?
    .map_err(Into::into)
}

async fn load_artifact(
    repository: Arc<TaskRepository>,
    artifact_id: &str,
) -> Result<TaskArtifact, NovelWorkflowPortError> {
    let artifact_id = artifact_id.to_string();
    tokio::task::spawn_blocking(move || repository.artifact(&artifact_id))
        .await
        .map_err(join_error)?
        .map_err(Into::into)
}

async fn complete_node(
    repository: Arc<TaskRepository>,
    instance_run_id: &str,
    expected_version: u64,
    usage: ActualUsage,
    artifact_id: &str,
) -> Result<(), NovelWorkflowPortError> {
    let instance_run_id = instance_run_id.to_string();
    let artifact_id = artifact_id.to_string();
    tokio::task::spawn_blocking(move || {
        repository.complete_node(
            &instance_run_id,
            expected_version,
            usage,
            Some(&artifact_id),
        )
    })
    .await
    .map_err(join_error)??;
    Ok(())
}

fn join_error(error: tokio::task::JoinError) -> NovelWorkflowPortError {
    let message = error.to_string();
    drop(error);
    NovelWorkflowPortError::TaskEngine(format!("blocking TaskEngine worker failed: {message}"))
}
