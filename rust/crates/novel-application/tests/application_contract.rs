use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use novel_application::{
    NovelApplicationError, NovelApplicationService, NovelDomainStore, NovelResourcePort,
    NovelTaskUnlockReceipt, StoreWorkflowEnvironment,
};
use novel_domain::{
    CanonStatus, ConflictRecord, MainReviewChecks, MainReviewRecord, MainReviewVerdict,
    NovelArtifactReceipt, NovelDraftEnvelope, NovelFactKind, NovelLifecycleActor, NovelMemoryDelta,
    NovelOutcome, NovelProject, NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict,
    NovelTaskPhase, NovelTaskRequest, NovelTaskState, NovelTaskType, ProposedFact,
    PublicationPolicy, ReviewCheckStatus, UserDecision, UserDecisionRecord,
};
use novel_workflow::{
    NovelStartWorkflow, NovelTaskExecutionState, NovelWorkflowBudget, NovelWorkflowEnvironmentPort,
    NovelWorkflowModels, NovelWorkflowPortError, NovelWriterExecution, NovelWriterInvocation,
    NovelWriterPort, ProfileModel,
};
use task_engine::{
    ActualUsage, BudgetLimits, BudgetRequest, NewTaskNode, NewTaskRun, NodeKind, Scheduler,
    SchedulerLimits, TaskCoordinator, TaskRepository, TaskRunState,
};

struct TestResources {
    root: PathBuf,
}

struct PanicWriter {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl NovelWriterPort for PanicWriter {
    async fn execute(
        &self,
        _invocation: NovelWriterInvocation,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("失败解锁不得调用 Writer")
    }
}

#[async_trait]
impl NovelResourcePort for TestResources {
    async fn read_context(
        &self,
        _reference: &novel_domain::ContextRef,
    ) -> novel_application::Result<novel_workflow::NovelContextDocument> {
        unreachable!("publication test has no context")
    }

    async fn resolve_artifact_path(&self, path: &Path) -> novel_application::Result<PathBuf> {
        Ok(self.root.join(path))
    }

    async fn write_artifact_atomic(
        &self,
        path: &Path,
        content: &str,
    ) -> novel_application::Result<NovelArtifactReceipt> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if path.exists() {
            let existing = std::fs::read_to_string(&path).unwrap();
            assert_eq!(existing, content);
        } else {
            std::fs::write(&path, content).unwrap();
        }
        Ok(NovelArtifactReceipt {
            canonical_path: path.to_string_lossy().into_owned(),
            sha256: novel_domain::sha256_hex(content.as_bytes()),
            bytes: content.len() as u64,
            written_at: 50,
        })
    }

    async fn verify_artifact(
        &self,
        path: &Path,
        expected_sha256: &str,
    ) -> novel_application::Result<Option<NovelArtifactReceipt>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read(path).unwrap();
        let actual = novel_domain::sha256_hex(&content);
        assert_eq!(actual, expected_sha256);
        Ok(Some(NovelArtifactReceipt {
            canonical_path: path.to_string_lossy().into_owned(),
            sha256: actual,
            bytes: content.len() as u64,
            written_at: 50,
        }))
    }
}

fn task_request() -> NovelTaskRequest {
    NovelTaskRequest {
        task_id: "task-1".into(),
        project_id: "project-1".into(),
        task_type: NovelTaskType::Body,
        task_brief: "Write chapter one".into(),
        target_chapter: Some(1),
        expected_revision: 0,
        output_path: "chapters/0001.md".into(),
        context_refs: Vec::new(),
        must_happen: Vec::new(),
        must_not_change: Vec::new(),
        acceptance_criteria: vec!["complete chapter".into()],
        allow_web_research: false,
        publication_policy: PublicationPolicy::RequireUserAcceptance,
        parent_task_id: None,
        source_conversation_id: None,
        source_generation_id: None,
    }
}

fn empty_drafting_state() -> NovelTaskState {
    let mut state = NovelTaskState::new(task_request()).unwrap();
    state.begin_drafting().unwrap();
    state
}

fn workflow_models() -> NovelWorkflowModels {
    let model = ProfileModel::new("test-provider", "test-model");
    NovelWorkflowModels {
        writer: model.clone(),
        reviewer: model.clone(),
        canon_extractor: model,
    }
}

