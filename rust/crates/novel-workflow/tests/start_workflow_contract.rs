use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use novel_domain::{
    CanonStatus, ContextRef, ContextRole, NovelDraftEnvelope, NovelFactKind, NovelMemoryDelta,
    NovelOutcome, NovelProject, NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict,
    NovelTaskCheckpoint, NovelTaskEvent, NovelTaskPhase, NovelTaskRequest, NovelTaskState,
    NovelTaskType, ProposedFact, PublicationPolicy, ReviewCheckStatus, UserDecision,
    UserDecisionRecord,
};
use novel_workflow::{
    NovelContextDocument, NovelStartWorkflow, NovelTaskExecutionState, NovelWorkflowBudget,
    NovelWorkflowEnvironmentPort, NovelWorkflowModels, NovelWorkflowPortError,
    NovelWriterExecution, NovelWriterInvocation, NovelWriterPort, ProfileModel,
};
use task_engine::{
    ActualUsage, Scheduler, SchedulerLimits, TaskCoordinator, TaskRepository, TaskRunState,
};

#[derive(Default)]
struct FakeEnvironment {
    project: Mutex<Option<NovelProject>>,
    checkpoint: Mutex<Option<NovelTaskCheckpoint>>,
    events: Mutex<Vec<NovelTaskEvent>>,
    fail_final_checkpoint_once: AtomicBool,
    fail_completed_event_once: AtomicBool,
}

#[async_trait]
impl NovelWorkflowEnvironmentPort for FakeEnvironment {
    async fn load_project(
        &self,
        _project_id: &str,
    ) -> Result<NovelProject, NovelWorkflowPortError> {
        self.project
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| NovelWorkflowPortError::Storage("project missing".into()))
    }

    async fn load_checkpoint(
        &self,
        _task_id: &str,
    ) -> Result<Option<NovelTaskCheckpoint>, NovelWorkflowPortError> {
        Ok(self.checkpoint.lock().unwrap().clone())
    }

    async fn active_checkpoint_for_project(
        &self,
        _project_id: &str,
    ) -> Result<Option<NovelTaskCheckpoint>, NovelWorkflowPortError> {
        Ok(self.checkpoint.lock().unwrap().clone())
    }

    async fn read_context(
        &self,
        reference: &ContextRef,
    ) -> Result<NovelContextDocument, NovelWorkflowPortError> {
        Ok(NovelContextDocument {
            reference: reference.clone(),
            content: "outline body".into(),
        })
    }

    async fn save_checkpoint(
        &self,
        checkpoint: NovelTaskCheckpoint,
    ) -> Result<(), NovelWorkflowPortError> {
        if checkpoint.phase == NovelTaskPhase::AwaitingMainReview
            && self
                .fail_final_checkpoint_once
                .swap(false, Ordering::SeqCst)
        {
            return Err(NovelWorkflowPortError::Storage(
                "injected final checkpoint failure".into(),
            ));
        }
        *self.checkpoint.lock().unwrap() = Some(checkpoint);
        Ok(())
    }

    async fn append_task_event(&self, event: NovelTaskEvent) -> Result<(), NovelWorkflowPortError> {
        if event.phase == NovelTaskPhase::AwaitingMainReview
            && self.fail_completed_event_once.swap(false, Ordering::SeqCst)
        {
            return Err(NovelWorkflowPortError::Storage(
                "injected completed event failure".into(),
            ));
        }
        let mut events = self.events.lock().unwrap();
        if !events
            .iter()
            .any(|existing| existing.event_id == event.event_id)
        {
            events.push(event);
        }
        Ok(())
    }
}

struct FakeWriter {
    calls: AtomicUsize,
    failures_remaining: AtomicUsize,
    branch_id: &'static str,
}

struct UsageFailingWriter;

#[async_trait]
impl NovelWriterPort for UsageFailingWriter {
    async fn execute(
        &self,
        _invocation: NovelWriterInvocation,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
        Err(NovelWorkflowPortError::WriterExecutionFailed {
            message: "两次输出均不符合合同".into(),
            usage: ActualUsage {
                input_tokens: 52,
                output_tokens: 11,
            },
        })
    }
}

