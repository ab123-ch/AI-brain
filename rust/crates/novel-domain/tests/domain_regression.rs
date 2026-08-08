use std::path::PathBuf;

use novel_domain::{
    build_recall_pack, check_consistency, CanonStatus, ClarificationRequest, CommitReport,
    ConflictRecord, ContextRef, ContextRole, MainReviewChecks, MainReviewRecord, MainReviewVerdict,
    NovelArtifactReceipt, NovelDraftEnvelope, NovelFact, NovelFactKind, NovelMemoryDelta,
    NovelOutcome, NovelProject, NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict,
    NovelTaskPhase, NovelTaskRequest, NovelTaskState, NovelTaskType, NovelTransition,
    PublicationPolicy, ReviewCheckStatus, ReviewIssue, UserDecision, UserDecisionRecord,
};

fn fact(id: &str, kind: NovelFactKind, subject_key: &str) -> NovelFact {
    NovelFact {
        fact_id: id.into(),
        kind,
        subject_key: subject_key.into(),
        title: id.into(),
        summary: id.into(),
        data: serde_json::Value::Null,
        status: CanonStatus::Confirmed,
        branch_id: "main".into(),
        valid_from_chapter: None,
        valid_to_chapter: None,
        source_refs: Vec::new(),
        confidence: 1.0,
        revision: 1,
        created_at: 1,
        updated_at: 1,
    }
}

fn request(policy: PublicationPolicy) -> NovelTaskRequest {
    NovelTaskRequest {
        task_id: "task-1".into(),
        project_id: "project-1".into(),
        task_type: NovelTaskType::Body,
        task_brief: "Write chapter one".into(),
        target_chapter: Some(1),
        expected_revision: 0,
        output_path: PathBuf::from("chapters/0001.md"),
        context_refs: Vec::new(),
        must_happen: Vec::new(),
        must_not_change: Vec::new(),
        acceptance_criteria: vec!["Complete chapter one".into()],
        allow_web_research: false,
        publication_policy: policy,
        parent_task_id: None,
        source_conversation_id: None,
        source_generation_id: None,
    }
}

fn draft(version: u32) -> NovelDraftEnvelope {
    NovelDraftEnvelope {
        task_id: "task-1".into(),
        draft_version: version,
        project_id: "project-1".into(),
        canon_revision: 0,
        content: format!("Chapter one draft {version}"),
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
            summary: "Self review passed".into(),
        },
        proposed_delta: NovelMemoryDelta {
            project_id: "project-1".into(),
            branch_id: "main".into(),
            expected_revision: 0,
            task_type: NovelTaskType::Body,
            source_ref: "chapters/0001.md".into(),
            progress: None,
            proposed_facts: Vec::new(),
            state_changes: Vec::new(),
            plot_updates: Vec::new(),
            foreshadowing_updates: Vec::new(),
            feedback: Vec::new(),
            experience_candidates: Vec::new(),
        },
        evidence_refs: vec!["task:requirements".into()],
    }
}

fn review(version: u32, verdict: MainReviewVerdict) -> MainReviewRecord {
    let mut checks = MainReviewChecks {
        user_requirements: ReviewCheckStatus::Pass,
        outline_alignment: ReviewCheckStatus::Pass,
        canon_consistency: ReviewCheckStatus::Pass,
        character_consistency: ReviewCheckStatus::Pass,
        timeline_consistency: ReviewCheckStatus::Pass,
        plot_and_foreshadowing: ReviewCheckStatus::Pass,
        style_quality: ReviewCheckStatus::Pass,
        pacing_and_hook: ReviewCheckStatus::Pass,
    };
    let issues = if verdict == MainReviewVerdict::Revise {
        checks.timeline_consistency = ReviewCheckStatus::Fail;
        vec![ReviewIssue::Message("Repair the timeline".into())]
    } else {
        Vec::new()
    };
    MainReviewRecord {
        task_id: "task-1".into(),
        draft_version: version,
        reviewed_canon_revision: 0,
        verdict,
        checks,
        issues,
        evidence_refs: vec!["requirements".into(), "outline".into(), "canon".into()],
        summary: "Review complete".into(),
    }
}

fn drafted_state(policy: PublicationPolicy, version: u32) -> NovelTaskState {
    let mut state = NovelTaskState::new(request(policy)).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::DraftReady(draft(version)))
        .unwrap();
    state
}