fn task_coordinator(repository: Arc<TaskRepository>) -> TaskCoordinator {
    TaskCoordinator::new(
        repository,
        Scheduler::new(SchedulerLimits {
            max_workers: 1,
            max_global: 1,
            max_per_room: 1,
            max_per_member: 1,
            max_per_provider: 1,
            max_per_profile: 1,
            max_per_task: 1,
        })
        .unwrap(),
    )
}

fn task_run(task_id: &str) -> NewTaskRun {
    NewTaskRun {
        task_run_id: format!("novel-task-{task_id}"),
        workflow: "novel.test".into(),
        objective: "test failed task unlock".into(),
        origin_kind: "novel_task".into(),
        origin_id: task_id.into(),
        room_id: None,
        config_version: "test-v1".into(),
        resolved_config: serde_json::json!({"test": true}),
        parent_budget_account_id: None,
        budget: BudgetLimits {
            input_tokens: 100,
            output_tokens: 100,
        },
        nodes: vec![NewTaskNode {
            node_id: format!("novel-writer-{task_id}"),
            kind: NodeKind::Model,
            dependencies: Vec::new(),
            provider: "test-provider".into(),
            model: "test-model".into(),
            profile: "novel.writer.v1".into(),
            room_id: None,
            member_id: None,
            reservation: BudgetRequest {
                input_tokens: 10,
                output_tokens: 10,
            },
            retryable: false,
            side_effecting: false,
        }],
    }
}

fn create_task_run_in_state(repository: &TaskRepository, task_id: &str, state: TaskRunState) {
    let created = repository.create_task(task_run(task_id)).unwrap();
    let task_run_id = format!("novel-task-{task_id}");
    let node_id = format!("novel-writer-{task_id}");
    match state {
        TaskRunState::Queued => {}
        TaskRunState::Running => {
            let node = repository.node(&node_id).unwrap();
            repository
                .start_node(&node_id, node.version, &format!("instance-{task_id}"))
                .unwrap();
        }
        TaskRunState::PausedBudget => {
            repository
                .pause_task_for_budget(&task_run_id, "injected setup pause")
                .unwrap();
        }
        TaskRunState::NeedsInput => {
            let node = repository.node(&node_id).unwrap();
            repository
                .start_node(&node_id, node.version, &format!("instance-{task_id}"))
                .unwrap();
            repository.recover_inflight().unwrap();
        }
        TaskRunState::Completed => {
            let node = repository.node(&node_id).unwrap();
            let started = repository
                .start_node(&node_id, node.version, &format!("instance-{task_id}"))
                .unwrap();
            repository
                .complete_node(
                    &started.instance.instance_run_id,
                    started.instance.version,
                    ActualUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                    },
                    None,
                )
                .unwrap();
        }
        TaskRunState::Failed => {
            let node = repository.node(&node_id).unwrap();
            let started = repository
                .start_node(&node_id, node.version, &format!("instance-{task_id}"))
                .unwrap();
            repository
                .fail_node(
                    &started.instance.instance_run_id,
                    started.instance.version,
                    "injected setup failure",
                    false,
                )
                .unwrap();
        }
        TaskRunState::Cancelled => {
            repository
                .cancel_task(&task_run_id, created.version)
                .unwrap();
        }
    }
    assert_eq!(repository.task(&task_run_id).unwrap().state, state);
}

struct UnlockFixture {
    _directory: tempfile::TempDir,
    store: Arc<NovelDomainStore>,
    repository: Arc<TaskRepository>,
    writer_calls: Arc<AtomicUsize>,
    service: NovelApplicationService,
}

fn unlock_fixture(state: &NovelTaskState, execution_state: Option<TaskRunState>) -> UnlockFixture {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(NovelDomainStore::open(directory.path().join("novel.db")).unwrap());
    store
        .import_project(&NovelProject::new("project-1", "Project"))
        .unwrap();
    store.save_checkpoint(&state.checkpoint().unwrap()).unwrap();

    let repository = Arc::new(TaskRepository::open(directory.path().join("runtime.db")).unwrap());
    if let Some(execution_state) = execution_state {
        create_task_run_in_state(&repository, &state.request.task_id, execution_state);
    }
    let resources = Arc::new(TestResources {
        root: directory.path().join("workspace"),
    });
    let environment: Arc<dyn NovelWorkflowEnvironmentPort> = Arc::new(
        StoreWorkflowEnvironment::new(Arc::clone(&store), resources.clone()),
    );
    let writer_calls = Arc::new(AtomicUsize::new(0));
    let writer: Arc<dyn NovelWriterPort> = Arc::new(PanicWriter {
        calls: Arc::clone(&writer_calls),
    });
    let workflow = Arc::new(NovelStartWorkflow::new(
        Arc::clone(&repository),
        task_coordinator(Arc::clone(&repository)),
        environment,
        writer,
        workflow_models(),
        NovelWorkflowBudget {
            input_tokens: 100,
            output_tokens: 100,
        },
    ));
    let service = NovelApplicationService::new(Arc::clone(&store), Some(workflow), resources);

    UnlockFixture {
        _directory: directory,
        store,
        repository,
        writer_calls,
        service,
    }
}

