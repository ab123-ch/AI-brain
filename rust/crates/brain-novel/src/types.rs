use std::path::PathBuf;

use brain_memory::novel::{
    CommitReport, NovelArtifactReceipt, NovelMemoryDelta, NovelPublicationRecord, NovelTaskPhase,
    NovelTaskType,
};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Server-owned provenance for invalidating uncommitted work when a Web
    /// conversation is edited or retried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_generation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationRequest {
    pub task_id: String,
    pub project_id: String,
    pub questions: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDecisionRecord {
    pub task_id: String,
    pub draft_version: u32,
    pub decision: UserDecision,
    #[serde(default)]
    pub feedback: Option<String>,
    #[serde(default = "now_millis")]
    pub decided_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum NovelTransition {
    DraftReady { draft: NovelDraftEnvelope },
    NeedsClarification { clarification: ClarificationRequest },
    AwaitingUserDecision { draft: NovelDraftEnvelope },
    ApprovedForPublication { task_id: String, draft_version: u32 },
    Rejected { task_id: String, draft_version: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationReceipt {
    pub publication_id: String,
    pub task_id: String,
    pub project_id: String,
    pub draft_version: u32,
    pub artifact: NovelArtifactReceipt,
    pub commit_report: CommitReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentBrainStatus {
    Starting,
    Ready,
    Degraded,
    ShuttingDown,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelProjectStatusView {
    pub project_id: String,
    pub task_id: Option<String>,
    pub phase: Option<NovelTaskPhase>,
    pub draft_version: u32,
    pub canon_revision: u64,
    pub history_messages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelBrainStatus {
    pub brain_id: String,
    pub status: ResidentBrainStatus,
    pub projects: Vec<NovelProjectStatusView>,
    pub pending_publications: Vec<NovelPublicationRecord>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelBrainEventKind {
    Activated,
    Degraded,
    Deactivated,
    ProjectLoaded,
    ProjectEvicted,
    TaskStarted,
    TaskResumed,
    MemoryRecalled,
    ConsistencyChecked,
    ClarificationRequested,
    DraftReady,
    MainReviewRecorded,
    RevisionStarted,
    UserDecisionRecorded,
    PublicationPending,
    ArtifactSaved,
    CanonCommitted,
    TaskCancelled,
    TaskFailed,
    StaleRevision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelBrainEvent {
    pub event_id: String,
    pub kind: NovelBrainEventKind,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub summary: String,
    pub created_at: i64,
}

pub(crate) fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
