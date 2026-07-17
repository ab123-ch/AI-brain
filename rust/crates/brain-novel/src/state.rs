use brain_memory::novel::{
    CommitReport, NovelArtifactReceipt, NovelTaskCheckpoint, NovelTaskPhase,
};
use serde::{Deserialize, Serialize};

use crate::review::{validate_main_review, validate_self_review};
use crate::{
    MainReviewRecord, MainReviewVerdict, NovelBrainError, NovelConversationSource,
    NovelDraftEnvelope, NovelOutcome, NovelTaskRequest, NovelTransition, PublicationPolicy, Result,
    UserDecision, UserDecisionRecord,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelTaskState {
    pub request: NovelTaskRequest,
    pub phase: NovelTaskPhase,
    pub draft_version: u32,
    #[serde(default)]
    pub draft: Option<NovelDraftEnvelope>,
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
        let now = crate::types::now_millis();
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
            return Err(NovelBrainError::InvalidRequest(format!(
                "checkpoint {} 与持久化任务状态不一致",
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
            return Err(NovelBrainError::InvalidTransition(format!(
                "任务 {} 当前为 {:?}，不能开始创作",
                self.request.task_id, self.phase
            )));
        }
        self.phase = NovelTaskPhase::Drafting;
        self.updated_at = crate::types::now_millis();
        Ok(())
    }

    pub fn apply_outcome(&mut self, outcome: NovelOutcome) -> Result<NovelOutcome> {
        if self.phase != NovelTaskPhase::Drafting {
            return Err(NovelBrainError::InvalidTransition(format!(
                "任务 {} 当前为 {:?}，不能接收模型产物",
                self.request.task_id, self.phase
            )));
        }
        self.updated_at = crate::types::now_millis();
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
                    return Err(NovelBrainError::InvalidModelOutput(
                        "澄清请求缺少匹配的任务、项目或有效问题".into(),
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
                    return Err(NovelBrainError::InvalidModelOutput(
                        "草稿任务、项目、revision、正文或 Delta 与任务合同不一致".into(),
                    ));
                }
                let expected_version = self.draft_version + 1;
                if draft.draft_version != expected_version {
                    return Err(NovelBrainError::InvalidModelOutput(format!(
                        "草稿版本无效: expected={expected_version}, actual={}",
                        draft.draft_version
                    )));
                }
                self.draft_version = draft.draft_version;
                self.draft = Some(draft.clone());
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

    pub fn record_main_review(&mut self, review: MainReviewRecord) -> Result<NovelTransition> {
        if self.phase != NovelTaskPhase::AwaitingMainReview {
            return Err(NovelBrainError::InvalidTransition(format!(
                "任务 {} 当前为 {:?}，不能提交主脑复审",
                self.request.task_id, self.phase
            )));
        }
        let draft = self
            .draft
            .as_ref()
            .ok_or_else(|| NovelBrainError::InvalidTransition("任务没有可复审草稿".into()))?;
        validate_main_review(&review, draft)?;
        self.main_review = Some(review.clone());
        self.updated_at = crate::types::now_millis();

        match review.verdict {
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
            return Err(NovelBrainError::InvalidTransition(format!(
                "任务 {} 当前为 {:?}，不能记录用户决策",
                self.request.task_id, self.phase
            )));
        }
        if decision.task_id != self.request.task_id || decision.draft_version != self.draft_version
        {
            return Err(NovelBrainError::InvalidTransition(
                "用户决策对应的 task_id 或 draft_version 已过期".into(),
            ));
        }
        if decision.decision == UserDecision::Revise
            && decision
                .feedback
                .as_deref()
                .is_none_or(|feedback| feedback.trim().is_empty())
        {
            return Err(NovelBrainError::InvalidTransition(
                "用户选择 revise 时必须提供具体反馈".into(),
            ));
        }
        let draft = self
            .draft
            .clone()
            .ok_or_else(|| NovelBrainError::InvalidTransition("任务没有候选草稿".into()))?;
        self.user_decision = Some(decision.clone());
        self.updated_at = crate::types::now_millis();

        match decision.decision {
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
            return Err(NovelBrainError::InvalidTransition(format!(
                "任务 {} 当前为 {:?}，尚未获准发布",
                self.request.task_id, self.phase
            )));
        }
        if draft_version != self.draft_version {
            return Err(NovelBrainError::InvalidTransition(format!(
                "发布草稿版本已过期: expected={}, actual={draft_version}",
                self.draft_version
            )));
        }
        if self.request.publication_policy == PublicationPolicy::RequireUserAcceptance
            && self
                .user_decision
                .as_ref()
                .is_none_or(|record| record.decision != UserDecision::Accept)
        {
            return Err(NovelBrainError::InvalidTransition(
                "默认发布策略缺少用户 Accept 记录".into(),
            ));
        }
        self.draft
            .as_ref()
            .ok_or_else(|| NovelBrainError::InvalidTransition("任务没有已审核草稿".into()))
    }

    pub fn mark_publication_pending(&mut self, publication_id: String) {
        self.publication_id = Some(publication_id);
        self.artifact = None;
        self.commit_report = None;
        self.phase = NovelTaskPhase::PublicationPending;
        self.updated_at = crate::types::now_millis();
    }

    pub(crate) fn restore_approved(&mut self) {
        self.publication_id = None;
        self.artifact = None;
        self.commit_report = None;
        self.phase = NovelTaskPhase::ApprovedForPublication;
        self.updated_at = crate::types::now_millis();
    }

    pub fn mark_artifact_saved(&mut self, artifact: NovelArtifactReceipt) {
        self.artifact = Some(artifact);
        self.phase = NovelTaskPhase::ArtifactSavedMemoryPending;
        self.updated_at = crate::types::now_millis();
    }

    pub fn mark_completed(&mut self, report: CommitReport) {
        self.commit_report = Some(report);
        self.phase = NovelTaskPhase::Completed;
        self.updated_at = crate::types::now_millis();
    }

    pub fn mark_failed(&mut self) {
        self.phase = NovelTaskPhase::Failed;
        self.updated_at = crate::types::now_millis();
    }

    pub fn cancel_for_conversation_fork(&mut self) -> Result<()> {
        if self.phase.is_terminal() {
            return Err(NovelBrainError::InvalidTransition(format!(
                "任务 {} 已处于终态 {:?}，不能按会话分叉取消",
                self.request.task_id, self.phase
            )));
        }
        if matches!(
            self.phase,
            NovelTaskPhase::PublicationPending | NovelTaskPhase::ArtifactSavedMemoryPending
        ) {
            return Err(NovelBrainError::InvalidTransition(format!(
                "任务 {} 正处于发布事务中，不能按会话分叉取消",
                self.request.task_id
            )));
        }
        self.phase = NovelTaskPhase::Cancelled;
        self.updated_at = crate::types::now_millis();
        Ok(())
    }

    pub fn associate_conversation_source(&mut self, source: NovelConversationSource) {
        if !self.conversation_sources.contains(&source) {
            self.conversation_sources.push(source);
            self.updated_at = crate::types::now_millis();
        }
    }

    pub fn matches_conversation_generations(
        &self,
        conversation_id: &str,
        generations: &std::collections::HashSet<&str>,
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
    for (field, value) in [
        ("task_id", request.task_id.as_str()),
        ("project_id", request.project_id.as_str()),
    ] {
        if value.is_empty()
            || !value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err(NovelBrainError::InvalidRequest(format!(
                "{field} 只能包含字母、数字、-、_"
            )));
        }
    }
    if request.source_conversation_id.is_some() != request.source_generation_id.is_some() {
        return Err(NovelBrainError::InvalidRequest(
            "source_conversation_id 与 source_generation_id 必须同时提供".into(),
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
        if value.is_some_and(|value| {
            value.is_empty()
                || !value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        }) {
            return Err(NovelBrainError::InvalidRequest(format!(
                "{field} 只能包含字母、数字、-、_"
            )));
        }
    }
    if request.task_brief.trim().is_empty() {
        return Err(NovelBrainError::InvalidRequest(
            "task_brief 不能为空".into(),
        ));
    }
    if request.output_path.as_os_str().is_empty() {
        return Err(NovelBrainError::InvalidRequest(
            "output_path 不能为空".into(),
        ));
    }
    if request.acceptance_criteria.is_empty()
        || request
            .acceptance_criteria
            .iter()
            .any(|item| item.trim().is_empty())
    {
        return Err(NovelBrainError::InvalidRequest(
            "acceptance_criteria 必须包含非空验收条件".into(),
        ));
    }
    if request
        .context_refs
        .iter()
        .any(|reference| reference.sha256.trim().is_empty())
    {
        return Err(NovelBrainError::InvalidRequest(
            "每个 ContextRef 都必须包含内容 sha256".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use brain_memory::novel::{NovelMemoryDelta, NovelTaskType};

    use super::*;
    use crate::{
        MainReviewChecks, NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict,
        ReviewCheckStatus,
    };

    fn request(policy: PublicationPolicy) -> NovelTaskRequest {
        NovelTaskRequest {
            task_id: "task-1".into(),
            project_id: "project-1".into(),
            task_type: NovelTaskType::Body,
            task_brief: "写第一章".into(),
            target_chapter: Some(1),
            expected_revision: 0,
            output_path: PathBuf::from("chapters/0001.md"),
            context_refs: Vec::new(),
            must_happen: Vec::new(),
            must_not_change: Vec::new(),
            acceptance_criteria: vec!["完成第一章".into()],
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
            content: "第一章正文".into(),
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
                summary: "自检通过".into(),
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
            evidence_refs: vec!["task:requirements".into(), "memory:canon".into()],
        }
    }

    fn pass_review(version: u32) -> MainReviewRecord {
        MainReviewRecord {
            task_id: "task-1".into(),
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

    #[test]
    fn default_policy_requires_main_review_and_user_acceptance() {
        let mut state =
            NovelTaskState::new(request(PublicationPolicy::RequireUserAcceptance)).unwrap();
        state.begin_drafting().unwrap();
        state
            .apply_outcome(NovelOutcome::DraftReady(draft(1)))
            .unwrap();
        assert!(state.ensure_publishable(1).is_err());

        let transition = state.record_main_review(pass_review(1)).unwrap();
        assert!(matches!(
            transition,
            NovelTransition::AwaitingUserDecision { .. }
        ));
        assert!(state.ensure_publishable(1).is_err());

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
    }

    #[test]
    fn explicit_auto_policy_can_publish_after_main_review() {
        let mut state =
            NovelTaskState::new(request(PublicationPolicy::AutoAfterMainReview)).unwrap();
        state.begin_drafting().unwrap();
        state
            .apply_outcome(NovelOutcome::DraftReady(draft(1)))
            .unwrap();
        let transition = state.record_main_review(pass_review(1)).unwrap();
        assert!(matches!(
            transition,
            NovelTransition::ApprovedForPublication { .. }
        ));
        assert!(state.ensure_publishable(1).is_ok());
    }

    #[test]
    fn stale_review_and_incomplete_self_review_are_rejected() {
        let mut state =
            NovelTaskState::new(request(PublicationPolicy::RequireUserAcceptance)).unwrap();
        state.begin_drafting().unwrap();
        let mut invalid = draft(1);
        invalid.self_review.checks.timeline_consistency = ReviewCheckStatus::Fail;
        assert!(state
            .apply_outcome(NovelOutcome::DraftReady(invalid))
            .is_err());

        let mut state =
            NovelTaskState::new(request(PublicationPolicy::RequireUserAcceptance)).unwrap();
        state.begin_drafting().unwrap();
        state
            .apply_outcome(NovelOutcome::DraftReady(draft(1)))
            .unwrap();
        let mut stale = pass_review(1);
        stale.draft_version = 2;
        assert!(state.record_main_review(stale).is_err());
    }

    #[test]
    fn invalid_user_revision_does_not_mutate_state() {
        let mut state =
            NovelTaskState::new(request(PublicationPolicy::RequireUserAcceptance)).unwrap();
        state.begin_drafting().unwrap();
        state
            .apply_outcome(NovelOutcome::DraftReady(draft(1)))
            .unwrap();
        state.record_main_review(pass_review(1)).unwrap();

        let error = state
            .record_user_decision(UserDecisionRecord {
                task_id: "task-1".into(),
                draft_version: 1,
                decision: UserDecision::Revise,
                feedback: Some("   ".into()),
                decided_at: 1,
            })
            .unwrap_err();

        assert!(error.to_string().contains("具体反馈"));
        assert_eq!(state.phase, NovelTaskPhase::AwaitingUserDecision);
        assert!(state.user_decision.is_none());
    }
}
