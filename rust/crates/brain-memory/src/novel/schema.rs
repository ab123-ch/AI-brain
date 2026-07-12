use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelFact {
    pub fact_id: String,
    pub kind: NovelFactKind,
    /// 同类事实的稳定业务键，例如 `character:lin-mo:location`。
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// 未解决和已解决的 Canon 冲突审计记录。
    #[serde(default)]
    pub conflicts: Vec<ConflictRecord>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl NovelProject {
    #[must_use]
    pub fn new(project_id: impl Into<String>, title: impl Into<String>) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
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
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChange {
    pub fact: ProposedFact,
    pub supersedes_fact_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlotUpdate {
    pub fact: ProposedFact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeshadowingUpdate {
    pub fact: ProposedFact,
    pub resolves_fact_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceCandidate {
    pub fact: ProposedFact,
    #[serde(default)]
    pub evidence_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelMemoryDelta {
    pub project_id: String,
    #[serde(default = "default_branch")]
    pub branch_id: String,
    pub expected_revision: u64,
    pub task_type: NovelTaskType,
    pub source_ref: String,
    /// 文件保存成功后随事实一起推进的项目进度。
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
    pub(crate) fn all_facts(&self) -> impl Iterator<Item = &ProposedFact> {
        self.proposed_facts
            .iter()
            .chain(self.state_changes.iter().map(|change| &change.fact))
            .chain(self.plot_updates.iter().map(|update| &update.fact))
            .chain(self.foreshadowing_updates.iter().map(|update| &update.fact))
            .chain(self.feedback.iter())
            .chain(self.experience_candidates.iter().map(|item| &item.fact))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NovelProjectProgress {
    pub current_volume: Option<String>,
    pub current_chapter: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitReport {
    pub project_id: String,
    pub previous_revision: u64,
    pub new_revision: u64,
    pub accepted_fact_ids: Vec<String>,
    pub conflicts: Vec<ConflictRecord>,
    pub graph_mirrored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

fn default_branch() -> String {
    "main".into()
}

const fn default_confidence() -> f64 {
    0.7
}

const fn default_canon_status() -> CanonStatus {
    CanonStatus::Draft
}