#[test]
fn consistency_reports_only_active_unresolved_and_overlapping_canon() {
    let mut project = NovelProject::new("project-1", "Project One");
    project.current_chapter = Some(20);
    project.conflicts.push(ConflictRecord {
        conflict_id: "conflict-open".into(),
        subject_key: "character:aria:location".into(),
        existing_fact_id: "location-a".into(),
        proposed_fact_id: "location-b".into(),
        reason: "Two locations".into(),
        created_at: 1,
        resolved: false,
        resolution: None,
    });
    project.conflicts.push(ConflictRecord {
        conflict_id: "conflict-closed".into(),
        subject_key: "character:aria:name".into(),
        existing_fact_id: "name-a".into(),
        proposed_fact_id: "name-b".into(),
        reason: "Resolved spelling".into(),
        created_at: 1,
        resolved: true,
        resolution: Some("Keep name-a".into()),
    });

    let mut first = fact(
        "location-a",
        NovelFactKind::CharacterState,
        "character:aria:location",
    );
    first.summary = "Aria is at the harbor".into();
    first.valid_from_chapter = Some(1);
    first.valid_to_chapter = Some(10);
    let mut second = fact(
        "location-b",
        NovelFactKind::CharacterState,
        "character:aria:location",
    );
    second.summary = "Aria is at the palace".into();
    second.valid_from_chapter = Some(10);
    second.valid_to_chapter = Some(30);
    let mut alternate = second.clone();
    alternate.fact_id = "location-alternate".into();
    alternate.branch_id = "alternate".into();
    alternate.summary = "Aria is elsewhere".into();
    let mut foreshadowing = fact(
        "foreshadow-key",
        NovelFactKind::Foreshadowing,
        "foreshadow:key",
    );
    foreshadowing.data = serde_json::json!({"target_chapter": 10});
    project.facts = vec![first, second, alternate, foreshadowing];

    let report = check_consistency(&project);
    let codes = report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        codes,
        vec![
            "unresolved_canon_conflict",
            "overlapping_canon",
            "overdue_foreshadowing"
        ]
    );
    assert!(report.has_errors());
}

#[test]
fn recall_filters_by_branch_status_chapter_and_task_type() {
    let mut project = NovelProject::new("project-1", "Project One");
    project.current_chapter = Some(5);

    let character = fact("character", NovelFactKind::Character, "character:aria");
    let event = fact("event", NovelFactKind::Event, "event:gate");
    let feedback = fact("feedback", NovelFactKind::Feedback, "feedback:pacing");
    let mut draft_fact = fact("draft", NovelFactKind::Character, "character:draft");
    draft_fact.status = CanonStatus::Draft;
    let mut alternate = fact("alternate", NovelFactKind::Character, "character:alternate");
    alternate.branch_id = "alternate".into();
    let mut expired = fact("expired", NovelFactKind::Character, "character:expired");
    expired.valid_to_chapter = Some(4);
    let mut future = fact("future", NovelFactKind::Character, "character:future");
    future.valid_from_chapter = Some(6);
    project.facts = vec![
        character, event, feedback, draft_fact, alternate, expired, future,
    ];

    let outline = build_recall_pack(&project, NovelTaskType::Outline);
    assert_eq!(
        outline
            .facts
            .iter()
            .map(|item| item.fact_id.as_str())
            .collect::<Vec<_>>(),
        vec!["character", "feedback"]
    );

    let body = build_recall_pack(&project, NovelTaskType::Body);
    assert_eq!(
        body.facts
            .iter()
            .map(|item| item.fact_id.as_str())
            .collect::<Vec<_>>(),
        vec!["character", "event"]
    );
    assert!(body.rendered_context.contains("Project One"));
    assert!(!body.rendered_context.contains("feedback"));
}

#[test]
fn recall_orders_newest_first_and_caps_the_pack_at_eighty_facts() {
    let mut project = NovelProject::new("project-1", "Project One");
    for chapter in 0_u32..85 {
        let mut item = fact(
            &format!("fact-{chapter}"),
            NovelFactKind::Character,
            &format!("character:{chapter}"),
        );
        item.valid_from_chapter = Some(chapter);
        item.revision = u64::from(chapter);
        project.facts.push(item);
    }

    let pack = build_recall_pack(&project, NovelTaskType::Body);
    assert_eq!(pack.facts.len(), 80);
    assert_eq!(pack.facts.first().unwrap().fact_id, "fact-84");
    assert_eq!(pack.facts.last().unwrap().fact_id, "fact-5");
}

#[test]
fn clarification_pauses_drafting_and_can_be_resumed() {
    let mut state = NovelTaskState::new(request(PublicationPolicy::RequireUserAcceptance)).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::NeedsClarification(ClarificationRequest {
            task_id: "task-1".into(),
            project_id: "project-1".into(),
            questions: vec!["Which point of view?".into()],
            reason: "The brief is ambiguous".into(),
        }))
        .unwrap();
    assert_eq!(state.phase, NovelTaskPhase::NeedsClarification);

    state.begin_drafting().unwrap();
    assert_eq!(state.phase, NovelTaskPhase::Drafting);
    assert!(state
        .apply_outcome(NovelOutcome::NeedsClarification(ClarificationRequest {
            task_id: "task-1".into(),
            project_id: "wrong-project".into(),
            questions: vec!["Which point of view?".into()],
            reason: "The brief is ambiguous".into(),
        }))
        .is_err());
    assert_eq!(state.phase, NovelTaskPhase::Drafting);
}

