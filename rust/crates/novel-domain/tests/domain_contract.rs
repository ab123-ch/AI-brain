use std::path::PathBuf;

use novel_domain::{
    apply_canon_delta, sha256_hex, CandidateReview, CandidateReviewVerdict, CanonStatus,
    CommitReport, MainReviewChecks, MainReviewRecord, MainReviewVerdict, NovelArtifactReceipt,
    NovelDomainError, NovelDraftEnvelope, NovelFactKind, NovelMemoryDelta, NovelOutcome,
    NovelProject, NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict, NovelTaskPhase,
    NovelTaskRequest, NovelTaskState, NovelTaskType, NovelTransition, ProposedFact,
    PublicationPolicy, ReviewCheckStatus, UserDecision, UserDecisionRecord,
};

fn request() -> NovelTaskRequest {
    NovelTaskRequest {
        task_id: "task-1".into(),
        project_id: "project-1".into(),
        task_type: NovelTaskType::Body,
        task_brief: "Write chapter one".into(),
        target_chapter: Some(1),
        expected_revision: 0,
        output_path: PathBuf::from("chapters/0001.md"),
        context_refs: Vec::new(),
        must_happen: vec!["Open the locked gate".into()],
        must_not_change: vec!["The protagonist identity".into()],
        acceptance_criteria: vec!["Complete chapter one".into()],
        allow_web_research: false,
        publication_policy: PublicationPolicy::RequireUserAcceptance,
        parent_task_id: None,
        source_conversation_id: Some("room-1".into()),
        source_generation_id: Some("generation-1".into()),
    }
}

fn delta() -> NovelMemoryDelta {
    NovelMemoryDelta {
        project_id: "project-1".into(),
        branch_id: "main".into(),
        expected_revision: 0,
        task_type: NovelTaskType::Body,
        source_ref: "chapters/0001.md".into(),
        progress: None,
        proposed_facts: vec![ProposedFact {
            fact_id: "fact-1".into(),
            kind: NovelFactKind::Character,
            subject_key: "character:aria".into(),
            title: "Aria".into(),
            summary: "Aria opened the locked gate".into(),
            data: serde_json::json!({"role": "protagonist"}),
            status: CanonStatus::Confirmed,
            valid_from_chapter: Some(1),
            valid_to_chapter: None,
            source_refs: vec!["artifact:writer-1".into()],
            confidence: 0.95,
        }],
        state_changes: Vec::new(),
        plot_updates: Vec::new(),
        foreshadowing_updates: Vec::new(),
        feedback: Vec::new(),
        experience_candidates: Vec::new(),
    }
}

fn draft(version: u32) -> NovelDraftEnvelope {
    NovelDraftEnvelope {
        task_id: "task-1".into(),
        draft_version: version,
        project_id: "project-1".into(),
        canon_revision: 0,
        content: "Chapter one content".into(),
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
            summary: "All checks passed".into(),
        },
        proposed_delta: delta(),
        evidence_refs: vec!["task:requirements".into(), "canon:revision:0".into()],
    }
}

fn main_review() -> MainReviewRecord {
    MainReviewRecord {
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
        evidence_refs: vec!["requirements".into(), "outline".into(), "canon".into()],
        summary: "Review passed".into(),
    }
}

#[test]
fn candidate_hash_and_review_are_bound_to_one_sealed_artifact() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::DraftReady(draft(1)))
        .unwrap();

    let content_hash = sha256_hex(b"Chapter one content");
    let candidate = state
        .seal_candidate("artifact-writer-1", &content_hash)
        .unwrap();
    assert_eq!(candidate.content_hash, content_hash);
    assert_eq!(candidate.artifact_id, "artifact-writer-1");

    let stale = CandidateReview {
        review_id: "review-1".into(),
        candidate_id: candidate.candidate_id.clone(),
        candidate_content_hash: sha256_hex(b"changed content"),
        reviewer_artifact_id: "artifact-review-1".into(),
        verdict: CandidateReviewVerdict::Approve,
        evidence_refs: vec!["canon:revision:0".into()],
        summary: "Looks good".into(),
    };
    assert!(candidate.validate_review(&stale).is_err());

    let accepted = CandidateReview {
        candidate_content_hash: candidate.content_hash.clone(),
        ..stale
    };
    candidate.validate_review(&accepted).unwrap();
}

