use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::model::{now_millis, require_non_empty, validate_identifier};
use crate::{
    sha256_hex, validate_main_review, validate_self_review, CandidateReview, CommitReport,
    MainReviewRecord, MainReviewVerdict, NovelArtifactReceipt, NovelCandidate,
    NovelConversationSource, NovelDomainError, NovelDraftEnvelope, NovelOutcome,
    NovelTaskCheckpoint, NovelTaskPhase, NovelTaskRequest, NovelTransition, PublicationPolicy,
    Result, UserDecision, UserDecisionRecord,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelTaskState {
    pub request: NovelTaskRequest,
    pub phase: NovelTaskPhase,
    pub draft_version: u32,
    #[serde(default)]
    pub draft: Option<NovelDraftEnvelope>,
    #[serde(default)]
    pub candidate: Option<NovelCandidate>,
    #[serde(default)]
    pub candidate_reviews: Vec<CandidateReview>,
    #[serde(default)]
    pub main_review: Option<MainReviewRecord>,
    #[serde(default)]
    pub user_decision: Option<UserDecisionRecord>,
    #[serde(default)]
    pub publication_id: Option<String>,
    #[serde(default)]
    pub artifact: Option<NovelArtifactReceipt>,
    #[serde(default)]
    pub commit_report: Option<CommitReport>,
    #[serde(default)]
    pub conversation_sources: Vec<NovelConversationSource>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl NovelTaskState {
    pub fn new(request: NovelTaskRequest) -> Result<Self> {
        validate_request(&request)?;
        let now = now_millis();
        let conversation_sources = match (
            request.source_conversation_id.clone(),
            request.source_generation_id.clone(),
        ) {
            (Some(conversation_id), Some(generation_id)) => vec![NovelConversationSource {
                conversation_id,
                generation_id,
            }],
            _ => Vec::new(),
        };
        Ok(Self {
            request,
            phase: NovelTaskPhase::Preparing,
            draft_version: 0,
            draft: None,
            candidate: None,
            candidate_reviews: Vec::new(),
            main_review: None,
            user_decision: None,
            publication_id: None,
            artifact: None,
            commit_report: None,
            conversation_sources,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn from_checkpoint(checkpoint: &NovelTaskCheckpoint) -> Result<Self> {
        let state: Self = serde_json::from_value(checkpoint.state.clone())?;
        if state.request.task_id != checkpoint.task_id
            || state.request.project_id != checkpoint.project_id
            || state.phase != checkpoint.phase
            || state.draft_version != checkpoint.draft_version
        {
            return Err(NovelDomainError::InvalidRequest(format!(
                "checkpoint {} does not match its embedded task state",
                checkpoint.task_id
            )));
        }
        Ok(state)
    }

    pub fn checkpoint(&self) -> Result<NovelTaskCheckpoint> {
        Ok(NovelTaskCheckpoint {
            task_id: self.request.task_id.clone(),
            project_id: self.request.project_id.clone(),
            phase: self.phase,
            draft_version: self.draft_version,
            canon_revision: self.request.expected_revision,
            output_path: self.request.output_path.to_string_lossy().into_owned(),
            state: serde_json::to_value(self)?,
            updated_at: self.updated_at,
        })
    }

    pub fn begin_drafting(&mut self) -> Result<()> {
        if !matches!(
            self.phase,
            NovelTaskPhase::Preparing
                | NovelTaskPhase::NeedsClarification
                | NovelTaskPhase::Drafting
        ) {
            return Err(NovelDomainError::InvalidTransition(format!(
                "task {} cannot draft from {:?}",
                self.request.task_id, self.phase
            )));
        }
        self.phase = NovelTaskPhase::Drafting;
        self.updated_at = now_millis();
        Ok(())
    }

    /// 在用户澄清后显式接受同一批上下文文件的新内容哈希。
    ///
    /// 这里只允许更新 hash；角色、路径和顺序仍由原任务合同冻结。
    pub fn refresh_context_refs(&mut self, refreshed: Vec<crate::ContextRef>) -> Result<()> {
        if self.phase != NovelTaskPhase::NeedsClarification {
            return Err(NovelDomainError::InvalidTransition(format!(
                "task {} can refresh context only from NeedsClarification",
                self.request.task_id
            )));
        }
        if refreshed.len() != self.request.context_refs.len() {
            return Err(NovelDomainError::InvalidRequest(
                "refreshed context must preserve the frozen reference count".into(),
            ));
        }
        for (existing, replacement) in self.request.context_refs.iter().zip(&refreshed) {
            if existing.role != replacement.role
                || existing.canonical_path != replacement.canonical_path
            {
                return Err(NovelDomainError::InvalidRequest(
                    "refreshed context may change only hashes, not roles or paths".into(),
                ));
            }
            if replacement.sha256.trim().is_empty() {
                return Err(NovelDomainError::InvalidRequest(
                    "every refreshed context reference requires a content hash".into(),
                ));
            }
        }
        for (existing, replacement) in self.request.context_refs.iter_mut().zip(refreshed) {
            existing.sha256 = replacement.sha256.trim().to_ascii_lowercase();
        }
        self.updated_at = now_millis();
        Ok(())
    }

    pub fn apply_outcome(&mut self, outcome: NovelOutcome) -> Result<NovelOutcome> {
        if self.phase != NovelTaskPhase::Drafting {
            return Err(NovelDomainError::InvalidTransition(format!(
                "task {} cannot accept model output from {:?}",
                self.request.task_id, self.phase
            )));
        }
        self.updated_at = now_millis();
        match outcome {
            NovelOutcome::NeedsClarification(clarification) => {
                if clarification.task_id != self.request.task_id
                    || clarification.project_id != self.request.project_id
                    || clarification.questions.is_empty()
                    || clarification
                        .questions
                        .iter()
                        .any(|item| item.trim().is_empty())
                {
                    return Err(NovelDomainError::InvalidCandidate(
                        "clarification request does not match the task or has empty questions"
                            .into(),
                    ));
                }
                self.phase = NovelTaskPhase::NeedsClarification;
                Ok(NovelOutcome::NeedsClarification(clarification))
            }
            NovelOutcome::DraftReady(draft) => {
                validate_self_review(&draft.self_review)?;
                if draft.task_id != self.request.task_id
                    || draft.project_id != self.request.project_id
                    || draft.canon_revision != self.request.expected_revision
                    || draft.proposed_delta.project_id != self.request.project_id
                    || draft.proposed_delta.expected_revision != self.request.expected_revision
                    || draft.proposed_delta.task_type != self.request.task_type
                    || draft.proposed_delta.source_ref != self.request.output_path.to_string_lossy()
                    || draft.content.trim().is_empty()
                {
                    return Err(NovelDomainError::InvalidCandidate(
                        "draft task, project, revision, content, or delta is inconsistent".into(),
                    ));
                }
                let expected_version = self.draft_version + 1;
                if draft.draft_version != expected_version {
                    return Err(NovelDomainError::InvalidCandidate(format!(
                        "draft version must be {expected_version}, got {}",
                        draft.draft_version
                    )));
                }
                self.draft_version = draft.draft_version;
                self.draft = Some(draft.clone());
                self.candidate = None;
                self.candidate_reviews.clear();
                self.main_review = None;
                self.user_decision = None;
                self.publication_id = None;
                self.artifact = None;
                self.commit_report = None;
                self.phase = NovelTaskPhase::AwaitingMainReview;
                Ok(NovelOutcome::DraftReady(draft))
            }
        }
    }

    pub fn seal_candidate(
        &mut self,
        artifact_id: impl Into<String>,
        content_hash: &str,
    ) -> Result<NovelCandidate> {
        let artifact_id = artifact_id.into();
        require_non_empty("artifact id", &artifact_id)?;
        require_non_empty("candidate content hash", content_hash)?;
        let draft = self
            .draft
            .as_ref()
            .ok_or_else(|| NovelDomainError::InvalidCandidate("task has no draft".into()))?;
        let actual_hash = sha256_hex(draft.content.as_bytes());
        if actual_hash != content_hash {
            return Err(NovelDomainError::InvalidCandidate(format!(
                "candidate hash mismatch: expected={actual_hash}, actual={content_hash}"
            )));
        }
        if let Some(existing) = &self.candidate {
            if existing.artifact_id == artifact_id && existing.content_hash == content_hash {
                return Ok(existing.clone());
            }
            return Err(NovelDomainError::InvalidCandidate(
                "draft version is already sealed to another artifact".into(),
            ));
        }
        let candidate = NovelCandidate {
            candidate_id: format!(
                "candidate-{}-{}-{}",
                self.request.task_id,
                self.draft_version,
                &content_hash[..content_hash.len().min(12)]
            ),
            task_id: self.request.task_id.clone(),
            project_id: self.request.project_id.clone(),
            draft_version: self.draft_version,
            canon_revision: self.request.expected_revision,
            artifact_id,
            content_hash: content_hash.into(),
            created_at: now_millis(),
        };
        self.candidate = Some(candidate.clone());
        self.updated_at = now_millis();
        Ok(candidate)
    }

    pub fn record_candidate_review(&mut self, review: CandidateReview) -> Result<()> {
        let candidate = self
            .candidate
            .as_ref()
            .ok_or_else(|| NovelDomainError::InvalidCandidate("task has no candidate".into()))?;
        candidate.validate_review(&review)?;
        if self
            .candidate_reviews
            .iter()
            .any(|existing| existing.review_id == review.review_id)
        {
            return Err(NovelDomainError::InvalidCandidate(format!(
                "duplicate candidate review: {}",
                review.review_id
            )));
        }
        self.candidate_reviews.push(review);
        self.updated_at = now_millis();
        Ok(())
    }

    pub fn record_main_review(&mut self, review: MainReviewRecord) -> Result<NovelTransition> {
        if self.phase != NovelTaskPhase::AwaitingMainReview {
            return Err(NovelDomainError::InvalidTransition(format!(
                "task {} cannot be reviewed from {:?}",
                self.request.task_id, self.phase
            )));
        }
        let draft = self
            .draft
            .as_ref()
            .ok_or_else(|| NovelDomainError::InvalidTransition("task has no draft".into()))?;
        validate_main_review(&review, draft)?;
        let verdict = review.verdict;
        self.main_review = Some(review);
        self.updated_at = now_millis();
        match verdict {
            MainReviewVerdict::Revise => {
                self.phase = NovelTaskPhase::Drafting;
                Ok(NovelTransition::DraftReady {
                    draft: draft.clone(),
                })
            }
            MainReviewVerdict::Pass => {
                if self.request.publication_policy == PublicationPolicy::AutoAfterMainReview {
                    self.phase = NovelTaskPhase::ApprovedForPublication;
                    Ok(NovelTransition::ApprovedForPublication {
                        task_id: self.request.task_id.clone(),
                        draft_version: self.draft_version,
                    })
                } else {
                    self.phase = NovelTaskPhase::AwaitingUserDecision;
                    Ok(NovelTransition::AwaitingUserDecision {
                        draft: draft.clone(),
                    })
                }
            }
        }
    }

    pub fn record_user_decision(
        &mut self,
        decision: UserDecisionRecord,
    ) -> Result<NovelTransition> {
        if self.phase != NovelTaskPhase::AwaitingUserDecision {
            return Err(NovelDomainError::InvalidTransition(format!(
                "task {} cannot record a user decision from {:?}",
                self.request.task_id, self.phase
            )));
        }
        if decision.task_id != self.request.task_id || decision.draft_version != self.draft_version
        {
            return Err(NovelDomainError::InvalidTransition(
                "user decision refers to a stale task or draft".into(),
            ));
        }
        if decision.decision == UserDecision::Revise
            && decision
                .feedback
                .as_deref()
                .is_none_or(|feedback| feedback.trim().is_empty())
        {
            return Err(NovelDomainError::InvalidTransition(
                "revision decision requires feedback".into(),
            ));
        }
        let draft = self
            .draft
            .clone()
            .ok_or_else(|| NovelDomainError::InvalidTransition("task has no draft".into()))?;
        let user_decision = decision.decision;
        self.user_decision = Some(decision);
        self.updated_at = now_millis();
        match user_decision {
            UserDecision::Accept => {
                self.phase = NovelTaskPhase::ApprovedForPublication;
                Ok(NovelTransition::ApprovedForPublication {
                    task_id: self.request.task_id.clone(),
                    draft_version: self.draft_version,
                })
            }
            UserDecision::Revise => {
                self.phase = NovelTaskPhase::Drafting;
                Ok(NovelTransition::DraftReady { draft })
            }
            UserDecision::Reject => {
                self.phase = NovelTaskPhase::Rejected;
                Ok(NovelTransition::Rejected {
                    task_id: self.request.task_id.clone(),
                    draft_version: self.draft_version,
                })
            }
        }
    }

    pub fn ensure_publishable(&self, draft_version: u32) -> Result<&NovelDraftEnvelope> {
        if self.phase != NovelTaskPhase::ApprovedForPublication {
            return Err(NovelDomainError::InvalidTransition(format!(
                "task {} is not approved for publication",
                self.request.task_id
            )));
        }
        if draft_version != self.draft_version {
            return Err(NovelDomainError::InvalidTransition(format!(
                "stale publication draft: expected={}, actual={draft_version}",
                self.draft_version
            )));
        }
        if self.request.publication_policy == PublicationPolicy::RequireUserAcceptance
            && self
                .user_decision
                .as_ref()
                .is_none_or(|record| record.decision != UserDecision::Accept)
        {
            return Err(NovelDomainError::InvalidTransition(
                "publication requires an explicit user acceptance".into(),
            ));
        }
        self.draft
            .as_ref()
            .ok_or_else(|| NovelDomainError::InvalidTransition("task has no draft".into()))
    }

    pub fn mark_publication_pending(&mut self, publication_id: String) {
        self.publication_id = Some(publication_id);
        self.artifact = None;
        self.commit_report = None;
        self.phase = NovelTaskPhase::PublicationPending;
        self.updated_at = now_millis();
    }

    pub fn restore_approved(&mut self) {
        self.publication_id = None;
        self.artifact = None;
        self.commit_report = None;
        self.phase = NovelTaskPhase::ApprovedForPublication;
        self.updated_at = now_millis();
    }

    pub fn mark_artifact_saved(&mut self, artifact: NovelArtifactReceipt) {
        self.artifact = Some(artifact);
        self.phase = NovelTaskPhase::ArtifactSavedMemoryPending;
        self.updated_at = now_millis();
    }

    pub fn mark_completed(&mut self, report: CommitReport) {
        self.commit_report = Some(report);
        self.phase = NovelTaskPhase::Completed;
        self.updated_at = now_millis();
    }

    pub fn mark_failed(&mut self) {
        self.phase = NovelTaskPhase::Failed;
        self.updated_at = now_millis();
    }

    pub fn cancel_for_conversation_fork(&mut self) -> Result<()> {
        if self.phase.is_terminal() {
            return Err(NovelDomainError::InvalidTransition(format!(
                "terminal task {} cannot be cancelled",
                self.request.task_id
            )));
        }
        if matches!(
            self.phase,
            NovelTaskPhase::PublicationPending | NovelTaskPhase::ArtifactSavedMemoryPending
        ) {
            return Err(NovelDomainError::InvalidTransition(format!(
                "publishing task {} cannot be fork-cancelled",
                self.request.task_id
            )));
        }
        self.phase = NovelTaskPhase::Cancelled;
        self.updated_at = now_millis();
        Ok(())
    }

    pub fn associate_conversation_source(&mut self, source: NovelConversationSource) {
        if !self.conversation_sources.contains(&source) {
            self.conversation_sources.push(source);
            self.updated_at = now_millis();
        }
    }

    pub fn matches_conversation_generations(
        &self,
        conversation_id: &str,
        generations: &HashSet<&str>,
        include_unscoped: bool,
    ) -> bool {
        if self.conversation_sources.is_empty() {
            return include_unscoped;
        }
        self.conversation_sources.iter().any(|source| {
            source.conversation_id == conversation_id
                && generations.contains(source.generation_id.as_str())
        })
    }
}

fn validate_request(request: &NovelTaskRequest) -> Result<()> {
    validate_identifier("task_id", &request.task_id)?;
    validate_identifier("project_id", &request.project_id)?;
    if request.source_conversation_id.is_some() != request.source_generation_id.is_some() {
        return Err(NovelDomainError::InvalidRequest(
            "conversation and generation provenance must be provided together".into(),
        ));
    }
    for (field, value) in [
        (
            "source_conversation_id",
            request.source_conversation_id.as_deref(),
        ),
        (
            "source_generation_id",
            request.source_generation_id.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            validate_identifier(field, value)?;
        }
    }
    require_non_empty("task brief", &request.task_brief)?;
    if request.output_path.as_os_str().is_empty() {
        return Err(NovelDomainError::InvalidRequest(
            "output path must not be empty".into(),
        ));
    }
    if request.acceptance_criteria.is_empty()
        || request
            .acceptance_criteria
            .iter()
            .any(|item| item.trim().is_empty())
    {
        return Err(NovelDomainError::InvalidRequest(
            "acceptance criteria must contain non-empty items".into(),
        ));
    }
    if request
        .context_refs
        .iter()
        .any(|reference| reference.sha256.trim().is_empty())
    {
        return Err(NovelDomainError::InvalidRequest(
            "every context reference requires a content hash".into(),
        ));
    }
    Ok(())
}
