use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{NovelDomainError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelProjectStatus {
    Planning,
    Writing,
    Paused,
    Completed,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonStatus {
    Draft,
    Confirmed,
    Rejected,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelFactKind {
    WorldRule,
    Character,
    CharacterState,
    Location,
    Organization,
    Item,
    Event,
    Timeline,
    PlotThread,
    Foreshadowing,
    Outline,
    ChapterPlan,
    ChapterSummary,
    Decision,
    Feedback,
    WritingExperience,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelTaskType {
    Outline,
    VolumeOutline,
    ChapterPlan,
    Body,
    Continuation,
    Review,
    Polish,
    Retrospective,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelFact {
    pub fact_id: String,
    pub kind: NovelFactKind,
    pub subject_key: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub data: Value,
    pub status: CanonStatus,
    #[serde(default = "default_branch")]
    pub branch_id: String,
    pub valid_from_chapter: Option<u32>,
    pub valid_to_chapter: Option<u32>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelProject {
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    pub target_platform: Option<String>,
    pub status: NovelProjectStatus,
    #[serde(default = "default_branch")]
    pub active_branch: String,
    pub current_volume: Option<String>,
    pub current_chapter: Option<u32>,
    #[serde(default)]
    pub canon_revision: u64,
    #[serde(default)]
    pub facts: Vec<NovelFact>,
    #[serde(default)]
    pub conflicts: Vec<ConflictRecord>,
    #[serde(default)]
    pub applied_publications: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl NovelProject {
    #[must_use]
    pub fn new(project_id: impl Into<String>, title: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            project_id: project_id.into(),
            title: title.into(),
            aliases: Vec::new(),
            genres: Vec::new(),
            target_platform: None,
            status: NovelProjectStatus::Planning,
            active_branch: default_branch(),
            current_volume: None,
            current_chapter: None,
            canon_revision: 0,
            facts: Vec::new(),
            conflicts: Vec::new(),
            applied_publications: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn validate(&self) -> Result<()> {
        validate_identifier("project_id", &self.project_id)?;
        require_non_empty("project title", &self.title)?;
        require_non_empty("active branch", &self.active_branch)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedFact {
    pub fact_id: String,
    pub kind: NovelFactKind,
    pub subject_key: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default = "default_canon_status")]
    pub status: CanonStatus,
    pub valid_from_chapter: Option<u32>,
    pub valid_to_chapter: Option<u32>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateChange {
    pub fact: ProposedFact,
    pub supersedes_fact_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlotUpdate {
    pub fact: ProposedFact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeshadowingUpdate {
    pub fact: ProposedFact,
    pub resolves_fact_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperienceCandidate {
    pub fact: ProposedFact,
    #[serde(default)]
    pub evidence_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelMemoryDelta {
    pub project_id: String,
    #[serde(default = "default_branch")]
    pub branch_id: String,
    pub expected_revision: u64,
    pub task_type: NovelTaskType,
    pub source_ref: String,
    #[serde(default)]
    pub progress: Option<NovelProjectProgress>,
    #[serde(default)]
    pub proposed_facts: Vec<ProposedFact>,
    #[serde(default)]
    pub state_changes: Vec<StateChange>,
    #[serde(default)]
    pub plot_updates: Vec<PlotUpdate>,
    #[serde(default)]
    pub foreshadowing_updates: Vec<ForeshadowingUpdate>,
    #[serde(default)]
    pub feedback: Vec<ProposedFact>,
    #[serde(default)]
    pub experience_candidates: Vec<ExperienceCandidate>,
}

impl NovelMemoryDelta {
    pub fn all_facts(&self) -> impl Iterator<Item = &ProposedFact> {
        self.proposed_facts
            .iter()
            .chain(self.state_changes.iter().map(|change| &change.fact))
            .chain(self.plot_updates.iter().map(|update| &update.fact))
            .chain(self.foreshadowing_updates.iter().map(|update| &update.fact))
            .chain(self.feedback.iter())
            .chain(self.experience_candidates.iter().map(|item| &item.fact))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub conflict_id: String,
    pub subject_key: String,
    pub existing_fact_id: String,
    pub proposed_fact_id: String,
    pub reason: String,
    pub created_at: i64,
    #[serde(default)]
    pub resolved: bool,
    pub resolution: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelProjectProgress {
    pub current_volume: Option<String>,
    pub current_chapter: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReport {
    pub project_id: String,
    pub previous_revision: u64,
    pub new_revision: u64,
    pub accepted_fact_ids: Vec<String>,
    pub conflicts: Vec<ConflictRecord>,
    pub graph_mirrored: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelRecallPack {
    pub project_id: String,
    pub project_title: String,
    pub revision: u64,
    pub task_type: NovelTaskType,
    pub branch_id: String,
    pub current_chapter: Option<u32>,
    pub facts: Vec<NovelFact>,
    pub rendered_context: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelTaskPhase {
    Preparing,
    Drafting,
    SelfReview,
    NeedsClarification,
    AwaitingMainReview,
    AwaitingUserDecision,
    ApprovedForPublication,
    PublicationPending,
    ArtifactSavedMemoryPending,
    Completed,
    Rejected,
    Cancelled,
    #[serde(alias = "recoverable_error")]
    Failed,
    StaleRevision,
}

impl NovelTaskPhase {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Rejected | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelLifecycleActor {
    User,
    Main,
    Novel,
    Memory,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelTaskEvent {
    pub event_id: String,
    pub task_id: String,
    pub project_id: String,
    pub actor: NovelLifecycleActor,
    pub phase: NovelTaskPhase,
    pub summary: String,
    #[serde(default)]
    pub details: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelTaskCheckpoint {
    pub task_id: String,
    pub project_id: String,
    pub phase: NovelTaskPhase,
    pub draft_version: u32,
    pub canon_revision: u64,
    pub output_path: String,
    #[serde(default)]
    pub state: Value,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelPublicationStatus {
    Pending,
    ArtifactSaved,
    Completed,
    Aborted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelArtifactReceipt {
    pub canonical_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub written_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelPublicationRecord {
    pub publication_id: String,
    pub task_id: String,
    pub project_id: String,
    pub draft_version: u32,
    pub expected_revision: u64,
    pub output_path: String,
    pub content_sha256: String,
    pub delta: NovelMemoryDelta,
    pub status: NovelPublicationStatus,
    pub artifact: Option<NovelArtifactReceipt>,
    pub commit_report: Option<CommitReport>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl NovelPublicationRecord {
    #[must_use]
    pub fn pending(
        publication_id: impl Into<String>,
        task_id: impl Into<String>,
        draft_version: u32,
        output_path: impl Into<String>,
        content_sha256: impl Into<String>,
        delta: NovelMemoryDelta,
    ) -> Self {
        let now = now_millis();
        Self {
            publication_id: publication_id.into(),
            task_id: task_id.into(),
            project_id: delta.project_id.clone(),
            draft_version,
            expected_revision: delta.expected_revision,
            output_path: output_path.into(),
            content_sha256: content_sha256.into(),
            delta,
            status: NovelPublicationStatus::Pending,
            artifact: None,
            commit_report: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRole {
    Body,
    PreviousChapter,
    ChapterOutline,
    VolumeOutline,
    CharacterCard,
    WorldSetting,
    StyleSample,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRef {
    pub role: ContextRole,
    pub canonical_path: PathBuf,
    pub sha256: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelConversationSource {
    pub conversation_id: String,
    pub generation_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPolicy {
    #[default]
    RequireUserAcceptance,
    AutoAfterMainReview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelTaskRequest {
    pub task_id: String,
    pub project_id: String,
    pub task_type: NovelTaskType,
    pub task_brief: String,
    #[serde(default)]
    pub target_chapter: Option<u32>,
    pub expected_revision: u64,
    pub output_path: PathBuf,
    #[serde(default)]
    pub context_refs: Vec<ContextRef>,
    #[serde(default)]
    pub must_happen: Vec<String>,
    #[serde(default)]
    pub must_not_change: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub allow_web_research: bool,
    #[serde(default)]
    pub publication_policy: PublicationPolicy,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_generation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelResumeInput {
    pub input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewCheckStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReviewIssue {
    Message(String),
    Detailed {
        category: String,
        message: String,
        #[serde(default)]
        evidence_refs: Vec<String>,
    },
}

impl ReviewIssue {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Message(message) | Self::Detailed { message, .. } => message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelSelfReviewChecks {
    pub outline_alignment: ReviewCheckStatus,
    pub canon_consistency: ReviewCheckStatus,
    pub character_consistency: ReviewCheckStatus,
    pub timeline_consistency: ReviewCheckStatus,
    pub plot_and_foreshadowing: ReviewCheckStatus,
    pub style_and_repetition: ReviewCheckStatus,
}

impl NovelSelfReviewChecks {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        [
            self.outline_alignment,
            self.canon_consistency,
            self.character_consistency,
            self.timeline_consistency,
            self.plot_and_foreshadowing,
            self.style_and_repetition,
        ]
        .into_iter()
        .all(|status| status == ReviewCheckStatus::Pass)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelSelfReviewVerdict {
    Pass,
    NeedsRevision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelSelfReview {
    pub verdict: NovelSelfReviewVerdict,
    pub checks: NovelSelfReviewChecks,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
    #[serde(default)]
    pub unverified_assumptions: Vec<String>,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelDraftEnvelope {
    pub task_id: String,
    pub draft_version: u32,
    pub project_id: String,
    pub canon_revision: u64,
    pub content: String,
    pub self_review: NovelSelfReview,
    pub proposed_delta: NovelMemoryDelta,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationRequest {
    pub task_id: String,
    pub project_id: String,
    pub questions: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
// Boxing DraftReady would break the established public matching API during cutover.
#[allow(clippy::large_enum_variant)]
pub enum NovelOutcome {
    NeedsClarification(ClarificationRequest),
    DraftReady(NovelDraftEnvelope),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MainReviewChecks {
    pub user_requirements: ReviewCheckStatus,
    pub outline_alignment: ReviewCheckStatus,
    pub canon_consistency: ReviewCheckStatus,
    pub character_consistency: ReviewCheckStatus,
    pub timeline_consistency: ReviewCheckStatus,
    pub plot_and_foreshadowing: ReviewCheckStatus,
    pub style_quality: ReviewCheckStatus,
    pub pacing_and_hook: ReviewCheckStatus,
}

impl MainReviewChecks {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        [
            self.user_requirements,
            self.outline_alignment,
            self.canon_consistency,
            self.character_consistency,
            self.timeline_consistency,
            self.plot_and_foreshadowing,
            self.style_quality,
            self.pacing_and_hook,
        ]
        .into_iter()
        .all(|status| status == ReviewCheckStatus::Pass)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainReviewVerdict {
    Pass,
    Revise,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainReviewRecord {
    pub task_id: String,
    pub draft_version: u32,
    pub reviewed_canon_revision: u64,
    pub verdict: MainReviewVerdict,
    pub checks: MainReviewChecks,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserDecision {
    Accept,
    Revise,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserDecisionRecord {
    pub task_id: String,
    pub draft_version: u32,
    pub decision: UserDecision,
    #[serde(default)]
    pub feedback: Option<String>,
    #[serde(default = "now_millis")]
    pub decided_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum NovelTransition {
    DraftReady { draft: NovelDraftEnvelope },
    NeedsClarification { clarification: ClarificationRequest },
    AwaitingUserDecision { draft: NovelDraftEnvelope },
    ApprovedForPublication { task_id: String, draft_version: u32 },
    Rejected { task_id: String, draft_version: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationReceipt {
    pub publication_id: String,
    pub task_id: String,
    pub project_id: String,
    pub draft_version: u32,
    pub artifact: NovelArtifactReceipt,
    pub commit_report: CommitReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelCandidate {
    pub candidate_id: String,
    pub task_id: String,
    pub project_id: String,
    pub draft_version: u32,
    pub canon_revision: u64,
    pub artifact_id: String,
    pub content_hash: String,
    pub created_at: i64,
}

impl NovelCandidate {
    pub fn validate_review(&self, review: &CandidateReview) -> Result<()> {
        if review.candidate_id != self.candidate_id
            || review.candidate_content_hash != self.content_hash
        {
            return Err(NovelDomainError::InvalidCandidate(
                "review does not match the sealed candidate id and content hash".into(),
            ));
        }
        require_non_empty("review id", &review.review_id)?;
        require_non_empty("reviewer artifact id", &review.reviewer_artifact_id)?;
        require_non_empty("review summary", &review.summary)?;
        if review
            .evidence_refs
            .iter()
            .any(|item| item.trim().is_empty())
        {
            return Err(NovelDomainError::InvalidCandidate(
                "candidate review evidence references must be non-empty".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateReviewVerdict {
    Approve,
    Revise,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReview {
    pub review_id: String,
    pub candidate_id: String,
    pub candidate_content_hash: String,
    pub reviewer_artifact_id: String,
    pub verdict: CandidateReviewVerdict,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub summary: String,
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn validate_identifier(field: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(NovelDomainError::InvalidRequest(format!(
            "{field} may contain only letters, numbers, hyphens, and underscores"
        )))
    }
}

pub(crate) fn require_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(NovelDomainError::InvalidRequest(format!(
            "{field} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn default_branch() -> String {
    "main".into()
}

const fn default_confidence() -> f64 {
    0.7
}

const fn default_canon_status() -> CanonStatus {
    CanonStatus::Draft
}