#[test]
fn default_publication_requires_review_user_acceptance_and_exact_candidate() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::DraftReady(draft(1)))
        .unwrap();
    let hash = sha256_hex(b"Chapter one content");
    state.seal_candidate("artifact-writer-1", &hash).unwrap();
    assert!(state.ensure_publishable(1).is_err());

    let transition = state.record_main_review(main_review()).unwrap();
    assert!(matches!(
        transition,
        NovelTransition::AwaitingUserDecision { .. }
    ));
    state
        .record_user_decision(UserDecisionRecord {
            task_id: "task-1".into(),
            draft_version: 1,
            decision: UserDecision::Accept,
            feedback: None,
            decided_at: 1,
        })
        .unwrap();
    assert!(state.ensure_publishable(1).is_ok());
    assert!(state.ensure_publishable(2).is_err());
}

#[test]
fn canon_delta_is_revision_guarded_and_publication_idempotent() {
    let project = NovelProject::new("project-1", "Project One");
    let first = apply_canon_delta(&project, &delta(), Some("publication-1"), 10).unwrap();
    assert_eq!(first.project.canon_revision, 1);
    assert_eq!(first.report.accepted_fact_ids, vec!["fact-1"]);
    assert_eq!(first.project.applied_publications, vec!["publication-1"]);

    let retry = apply_canon_delta(&first.project, &delta(), Some("publication-1"), 20).unwrap();
    assert_eq!(retry.project.canon_revision, 1);
    assert!(!retry.should_persist);

    let stale = apply_canon_delta(&first.project, &delta(), Some("publication-2"), 30);
    assert!(stale.is_err());
}

#[test]
fn checkpoint_roundtrip_preserves_domain_state_shape() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::DraftReady(draft(1)))
        .unwrap();
    let checkpoint = state.checkpoint().unwrap();
    let restored = NovelTaskState::from_checkpoint(&checkpoint).unwrap();
    assert_eq!(restored.request.task_id, state.request.task_id);
    assert_eq!(restored.phase, state.phase);
    assert_eq!(restored.draft_version, state.draft_version);
}

#[test]
fn failed_execution_unlock_cancels_blank_drafting_task() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    let mut before = state.checkpoint().unwrap();

    state.unlock_failed_execution().unwrap();

    assert_eq!(state.phase, NovelTaskPhase::Cancelled);
    assert!(state.phase.is_terminal());
    let mut after = state.checkpoint().unwrap();
    for checkpoint in [&mut before, &mut after] {
        let state = checkpoint.state.as_object_mut().unwrap();
        state.remove("phase");
        state.remove("updated_at");
    }
    before.phase = NovelTaskPhase::Cancelled;
    before.updated_at = after.updated_at;
    assert_eq!(after, before);
}