#[async_trait]
impl NovelWriterPort for FakeWriter {
    async fn execute(
        &self,
        invocation: NovelWriterInvocation,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failures_remaining.load(Ordering::SeqCst) > 0 {
            self.failures_remaining.fetch_sub(1, Ordering::SeqCst);
            return Err(NovelWorkflowPortError::InvalidWriterOutput(
                "injected writer serialization failure".into(),
            ));
        }
        let outcome = NovelOutcome::DraftReady(NovelDraftEnvelope {
            task_id: invocation.request.task_id.clone(),
            draft_version: invocation.next_draft_version,
            project_id: invocation.request.project_id.clone(),
            canon_revision: invocation.request.expected_revision,
            content: "chapter one".into(),
            self_review: NovelSelfReview {
                verdict: NovelSelfReviewVerdict::Pass,
                checks: NovelSelfReviewChecks {
                    outline_alignment: ReviewCheckStatus::Pass,
                    canon_consistency: ReviewCheckStatus::Pass,
                    character_consistency: ReviewCheckStatus::Pass,
                    timeline_consistency: ReviewCheckStatus::Pass,
                    plot_and_foreshadowing: ReviewCheckStatus::Pass,
                    style_and_repetition: ReviewCheckStatus::Pass,
                },
                issues: Vec::new(),
                unverified_assumptions: Vec::new(),
                summary: "passed".into(),
            },
            proposed_delta: NovelMemoryDelta {
                project_id: invocation.request.project_id.clone(),
                branch_id: self.branch_id.into(),
                expected_revision: invocation.request.expected_revision,
                task_type: invocation.request.task_type.clone(),
                source_ref: invocation
                    .request
                    .output_path
                    .to_string_lossy()
                    .into_owned(),
                progress: None,
                proposed_facts: vec![ProposedFact {
                    fact_id: "fact-1".into(),
                    kind: NovelFactKind::Character,
                    subject_key: "character:aria".into(),
                    title: "Aria".into(),
                    summary: "Aria opens the gate".into(),
                    data: serde_json::Value::Null,
                    status: CanonStatus::Confirmed,
                    valid_from_chapter: Some(1),
                    valid_to_chapter: None,
                    source_refs: Vec::new(),
                    confidence: 0.9,
                }],
                state_changes: Vec::new(),
                plot_updates: Vec::new(),
                foreshadowing_updates: Vec::new(),
                feedback: Vec::new(),
                experience_candidates: Vec::new(),
            },
            evidence_refs: vec!["requirements".into(), "canon".into()],
        });
        Ok(NovelWriterExecution {
            raw_output: serde_json::to_string(&outcome).unwrap(),
            outcome,
            usage: ActualUsage {
                input_tokens: 100,
                output_tokens: 200,
            },
        })
    }
}

fn request() -> NovelTaskRequest {
    NovelTaskRequest {
        task_id: "task-1".into(),
        project_id: "project-1".into(),
        task_type: NovelTaskType::Body,
        task_brief: "Write chapter one".into(),
        target_chapter: Some(1),
        expected_revision: 0,
        output_path: PathBuf::from("chapters/0001.md"),
        context_refs: vec![ContextRef {
            role: ContextRole::ChapterOutline,
            canonical_path: PathBuf::from("outline.md"),
            sha256: novel_domain::sha256_hex(b"outline body"),
            description: None,
        }],
        must_happen: Vec::new(),
        must_not_change: Vec::new(),
        acceptance_criteria: vec!["complete chapter".into()],
        allow_web_research: false,
        publication_policy: PublicationPolicy::RequireUserAcceptance,
        parent_task_id: None,
        source_conversation_id: Some("room-1".into()),
        source_generation_id: Some("generation-1".into()),
    }
}

fn models() -> NovelWorkflowModels {
    let model = ProfileModel::new("test-provider", "test-model");
    NovelWorkflowModels {
        writer: model.clone(),
        reviewer: model.clone(),
        canon_extractor: model,
    }
}