#[test]
fn clarification_refreshes_only_hashes_for_the_same_context_paths() {
    let mut task_request = request(PublicationPolicy::RequireUserAcceptance);
    task_request.context_refs = vec![ContextRef {
        role: ContextRole::ChapterOutline,
        canonical_path: PathBuf::from("outline.md"),
        sha256: "old-hash".into(),
        description: Some("第一章章纲".into()),
    }];
    let mut state = NovelTaskState::new(task_request).unwrap();
    state.begin_drafting().unwrap();
    state
        .apply_outcome(NovelOutcome::NeedsClarification(ClarificationRequest {
            task_id: "task-1".into(),
            project_id: "project-1".into(),
            questions: vec!["是否接受更新后的章纲？".into()],
            reason: "章纲已变化".into(),
        }))
        .unwrap();
    let refreshed = ContextRef {
        role: ContextRole::ChapterOutline,
        canonical_path: PathBuf::from("outline.md"),
        sha256: "new-hash".into(),
        description: Some("第一章章纲".into()),
    };

    state.refresh_context_refs(vec![refreshed.clone()]).unwrap();

    assert_eq!(state.phase, NovelTaskPhase::NeedsClarification);
    assert_eq!(state.request.context_refs, vec![refreshed]);

    let mut wrong_path = state.request.context_refs.clone();
    wrong_path[0].canonical_path = PathBuf::from("other.md");
    assert!(state.refresh_context_refs(wrong_path).is_err());
    state.begin_drafting().unwrap();
    assert!(state
        .refresh_context_refs(state.request.context_refs.clone())
        .is_err());
}

#[test]
fn review_and_user_revision_paths_require_new_drafts_before_rejection() {
    let mut state = drafted_state(PublicationPolicy::RequireUserAcceptance, 1);
    let transition = state
        .record_main_review(review(1, MainReviewVerdict::Revise))
        .unwrap();
    assert!(matches!(transition, NovelTransition::DraftReady { .. }));
    assert_eq!(state.phase, NovelTaskPhase::Drafting);

    state
        .apply_outcome(NovelOutcome::DraftReady(draft(2)))
        .unwrap();
    state
        .record_main_review(review(2, MainReviewVerdict::Pass))
        .unwrap();
    state
        .record_user_decision(UserDecisionRecord {
            task_id: "task-1".into(),
            draft_version: 2,
            decision: UserDecision::Revise,
            feedback: Some("Strengthen the ending".into()),
            decided_at: 1,
        })
        .unwrap();
    assert_eq!(state.phase, NovelTaskPhase::Drafting);

    state
        .apply_outcome(NovelOutcome::DraftReady(draft(3)))
        .unwrap();
    state
        .record_main_review(review(3, MainReviewVerdict::Pass))
        .unwrap();
    let transition = state
        .record_user_decision(UserDecisionRecord {
            task_id: "task-1".into(),
            draft_version: 3,
            decision: UserDecision::Reject,
            feedback: None,
            decided_at: 2,
        })
        .unwrap();
    assert!(matches!(transition, NovelTransition::Rejected { .. }));
    assert_eq!(state.phase, NovelTaskPhase::Rejected);
    assert!(state.ensure_publishable(3).is_err());
    assert!(state.cancel_for_conversation_fork().is_err());
}

#[test]
fn automatic_approval_tracks_publication_recovery_states_and_cancellation_guards() {
    let mut state = drafted_state(PublicationPolicy::AutoAfterMainReview, 1);
    let transition = state
        .record_main_review(review(1, MainReviewVerdict::Pass))
        .unwrap();
    assert!(matches!(
        transition,
        NovelTransition::ApprovedForPublication { .. }
    ));
    assert!(state.ensure_publishable(1).is_ok());

    state.mark_publication_pending("publication-1".into());
    assert_eq!(state.phase, NovelTaskPhase::PublicationPending);
    assert!(state.cancel_for_conversation_fork().is_err());

    state.restore_approved();
    assert_eq!(state.phase, NovelTaskPhase::ApprovedForPublication);
    assert!(state.publication_id.is_none());
    state.mark_publication_pending("publication-1".into());
    state.mark_artifact_saved(NovelArtifactReceipt {
        canonical_path: "chapters/0001.md".into(),
        sha256: "content-hash".into(),
        bytes: 128,
        written_at: 1,
    });
    assert_eq!(state.phase, NovelTaskPhase::ArtifactSavedMemoryPending);
    assert!(state.cancel_for_conversation_fork().is_err());

    state.mark_completed(CommitReport {
        project_id: "project-1".into(),
        previous_revision: 0,
        new_revision: 1,
        accepted_fact_ids: Vec::new(),
        conflicts: Vec::new(),
        graph_mirrored: false,
    });
    assert_eq!(state.phase, NovelTaskPhase::Completed);
    assert!(state.cancel_for_conversation_fork().is_err());

    let mut cancellable =
        NovelTaskState::new(request(PublicationPolicy::RequireUserAcceptance)).unwrap();
    cancellable.cancel_for_conversation_fork().unwrap();
    assert_eq!(cancellable.phase, NovelTaskPhase::Cancelled);
}

#[test]
fn legacy_recoverable_error_phase_migrates_to_failed() {
    let phase: NovelTaskPhase = serde_json::from_str("\"recoverable_error\"").unwrap();

    assert_eq!(phase, NovelTaskPhase::Failed);
}