#[tokio::test]
async fn failed_task_unlock_is_atomic_and_audited_without_writer_call() {
    let fixture = unlock_fixture(&empty_drafting_state(), Some(TaskRunState::Failed));

    let receipt: NovelTaskUnlockReceipt = fixture
        .service
        .unlock_failed_task("task-1", "  人工确认执行失败  ")
        .await
        .unwrap();

    assert_eq!(receipt.task_id, "task-1");
    assert_eq!(receipt.project_id, "project-1");
    assert_eq!(receipt.previous_phase, NovelTaskPhase::Drafting);
    assert_eq!(receipt.phase, NovelTaskPhase::Cancelled);
    assert_eq!(receipt.execution_state, NovelTaskExecutionState::Failed);
    assert!(!receipt.already_unlocked);
    assert_eq!(receipt.reason, "人工确认执行失败");
    let checkpoint = fixture.store.load_checkpoint("task-1").unwrap().unwrap();
    assert_eq!(checkpoint.phase, NovelTaskPhase::Cancelled);
    assert!(checkpoint.phase.is_terminal());
    assert!(fixture
        .store
        .active_checkpoint_for_project("project-1")
        .unwrap()
        .is_none());
    assert_eq!(
        fixture.repository.task("novel-task-task-1").unwrap().state,
        TaskRunState::Failed
    );
    let events = fixture.store.load_task_events("task-1").unwrap();
    let event = events.last().unwrap();
    assert!(event.event_id.contains("-manual_unlock-"));
    assert_eq!(event.actor, NovelLifecycleActor::System);
    assert_eq!(event.phase, NovelTaskPhase::Cancelled);
    assert_eq!(event.summary, "manual_unlock");
    assert_eq!(
        event.details,
        serde_json::json!({
            "reason": "人工确认执行失败",
            "previous_phase": "drafting",
            "execution_state": "failed",
        })
    );
    let status = fixture.service.status(Some("project-1")).unwrap();
    assert_eq!(status.projects.len(), 1);
    assert!(status.projects[0].task_id.is_none());
    assert!(status.projects[0].phase.is_none());
    assert_eq!(fixture.writer_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelled_task_run_allows_failed_task_unlock() {
    let fixture = unlock_fixture(&empty_drafting_state(), Some(TaskRunState::Cancelled));

    let receipt = fixture
        .service
        .unlock_failed_task("task-1", "运行已取消")
        .await
        .unwrap();

    assert_eq!(receipt.execution_state, NovelTaskExecutionState::Cancelled);
    assert_eq!(receipt.phase, NovelTaskPhase::Cancelled);
    assert!(!receipt.already_unlocked);
    assert_eq!(fixture.store.load_task_events("task-1").unwrap().len(), 1);
    assert_eq!(fixture.writer_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unsafe_task_engine_states_reject_unlock_without_mutation() {
    for execution_state in [
        TaskRunState::Queued,
        TaskRunState::Running,
        TaskRunState::PausedBudget,
        TaskRunState::NeedsInput,
        TaskRunState::Completed,
    ] {
        let fixture = unlock_fixture(&empty_drafting_state(), Some(execution_state));
        let checkpoint = fixture.store.load_checkpoint("task-1").unwrap().unwrap();
        let event_count = fixture.store.load_task_events("task-1").unwrap().len();

        let error = fixture
            .service
            .unlock_failed_task("task-1", "人工确认")
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            NovelApplicationError::Conflict(message)
                if message.contains(&format!("{execution_state:?}"))
                    && message.contains("拒绝失败解锁")
        ));
        assert_eq!(
            fixture.store.load_checkpoint("task-1").unwrap().unwrap(),
            checkpoint
        );
        assert_eq!(
            fixture.store.load_task_events("task-1").unwrap().len(),
            event_count
        );
        assert_eq!(fixture.writer_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn missing_task_run_rejects_unlock_without_mutation() {
    let fixture = unlock_fixture(&empty_drafting_state(), None);
    let checkpoint = fixture.store.load_checkpoint("task-1").unwrap().unwrap();

    let error = fixture
        .service
        .unlock_failed_task("task-1", "人工确认")
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        NovelApplicationError::NotFound(message)
            if message == "Task Engine 运行记录 novel-task-task-1"
    ));
    assert_eq!(
        fixture.store.load_checkpoint("task-1").unwrap().unwrap(),
        checkpoint
    );
    assert!(fixture.store.load_task_events("task-1").unwrap().is_empty());
    assert_eq!(fixture.writer_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn domain_content_and_publication_guards_reject_unlock_without_mutation() {
    let mut draft_state = approved_state();
    draft_state.main_review = None;
    draft_state.user_decision = None;
    draft_state.phase = NovelTaskPhase::AwaitingMainReview;
    let mut candidate_state = draft_state.clone();
    let candidate_hash =
        novel_domain::sha256_hex(candidate_state.draft.as_ref().unwrap().content.as_bytes());
    candidate_state
        .seal_candidate("artifact-candidate-1", &candidate_hash)
        .unwrap();
    let mut reviewed_state = approved_state();
    reviewed_state.user_decision = None;
    reviewed_state.phase = NovelTaskPhase::AwaitingUserDecision;
    let decided_state = approved_state();
    let mut publication_state = approved_state();
    publication_state.mark_publication_pending("publication-1".into());

    for state in [
        draft_state,
        candidate_state,
        reviewed_state,
        decided_state,
        publication_state,
    ] {
        let fixture = unlock_fixture(&state, Some(TaskRunState::Failed));
        let checkpoint = fixture.store.load_checkpoint("task-1").unwrap().unwrap();

        let error = fixture
            .service
            .unlock_failed_task("task-1", "人工确认")
            .await
            .unwrap_err();

        assert!(matches!(error, NovelApplicationError::Domain(_)));
        assert_eq!(
            fixture.store.load_checkpoint("task-1").unwrap().unwrap(),
            checkpoint
        );
        assert!(fixture.store.load_task_events("task-1").unwrap().is_empty());
        assert_eq!(fixture.writer_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn repeated_unlock_is_idempotent_and_keeps_one_audit_event() {
    let fixture = unlock_fixture(&empty_drafting_state(), Some(TaskRunState::Failed));

    let first = fixture
        .service
        .unlock_failed_task("task-1", "首次人工确认")
        .await
        .unwrap();
    let second = fixture
        .service
        .unlock_failed_task("task-1", "重复人工确认")
        .await
        .unwrap();

    assert!(!first.already_unlocked);
    assert!(second.already_unlocked);
    assert_eq!(second.previous_phase, NovelTaskPhase::Cancelled);
    assert_eq!(second.phase, NovelTaskPhase::Cancelled);
    let events = fixture.store.load_task_events("task-1").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].summary, "manual_unlock");
    assert_eq!(events[0].details["reason"], "首次人工确认");
    assert_eq!(fixture.writer_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_unlock_reasons_are_rejected_before_state_changes() {
    let fixture = unlock_fixture(&empty_drafting_state(), Some(TaskRunState::Failed));
    let checkpoint = fixture.store.load_checkpoint("task-1").unwrap().unwrap();
    let too_long = "理".repeat(257);
    let invalid_reasons = [
        "",
        "   ",
        too_long.as_str(),
        "合法原因\n",
        "\t合法原因",
        "合法\u{0085}原因",
    ];

    for reason in invalid_reasons {
        let error = fixture
            .service
            .unlock_failed_task("task-1", reason)
            .await
            .unwrap_err();
        assert!(matches!(error, NovelApplicationError::Conflict(_)));
        assert_eq!(
            fixture.store.load_checkpoint("task-1").unwrap().unwrap(),
            checkpoint
        );
        assert!(fixture.store.load_task_events("task-1").unwrap().is_empty());
        assert_eq!(fixture.writer_calls.load(Ordering::SeqCst), 0);
    }
}

fn approved_state() -> NovelTaskState {
    let mut state = NovelTaskState::new(task_request()).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::DraftReady(NovelDraftEnvelope {
            task_id: "task-1".into(),
            draft_version: 1,
            project_id: "project-1".into(),
            canon_revision: 0,
            content: "Chapter one".into(),
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
                project_id: "project-1".into(),
                branch_id: "main".into(),
                expected_revision: 0,
                task_type: NovelTaskType::Body,
                source_ref: "chapters/0001.md".into(),
                progress: None,
                proposed_facts: vec![ProposedFact {
                    fact_id: "fact-1".into(),
                    kind: NovelFactKind::Event,
                    subject_key: "event:arrival".into(),
                    title: "Arrival".into(),
                    summary: "The traveler arrives".into(),
                    data: serde_json::Value::Null,
                    status: CanonStatus::Confirmed,
                    valid_from_chapter: Some(1),
                    valid_to_chapter: None,
                    source_refs: vec!["chapters/0001.md".into()],
                    confidence: 1.0,
                }],
                state_changes: Vec::new(),
                plot_updates: Vec::new(),
                foreshadowing_updates: Vec::new(),
                feedback: Vec::new(),
                experience_candidates: Vec::new(),
            },
            evidence_refs: vec!["canon:project-1@0".into()],
        }))
        .unwrap();
    state
        .record_main_review(MainReviewRecord {
            task_id: "task-1".into(),
            draft_version: 1,
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
            evidence_refs: vec![
                "requirements:task-1".into(),
                "canon:project-1@0".into(),
                "artifact:draft-1".into(),
            ],
            summary: "pass".into(),
        })
        .unwrap();
    state
        .record_user_decision(UserDecisionRecord {
            task_id: "task-1".into(),
            draft_version: 1,
            decision: UserDecision::Accept,
            feedback: None,
            decided_at: 40,
        })
        .unwrap();
    state
}

#[tokio::test]
async fn approved_publication_is_restart_idempotent_and_enqueues_new_canon() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("novel.db");
    let store = Arc::new(NovelDomainStore::open(&database).unwrap());
    store
        .import_project(&NovelProject::new("project-1", "Project"))
        .unwrap();
    store
        .save_checkpoint(&approved_state().checkpoint().unwrap())
        .unwrap();
    let resources = Arc::new(TestResources {
        root: dir.path().join("workspace"),
    });
    let service = NovelApplicationService::new(Arc::clone(&store), None, resources.clone());

    let first = service.publish("task-1", 1).await.unwrap();
    assert_eq!(first.commit_report.previous_revision, 0);
    assert_eq!(first.commit_report.new_revision, 1);
    assert_eq!(store.load_project("project-1").unwrap().canon_revision, 1);
    assert_eq!(store.pending_outbox(10).unwrap().len(), 2);

    drop(service);
    drop(store);
    let reopened = Arc::new(NovelDomainStore::open(&database).unwrap());
    let restarted = NovelApplicationService::new(reopened.clone(), None, resources);
    let replay = restarted.publish("task-1", 1).await.unwrap();
    assert_eq!(replay, first);
    assert_eq!(
        reopened.load_project("project-1").unwrap().canon_revision,
        1
    );
    assert_eq!(reopened.pending_outbox(10).unwrap().len(), 2);
}

#[tokio::test]
async fn project_commands_use_the_domain_store_and_enqueue_resolved_canon() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(NovelDomainStore::open(dir.path().join("novel.db")).unwrap());
    let resources = Arc::new(TestResources {
        root: dir.path().join("workspace"),
    });
    let service = NovelApplicationService::new(Arc::clone(&store), None, resources);
    let mut project = NovelProject::new("project-1", "Project");
    project.conflicts.push(ConflictRecord {
        conflict_id: "conflict-1".into(),
        subject_key: "character:lead".into(),
        existing_fact_id: "fact-old".into(),
        proposed_fact_id: "fact-new".into(),
        reason: "state mismatch".into(),
        created_at: 10,
        resolved: false,
        resolution: None,
    });

    service.create_project(project).unwrap();
    assert_eq!(service.list_projects().unwrap().len(), 1);
    let recall = service
        .recall_project("project-1", NovelTaskType::Body)
        .unwrap();
    assert_eq!(recall.project_id, "project-1");
    let consistency = service.check_consistency("project-1").unwrap();
    assert!(!consistency.issues.is_empty());

    let resolved = service
        .resolve_conflict("project-1", "conflict-1", "keep the published state")
        .await
        .unwrap();
    assert!(resolved.conflicts[0].resolved);
    assert_eq!(
        resolved.conflicts[0].resolution.as_deref(),
        Some("keep the published state")
    );
    assert!(service
        .check_consistency("project-1")
        .unwrap()
        .issues
        .is_empty());
    assert_eq!(store.pending_outbox(10).unwrap().len(), 2);
}