fn coordinator(repository: Arc<TaskRepository>) -> TaskCoordinator {
    TaskCoordinator::new(
        repository,
        Scheduler::new(SchedulerLimits {
            max_workers: 2,
            max_global: 2,
            max_per_room: 2,
            max_per_member: 2,
            max_per_provider: 2,
            max_per_profile: 2,
            max_per_task: 2,
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn start_is_task_backed_and_replays_without_a_second_model_call() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("runtime.db");
    let repository = Arc::new(TaskRepository::open(&database).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    let writer = Arc::new(FakeWriter {
        calls: AtomicUsize::new(0),
        failures_remaining: AtomicUsize::new(0),
        branch_id: "main",
    });
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment.clone(),
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    let first = service.start_task(request()).await.unwrap();
    assert!(!first.replayed);
    assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
    assert!(first.candidate.is_some());
    assert_eq!(
        repository.task(&first.task_run_id).unwrap().state,
        TaskRunState::Completed
    );
    let saved = environment.checkpoint.lock().unwrap().clone().unwrap();
    assert!(saved.state["candidate"]["artifact_id"].is_string());

    environment
        .project
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .canon_revision = 99;
    let replay = service.start_task(request()).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.artifact_id, first.artifact_id);
    assert_eq!(writer.calls.load(Ordering::SeqCst), 1);

    drop(service);
    drop(repository);
    let reopened = Arc::new(TaskRepository::open(&database).unwrap());
    let restarted = NovelStartWorkflow::new(
        Arc::clone(&reopened),
        coordinator(Arc::clone(&reopened)),
        environment,
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );
    let replay = restarted.start_task(request()).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.artifact_id, first.artifact_id);
    assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_writer_persists_known_actual_usage() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(TaskRepository::open(dir.path().join("runtime.db")).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment,
        Arc::new(UsageFailingWriter),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    service.start_task(request()).await.unwrap_err();

    let instance = repository.instance("novel-writer-run-task-1").unwrap();
    assert_eq!(
        instance.usage,
        Some(ActualUsage {
            input_tokens: 52,
            output_tokens: 11,
        })
    );
}

#[tokio::test]
async fn task_execution_state_reports_failed_and_missing_task_runs() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(TaskRepository::open(dir.path().join("runtime.db")).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment,
        Arc::new(UsageFailingWriter),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    service.start_task(request()).await.unwrap_err();

    assert_eq!(
        service.task_execution_state("task-1").await.unwrap(),
        Some(NovelTaskExecutionState::Failed)
    );
    assert_eq!(
        service.task_execution_state("missing-task").await.unwrap(),
        None
    );
}

#[test]
fn task_execution_state_maps_every_task_engine_state() {
    assert_eq!(
        NovelTaskExecutionState::from(TaskRunState::Queued),
        NovelTaskExecutionState::Queued
    );
    assert_eq!(
        NovelTaskExecutionState::from(TaskRunState::Running),
        NovelTaskExecutionState::Running
    );
    assert_eq!(
        NovelTaskExecutionState::from(TaskRunState::PausedBudget),
        NovelTaskExecutionState::PausedBudget
    );
    assert_eq!(
        NovelTaskExecutionState::from(TaskRunState::NeedsInput),
        NovelTaskExecutionState::NeedsInput
    );
    assert_eq!(
        NovelTaskExecutionState::from(TaskRunState::Completed),
        NovelTaskExecutionState::Completed
    );
    assert_eq!(
        NovelTaskExecutionState::from(TaskRunState::Failed),
        NovelTaskExecutionState::Failed
    );
    assert_eq!(
        NovelTaskExecutionState::from(TaskRunState::Cancelled),
        NovelTaskExecutionState::Cancelled
    );
}

#[tokio::test]
async fn revision_is_a_separate_restart_safe_writer_task_run() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(TaskRepository::open(dir.path().join("runtime.db")).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    let writer = Arc::new(FakeWriter {
        calls: AtomicUsize::new(0),
        failures_remaining: AtomicUsize::new(0),
        branch_id: "main",
    });
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment.clone(),
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    service.start_task(request()).await.unwrap();
    let checkpoint = environment.checkpoint.lock().unwrap().clone().unwrap();
    let mut state = NovelTaskState::from_checkpoint(&checkpoint).unwrap();
    state.phase = NovelTaskPhase::AwaitingUserDecision;
    state
        .record_user_decision(UserDecisionRecord {
            task_id: "task-1".into(),
            draft_version: 1,
            decision: UserDecision::Revise,
            feedback: Some("Tighten the pacing".into()),
            decided_at: 40,
        })
        .unwrap();
    *environment.checkpoint.lock().unwrap() = Some(state.checkpoint().unwrap());

    let revised = Box::pin(service.continue_task("task-1", "Tighten the pacing"))
        .await
        .unwrap();
    assert_eq!(revised.task_run_id, "novel-task-task-1-draft-2");
    assert_eq!(revised.checkpoint.draft_version, 2);
    assert_eq!(writer.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        repository.task(&revised.task_run_id).unwrap().state,
        TaskRunState::Completed
    );
}

#[tokio::test]
async fn failed_frozen_iteration_can_retry_with_a_new_resume_instruction() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(TaskRepository::open(dir.path().join("runtime.db")).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    let writer = Arc::new(FakeWriter {
        calls: AtomicUsize::new(0),
        failures_remaining: AtomicUsize::new(2),
        branch_id: "main",
    });
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment,
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    service.start_task(request()).await.unwrap_err();
    service
        .continue_task("task-1", "第一次恢复")
        .await
        .unwrap_err();
    let recovered = service
        .continue_task("task-1", "第二次恢复，使用兼容解析")
        .await
        .unwrap();

    assert_eq!(recovered.checkpoint.draft_version, 1);
    assert_eq!(writer.calls.load(Ordering::SeqCst), 3);
    assert!(recovered.task_run_id.contains("retry-"));
}

#[tokio::test]
async fn completed_frozen_iteration_replays_before_allocating_another_retry() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(TaskRepository::open(dir.path().join("runtime.db")).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    environment
        .fail_final_checkpoint_once
        .store(true, Ordering::SeqCst);
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    let writer = Arc::new(FakeWriter {
        calls: AtomicUsize::new(0),
        failures_remaining: AtomicUsize::new(1),
        branch_id: "main",
    });
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment,
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    service.start_task(request()).await.unwrap_err();
    service
        .continue_task("task-1", "第一次恢复")
        .await
        .unwrap_err();
    let replayed = service.continue_task("task-1", "第二次恢复").await.unwrap();

    assert!(replayed.replayed);
    assert_eq!(replayed.task_run_id, "novel-task-task-1-draft-1");
    assert_eq!(writer.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn writer_delta_must_match_the_frozen_active_branch() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(TaskRepository::open(dir.path().join("runtime.db")).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    let writer = Arc::new(FakeWriter {
        calls: AtomicUsize::new(0),
        failures_remaining: AtomicUsize::new(0),
        branch_id: "alternate",
    });
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment.clone(),
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    let error = service.start_task(request()).await.unwrap_err();
    assert!(error.to_string().contains("frozen active branch main"));
    assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        repository.task("novel-task-task-1").unwrap().state,
        TaskRunState::Failed
    );
    let checkpoint = environment.checkpoint.lock().unwrap().clone().unwrap();
    assert_eq!(checkpoint.phase, NovelTaskPhase::Drafting);
    assert!(checkpoint.state["draft"].is_null());
}

#[tokio::test]
async fn completed_task_reconciles_checkpoint_and_event_crash_windows() {
    assert_completed_reconciliation(true, false).await;
    assert_completed_reconciliation(false, true).await;
}

async fn assert_completed_reconciliation(fail_checkpoint: bool, fail_event: bool) {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("runtime.db");
    let repository = Arc::new(TaskRepository::open(&database).unwrap());
    let environment = Arc::new(FakeEnvironment::default());
    *environment.project.lock().unwrap() = Some(NovelProject::new("project-1", "Project"));
    environment
        .fail_final_checkpoint_once
        .store(fail_checkpoint, Ordering::SeqCst);
    environment
        .fail_completed_event_once
        .store(fail_event, Ordering::SeqCst);
    let writer = Arc::new(FakeWriter {
        calls: AtomicUsize::new(0),
        failures_remaining: AtomicUsize::new(0),
        branch_id: "main",
    });
    let service = NovelStartWorkflow::new(
        Arc::clone(&repository),
        coordinator(Arc::clone(&repository)),
        environment.clone(),
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );

    assert!(service.start_task(request()).await.is_err());
    assert_eq!(
        repository.task("novel-task-task-1").unwrap().state,
        TaskRunState::Completed
    );
    assert_eq!(writer.calls.load(Ordering::SeqCst), 1);

    drop(service);
    drop(repository);
    let reopened = Arc::new(TaskRepository::open(&database).unwrap());
    let restarted = NovelStartWorkflow::new(
        Arc::clone(&reopened),
        coordinator(Arc::clone(&reopened)),
        environment.clone(),
        writer.clone(),
        models(),
        NovelWorkflowBudget {
            input_tokens: 1_000,
            output_tokens: 1_000,
        },
    );
    let replay = restarted.start_task(request()).await.unwrap();
    assert!(replay.replayed);
    assert!(replay.candidate.is_some());
    assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(environment.events.lock().unwrap().len(), 2);
    let saved = environment.checkpoint.lock().unwrap().clone().unwrap();
    assert!(saved.state["candidate"]["artifact_id"].is_string());
}