#[test]
fn failed_execution_unlock_applies_fail_closed_phase_allowlist() {
    enum Expected {
        Allowed,
        Rejected(&'static str),
    }

    let cases = [
        (NovelTaskPhase::Preparing, Expected::Allowed),
        (NovelTaskPhase::Drafting, Expected::Allowed),
        (
            NovelTaskPhase::SelfReview,
            Expected::Rejected("任务当前阶段不允许失败解锁"),
        ),
        (NovelTaskPhase::NeedsClarification, Expected::Allowed),
        (
            NovelTaskPhase::AwaitingMainReview,
            Expected::Rejected("任务当前阶段不允许失败解锁"),
        ),
        (
            NovelTaskPhase::AwaitingUserDecision,
            Expected::Rejected("任务当前阶段不允许失败解锁"),
        ),
        (
            NovelTaskPhase::ApprovedForPublication,
            Expected::Rejected("任务当前阶段不允许失败解锁"),
        ),
        (
            NovelTaskPhase::PublicationPending,
            Expected::Rejected("任务处于发布流程或已有发布产物，拒绝解锁"),
        ),
        (
            NovelTaskPhase::ArtifactSavedMemoryPending,
            Expected::Rejected("任务处于发布流程或已有发布产物，拒绝解锁"),
        ),
        (
            NovelTaskPhase::Completed,
            Expected::Rejected("任务已是终态，不能执行失败解锁"),
        ),
        (
            NovelTaskPhase::Rejected,
            Expected::Rejected("任务已是终态，不能执行失败解锁"),
        ),
        (
            NovelTaskPhase::Cancelled,
            Expected::Rejected("任务已是终态，不能执行失败解锁"),
        ),
        (
            NovelTaskPhase::Failed,
            Expected::Rejected("任务已是终态，不能执行失败解锁"),
        ),
        (NovelTaskPhase::StaleRevision, Expected::Allowed),
    ];

    for (phase, expected) in cases {
        let mut state = NovelTaskState::new(request()).unwrap();
        state.phase = phase;
        let checkpoint = state.checkpoint().unwrap();

        match expected {
            Expected::Allowed => {
                state.unlock_failed_execution().unwrap();
                assert_eq!(state.phase, NovelTaskPhase::Cancelled, "phase={phase:?}");
            }
            Expected::Rejected(expected_message) => {
                let error = state.unlock_failed_execution().unwrap_err();
                let NovelDomainError::InvalidTransition(actual_message) = error else {
                    panic!("phase={phase:?} returned an unexpected error: {error}");
                };
                assert_eq!(actual_message, expected_message, "phase={phase:?}");
                assert_eq!(state.checkpoint().unwrap(), checkpoint, "phase={phase:?}");
            }
        }
    }
}

#[test]
fn failed_execution_unlock_prioritizes_publication_guard_without_mutation() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state.publication_id = Some("publication-1".into());
    state.draft_version = 1;
    let checkpoint = state.checkpoint().unwrap();

    let error = state.unlock_failed_execution().unwrap_err();

    assert!(error.to_string().contains("发布流程或已有发布产物"));
    assert_eq!(state.checkpoint().unwrap(), checkpoint);
}

fn assert_publication_guard(mut state: NovelTaskState) {
    let checkpoint = state.checkpoint().unwrap();

    let error = state.unlock_failed_execution().unwrap_err();

    assert!(error.to_string().contains("发布流程或已有发布产物"));
    assert_eq!(state.checkpoint().unwrap(), checkpoint);
}

#[test]
fn failed_execution_unlock_rejects_publication_pending_without_mutation() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state.phase = NovelTaskPhase::PublicationPending;

    assert_publication_guard(state);
}

#[test]
fn failed_execution_unlock_rejects_artifact_saved_pending_without_mutation() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state.phase = NovelTaskPhase::ArtifactSavedMemoryPending;

    assert_publication_guard(state);
}

#[test]
fn failed_execution_unlock_rejects_publication_id_without_mutation() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state.publication_id = Some("publication-1".into());

    assert_publication_guard(state);
}

#[test]
fn failed_execution_unlock_rejects_artifact_without_mutation() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state.artifact = Some(NovelArtifactReceipt {
        canonical_path: "chapters/0001.md".into(),
        sha256: "hash-1".into(),
        bytes: 1,
        written_at: 1,
    });

    assert_publication_guard(state);
}

#[test]
fn failed_execution_unlock_rejects_commit_report_without_mutation() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state.commit_report = Some(CommitReport {
        project_id: "project-1".into(),
        previous_revision: 0,
        new_revision: 1,
        accepted_fact_ids: Vec::new(),
        conflicts: Vec::new(),
        graph_mirrored: true,
    });

    assert_publication_guard(state);
}

#[test]
fn failed_execution_unlock_rejects_draft_history_without_mutation() {
    let mut state = NovelTaskState::new(request()).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::DraftReady(draft(1)))
        .unwrap();
    let checkpoint = state.checkpoint().unwrap();

    let error = state.unlock_failed_execution().unwrap_err();

    assert!(error.to_string().contains("已有草稿、候选、评审或用户决定"));
    assert_eq!(state.checkpoint().unwrap(), checkpoint);
}
