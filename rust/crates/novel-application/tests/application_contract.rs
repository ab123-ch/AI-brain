use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use novel_application::{NovelApplicationService, NovelDomainStore, NovelResourcePort};
use novel_domain::{
    CanonStatus, ConflictRecord, MainReviewChecks, MainReviewRecord, MainReviewVerdict,
    NovelArtifactReceipt, NovelDraftEnvelope, NovelFactKind, NovelMemoryDelta, NovelOutcome,
    NovelProject, NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict, NovelTaskRequest,
    NovelTaskState, NovelTaskType, ProposedFact, PublicationPolicy, ReviewCheckStatus,
    UserDecision, UserDecisionRecord,
};

struct TestResources {
    root: PathBuf,
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
