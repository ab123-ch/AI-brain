//! Durable collaboration-room control state.
//!
//! The repository deliberately owns only short SQLite transactions. Model and
//! tool execution happens in `collaboration_runtime` after a claim has been
//! committed and every database handle has been released.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use brain_llm::config::ResolvedModelPolicy;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use task_engine::SchedulerLimits;
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 7;
const DEFAULT_PROFILE_ID: &str = "general_member";
pub const DEFAULT_MEMBER_TEMPLATE_ID: &str = "general-member";
pub const DEFAULT_THREAD_KEY: &str = "room";
pub const LOCAL_PRINCIPAL_ID: &str = "local-user";

#[must_use]
pub(crate) fn default_runtime_dir() -> PathBuf {
    let fallback = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ai-brain");
    dirs::home_dir().unwrap_or(fallback).join(".ai-brain")
}

#[derive(Debug, Error)]
pub enum CollaborationError {
    #[error("协作存储错误: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("协作目录错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("房间不存在: {0}")]
    RoomNotFound(String),
    #[error("成员不存在: {0}")]
    MemberNotFound(String),
    #[error("成员 {member_id} 当前为 {availability}，不能接收新任务")]
    MemberUnavailable {
        member_id: String,
        availability: String,
    },
    #[error("房间成员数已达到上限 {0}")]
    MemberCapacity(usize),
    #[error("成员 {member_id} 的待处理消息已达到上限 {limit}")]
    InboxCapacity { member_id: String, limit: usize },
    #[error("房间待处理消息已达到上限 {0}")]
    RoomInboxCapacity(usize),
    #[error("一次最多可以选择 {0} 个成员")]
    RecipientCapacity(usize),
    #[error("至少需要选择一个成员")]
    EmptyRecipients,
    #[error("消息不能为空")]
    EmptyMessage,
    #[error("成员回复不能为空")]
    EmptyMemberReply,
    #[error("成员名称不能为空")]
    EmptyMemberName,
    #[error("房间内已存在同名的活动成员: {0}")]
    DuplicateMemberName(String),
    #[error("不允许使用模型策略: {0}")]
    ModelPolicyNotAllowed(String),
    #[error("不允许使用思考深度: {0}")]
    ReasoningDepthNotAllowed(String),
    #[error("成员 {0} 已归档，请先显式恢复")]
    MemberArchived(String),
    #[error("运行不存在或已经结束: {0}")]
    RunNotActive(String),
    #[error("主体 {principal_id} 缺少房间 {room_id} 的能力 {capability:?}")]
    CapabilityDenied {
        principal_id: String,
        room_id: String,
        capability: RoomCapability,
    },
    #[error("{entity} {id} 版本冲突，期望 {expected}，实际 {actual}")]
    VersionConflict {
        entity: &'static str,
        id: String,
        expected: u64,
        actual: u64,
    },
    #[error("成员线程标识不能为空")]
    EmptyThreadKey,
    #[error("成员模板不存在: {0}")]
    MemberTemplateNotFound(String),
    #[error("回复目标不存在: {0}")]
    ReplyTargetNotFound(String),
    #[error("回复目标 {event_id} 属于房间 {target_room_id}，不能用于房间 {room_id}")]
    ReplyTargetRoomMismatch {
        event_id: String,
        room_id: String,
        target_room_id: String,
    },
    #[error("回复目标已经失效: {0}")]
    ReplyTargetInvalidated(String),
    #[error("回复目标 {event_id} 的类型不受支持: {sender_kind}/{kind}")]
    ReplyTargetTypeUnsupported {
        event_id: String,
        sender_kind: String,
        kind: String,
    },
    #[error("协作配置错误: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, CollaborationError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationActor {
    pub principal_id: String,
}

impl CollaborationActor {
    pub fn new(principal_id: impl Into<String>) -> Self {
        Self {
            principal_id: principal_id.into(),
        }
    }

    pub fn local() -> Self {
        Self::new(LOCAL_PRINCIPAL_ID)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomCapability {
    RoomRead,
    RoomPost,
    ConfigureRoom,
    MentionMember,
    SubmitTask,
    CreateMember,
    ConfigureMember,
    WakeSleepMember,
    ArchiveRestoreMember,
    InterruptOwnRun,
    InterruptAnyRun,
    OverrideMemberModel,
    OverrideMemberReasoning,
    ManageMembership,
}

impl RoomCapability {
    fn owner_capabilities() -> Vec<Self> {
        vec![
            Self::RoomRead,
            Self::RoomPost,
            Self::ConfigureRoom,
            Self::MentionMember,
            Self::SubmitTask,
            Self::CreateMember,
            Self::ConfigureMember,
            Self::WakeSleepMember,
            Self::ArchiveRestoreMember,
            Self::InterruptOwnRun,
            Self::InterruptAnyRun,
            Self::OverrideMemberModel,
            Self::OverrideMemberReasoning,
            Self::ManageMembership,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomRole {
    Owner,
    Editor,
    Viewer,
    Service,
}

impl RoomRole {
    fn as_db(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Editor => "editor",
            Self::Viewer => "viewer",
            Self::Service => "service",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "owner" => Self::Owner,
            "editor" => Self::Editor,
            "service" => Self::Service,
            _ => Self::Viewer,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMembershipView {
    pub room_id: String,
    pub principal_id: String,
    pub role: RoomRole,
    pub capabilities: Vec<RoomCapability>,
    pub capability_version: u64,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberTemplateView {
    pub template_id: String,
    pub display_name: String,
    pub profile_id: String,
    pub default_model_policy: String,
    pub default_reasoning_depth: String,
    pub allowed_model_policies: Vec<String>,
    pub allowed_reasoning_depths: Vec<String>,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAddress {
    pub member_id: String,
    pub expected_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberCursorKind {
    Summary,
    Replay,
    Notification,
}

impl MemberCursorKind {
    fn as_db(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Replay => "replay",
            Self::Notification => "notification",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "replay" => Self::Replay,
            "notification" => Self::Notification,
            _ => Self::Summary,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberCursorView {
    pub member_id: String,
    pub cursor_kind: MemberCursorKind,
    pub event_sequence: u64,
    pub version: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationOutboxEvent {
    pub outbox_event_id: String,
    pub room_id: String,
    pub event_kind: String,
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub version: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CollaborationConfig {
    pub max_members_per_room: usize,
    pub max_pending_items_per_member: usize,
    pub max_pending_items_per_room: usize,
    pub max_recipients_per_message: usize,
    pub max_workers: usize,
    pub max_global_runs: usize,
    pub max_runs_per_room: usize,
    pub max_runs_per_member: usize,
    pub max_runs_per_provider: usize,
    pub max_runs_per_profile: usize,
    pub max_runs_per_task: usize,
    pub task_input_token_limit: u64,
    pub task_output_token_limit: u64,
    pub max_history_events_per_run: usize,
    pub max_debate_depth: u32,
    pub max_group_replies_per_conversation: usize,
    pub max_group_replies_per_member: usize,
    pub default_model_policy: String,
    pub default_reasoning_depth: String,
    pub allowed_model_policies: Vec<String>,
    pub allowed_reasoning_depths: Vec<String>,
}

impl Default for CollaborationConfig {
    fn default() -> Self {
        Self {
            max_members_per_room: 8,
            max_pending_items_per_member: 32,
            max_pending_items_per_room: 256,
            max_recipients_per_message: 8,
            max_workers: 4,
            max_global_runs: 4,
            max_runs_per_room: 4,
            max_runs_per_member: 1,
            max_runs_per_provider: 4,
            max_runs_per_profile: 4,
            max_runs_per_task: 4,
            task_input_token_limit: 4_000_000,
            task_output_token_limit: 524_288,
            max_history_events_per_run: 80,
            max_debate_depth: 3,
            max_group_replies_per_conversation: 12,
            max_group_replies_per_member: 2,
            default_model_policy: "main".into(),
            default_reasoning_depth: "medium".into(),
            allowed_model_policies: vec!["main".into()],
            allowed_reasoning_depths: vec!["low".into(), "medium".into(), "high".into()],
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ConfigRoot {
    #[serde(default)]
    collaboration: CollaborationConfig,
}

impl CollaborationConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = if path.exists() {
            let content = fs::read_to_string(path)?;
            toml::from_str::<ConfigRoot>(&content)
                .map_err(|error| CollaborationError::Config(error.to_string()))?
                .collaboration
        } else {
            Self::default()
        };
        config.normalize()?;
        Ok(config)
    }

    /// 使用已经验证可执行的实例模型目录覆盖协作成员模板的模型白名单。
    ///
    /// `main` 保留为兼容既有成员的策略；已有成员记录不会因此被改写。
    #[must_use]
    pub fn with_available_model_policies<I>(mut self, policies: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let mut seen = HashSet::from([String::from("main")]);
        let mut available = vec![String::from("main")];
        for policy in policies {
            let policy = policy.trim();
            if !policy.is_empty() && seen.insert(policy.to_owned()) {
                available.push(policy.to_owned());
            }
        }
        self.allowed_model_policies = available;
        if !self
            .allowed_model_policies
            .iter()
            .any(|policy| policy == &self.default_model_policy)
        {
            self.default_model_policy = "main".into();
        }
        self
    }

    #[must_use]
    pub(crate) const fn scheduler_limits(&self) -> SchedulerLimits {
        SchedulerLimits {
            max_workers: self.max_workers,
            max_global: self.max_global_runs,
            max_per_room: self.max_runs_per_room,
            max_per_member: self.max_runs_per_member,
            max_per_provider: self.max_runs_per_provider,
            max_per_profile: self.max_runs_per_profile,
            max_per_task: self.max_runs_per_task,
        }
    }

    fn normalize(&mut self) -> Result<()> {
        if self.max_members_per_room == 0
            || self.max_pending_items_per_member == 0
            || self.max_pending_items_per_room == 0
            || self.max_recipients_per_message == 0
            || self.max_workers == 0
            || self.max_global_runs == 0
            || self.max_runs_per_room == 0
            || self.max_runs_per_member == 0
            || self.max_runs_per_provider == 0
            || self.max_runs_per_profile == 0
            || self.max_runs_per_task == 0
            || self.task_input_token_limit == 0
            || self.task_output_token_limit == 0
            || self.max_history_events_per_run == 0
            || self.max_debate_depth == 0
            || self.max_group_replies_per_conversation == 0
            || self.max_group_replies_per_member == 0
        {
            return Err(CollaborationError::Config(
                "容量和并发配置必须大于 0".into(),
            ));
        }
        self.allowed_model_policies
            .retain(|value| !value.trim().is_empty());
        self.allowed_model_policies.sort();
        self.allowed_model_policies.dedup();
        self.allowed_reasoning_depths
            .retain(|value| !value.trim().is_empty());
        self.allowed_reasoning_depths.sort();
        self.allowed_reasoning_depths.dedup();
        if !self
            .allowed_model_policies
            .contains(&self.default_model_policy)
        {
            return Err(CollaborationError::Config(format!(
                "默认模型策略 {} 不在 allowlist 中",
                self.default_model_policy
            )));
        }
        if !self
            .allowed_reasoning_depths
            .contains(&self.default_reasoning_depth)
        {
            return Err(CollaborationError::Config(format!(
                "默认思考深度 {} 不在 allowlist 中",
                self.default_reasoning_depth
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberAvailability {
    Active,
    SleepAfterCurrent,
    Sleeping,
    Archived,
}

impl MemberAvailability {
    fn as_db(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::SleepAfterCurrent => "sleep_after_current",
            Self::Sleeping => "sleeping",
            Self::Archived => "archived",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "sleep_after_current" => Self::SleepAfterCurrent,
            "sleeping" => Self::Sleeping,
            "archived" => Self::Archived,
            _ => Self::Active,
        }
    }
}

impl std::fmt::Display for MemberAvailability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_db())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberActivity {
    Idle,
    Queued,
    Running,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxState {
    Pending,
    Leased,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxPurpose {
    Direct,
    Participation,
}

impl InboxPurpose {
    fn as_db(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Participation => "participation",
        }
    }

    fn from_db(value: &str) -> Self {
        if value == "participation" {
            Self::Participation
        } else {
            Self::Direct
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    Direct,
    Ambient,
}

impl DeliveryKind {
    fn as_db(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Ambient => "ambient",
        }
    }

    fn from_db(value: &str) -> Self {
        if value == "ambient" {
            Self::Ambient
        } else {
            Self::Direct
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    Running,
    Replied,
    Silent,
    Deferred,
    Observed,
    Suppressed,
}

impl DeliveryState {
    fn as_db(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Replied => "replied",
            Self::Silent => "silent",
            Self::Deferred => "deferred",
            Self::Observed => "observed",
            Self::Suppressed => "suppressed",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "replied" => Self::Replied,
            "silent" => Self::Silent,
            "deferred" => Self::Deferred,
            "observed" => Self::Observed,
            "suppressed" => Self::Suppressed,
            _ => Self::Queued,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipationDisposition {
    Replied,
    Silent,
    Suppressed,
    Cancelled,
}

impl InboxState {
    fn from_db(value: &str) -> Self {
        match value {
            "leased" => Self::Leased,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomInputMode {
    Chat,
    Task,
}

impl RoomInputMode {
    fn as_db(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Task => "task",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationRoomView {
    pub room_id: String,
    pub title: String,
    pub working_directory: String,
    pub default_member_id: String,
    pub latest_event_seq: u64,
    pub version: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainMemberView {
    pub member_id: String,
    pub room_id: String,
    pub display_name: String,
    pub template_id: String,
    pub profile_id: String,
    pub model_policy: String,
    pub reasoning_depth: String,
    pub availability: MemberAvailability,
    pub activity: MemberActivity,
    pub active_run_id: Option<String>,
    pub pending_count: usize,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub last_woken_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomEventView {
    pub event_id: String,
    pub room_id: String,
    pub sequence: u64,
    pub sender_kind: String,
    pub sender_id: String,
    pub sender_name: String,
    pub recipients: Vec<String>,
    #[serde(default)]
    pub audience: Vec<String>,
    pub kind: String,
    pub content: String,
    pub run_id: Option<String>,
    pub parent_event_id: Option<String>,
    #[serde(default)]
    pub reply_reference: Option<RoomEventReferenceView>,
    pub conversation_root_event_id: String,
    pub debate_depth: u32,
    pub group_enabled: bool,
    pub conversation_mode: RoomInputMode,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomEventReferenceView {
    pub event_id: String,
    pub sequence: u64,
    pub sender_kind: String,
    pub sender_id: String,
    pub sender_name: String,
    pub kind: String,
    pub content: String,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

struct ValidatedReplyTarget {
    reference: RoomEventReferenceView,
    conversation_root_event_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomEventDeliveryView {
    pub event_id: String,
    pub member_id: String,
    pub kind: DeliveryKind,
    pub state: DeliveryState,
    pub inbox_item_id: Option<String>,
    pub decision_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxItemView {
    pub inbox_item_id: String,
    pub member_id: String,
    pub source_event_id: String,
    pub thread_key: String,
    pub purpose: InboxPurpose,
    pub conversation_root_event_id: String,
    pub context_through_seq: u64,
    pub state: InboxState,
    pub mode: RoomInputMode,
    pub run_id: Option<String>,
    pub task_run_id: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub room: CollaborationRoomView,
    pub members: Vec<BrainMemberView>,
    pub events: Vec<RoomEventView>,
    #[serde(default)]
    pub has_earlier_events: bool,
    pub inbox: Vec<InboxItemView>,
    pub deliveries: Vec<RoomEventDeliveryView>,
    pub member_templates: Vec<MemberTemplateView>,
    pub model_policies: Vec<String>,
    #[serde(default)]
    pub model_policy_details: Vec<ResolvedModelPolicy>,
    pub reasoning_depths: Vec<String>,
    pub max_members: usize,
    pub max_workers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomEventPage {
    pub events: Vec<RoomEventView>,
    pub has_more: bool,
}

#[derive(Debug, Clone)]
pub struct LegacyMessageSeed {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub hidden: bool,
}

#[derive(Debug, Clone)]
pub struct PostMessageResult {
    pub event: RoomEventView,
    pub inbox_items: Vec<InboxItemView>,
    pub duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct ParticipationCompletion {
    pub disposition: ParticipationDisposition,
    pub event: Option<RoomEventView>,
}

#[derive(Debug, Clone)]
pub struct ClaimedInboxItem {
    pub inbox_item_id: String,
    pub room_id: String,
    pub member_id: String,
    pub member_name: String,
    pub profile_id: String,
    pub model_policy: String,
    pub reasoning_depth: String,
    pub source_event_id: String,
    pub source_event_seq: u64,
    pub thread_key: String,
    pub purpose: InboxPurpose,
    pub conversation_root_event_id: String,
    pub context_through_seq: u64,
    pub response_to_event_id: String,
    pub group_enabled: bool,
    pub execution_working_directory: PathBuf,
    pub reply_reference: Option<RoomEventReferenceView>,
    pub input: String,
    pub mode: RoomInputMode,
    pub run_id: String,
    pub task_run_id: String,
    pub version: u64,
}

#[derive(Debug, Clone)]
struct ClaimCandidate {
    inbox_item_id: String,
    room_id: Option<String>,
    member_id: String,
    member_name: String,
    profile_id: String,
    model_policy: String,
    reasoning_depth: String,
    source_event_id: String,
    source_event_found: bool,
    source_event_seq: Option<u64>,
    input: Option<String>,
    mode: String,
    task_run_id: String,
    version: u64,
    thread_key: String,
    purpose: String,
    conversation_root_event_id: String,
    context_through_seq: Option<u64>,
    response_to_event_id: String,
    group_enabled: Option<bool>,
    execution_working_directory: Option<String>,
    parent_event_id: Option<String>,
    reply_event_id: Option<String>,
    reply_room_id: Option<String>,
    reply_sequence: Option<u64>,
    reply_sender_kind: Option<String>,
    reply_sender_id: Option<String>,
    reply_sender_name: Option<String>,
    reply_kind: Option<String>,
    reply_content: Option<String>,
    reply_created_at: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct MemberReplyEventContext<'a> {
    room_id: &'a str,
    member_id: &'a str,
    member_name: &'a str,
    run_id: &'a str,
    response_to_event_id: &'a str,
    execution_working_directory: &'a Path,
}

impl<'a> From<&'a ClaimedInboxItem> for MemberReplyEventContext<'a> {
    fn from(claim: &'a ClaimedInboxItem) -> Self {
        Self {
            room_id: &claim.room_id,
            member_id: &claim.member_id,
            member_name: &claim.member_name,
            run_id: &claim.run_id,
            response_to_event_id: &claim.response_to_event_id,
            execution_working_directory: &claim.execution_working_directory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberHistoryMessage {
    pub event_id: String,
    pub sequence: u64,
    pub role: String,
    pub content: String,
    pub content_hash: String,
}

fn history_content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

#[derive(Debug, Clone)]
pub struct CollaborationRepository {
    database_path: PathBuf,
    config: CollaborationConfig,
    startup_working_directory: PathBuf,
}

impl CollaborationRepository {
    pub fn new(runtime_dir: &Path, config: CollaborationConfig) -> Result<Self> {
        let startup_working_directory = std::env::current_dir()?;
        Self::new_with_startup_working_directory(runtime_dir, config, &startup_working_directory)
    }

    pub fn new_with_startup_working_directory(
        runtime_dir: &Path,
        config: CollaborationConfig,
        startup_working_directory: &Path,
    ) -> Result<Self> {
        let startup_working_directory = startup_working_directory.canonicalize()?;
        if !startup_working_directory.is_dir() {
            return Err(CollaborationError::Config(format!(
                "启动工作目录不是目录: {}",
                startup_working_directory.display()
            )));
        }
        fs::create_dir_all(runtime_dir)?;
        let repository = Self {
            database_path: runtime_dir.join("runtime.db"),
            config,
            startup_working_directory,
        };
        repository.initialize()?;
        Ok(repository)
    }

    pub fn config(&self) -> &CollaborationConfig {
        &self.config
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.database_path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;\nPRAGMA synchronous = FULL;")?;
        Ok(connection)
    }

    #[allow(clippy::too_many_lines)]
    fn initialize(&self) -> Result<()> {
        let mut connection = self.connect()?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS collaboration_schema (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 version INTEGER NOT NULL
             );
             INSERT INTO collaboration_schema(singleton, version) VALUES (1, 1)
                 ON CONFLICT(singleton) DO NOTHING;
             CREATE TABLE IF NOT EXISTS collaboration_rooms (
                 room_id TEXT PRIMARY KEY,
                 workspace_id TEXT NOT NULL DEFAULT 'local',
                 title TEXT NOT NULL,
                 working_directory TEXT NOT NULL,
                 default_member_id TEXT NOT NULL,
                 latest_event_seq INTEGER NOT NULL DEFAULT 0,
                 room_summary_ref TEXT,
                 room_summary_through_seq INTEGER NOT NULL DEFAULT 0,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS member_templates (
                 template_id TEXT PRIMARY KEY,
                 display_name TEXT NOT NULL,
                 profile_id TEXT NOT NULL,
                 default_model_policy TEXT NOT NULL,
                 default_reasoning_depth TEXT NOT NULL,
                 allowed_model_policies_json TEXT NOT NULL,
                 allowed_reasoning_depths_json TEXT NOT NULL,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS room_principal_memberships (
                 room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id) ON DELETE CASCADE,
                 principal_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 capabilities_json TEXT NOT NULL,
                 capability_version INTEGER NOT NULL DEFAULT 1,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY(room_id, principal_id)
             );
             CREATE TABLE IF NOT EXISTS brain_members (
                 member_id TEXT PRIMARY KEY,
                 room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id),
                 display_name TEXT NOT NULL,
                 template_id TEXT NOT NULL DEFAULT 'general-member'
                     REFERENCES member_templates(template_id),
                 profile_id TEXT NOT NULL,
                 model_policy TEXT NOT NULL,
                 reasoning_depth TEXT NOT NULL,
                 availability TEXT NOT NULL,
                 member_scope TEXT NOT NULL DEFAULT 'member-private',
                 private_summary_ref TEXT,
                 private_summary_through_seq INTEGER NOT NULL DEFAULT 0,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 last_woken_at TEXT
             );
             CREATE INDEX IF NOT EXISTS brain_members_room_idx
                 ON brain_members(room_id, created_at);
             CREATE TABLE IF NOT EXISTS room_events (
                 event_id TEXT PRIMARY KEY,
                 room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id),
                 sequence INTEGER NOT NULL,
                 sender_kind TEXT NOT NULL,
                 sender_id TEXT NOT NULL,
                 sender_name TEXT NOT NULL,
                 visibility TEXT NOT NULL DEFAULT 'room',
                 kind TEXT NOT NULL,
                 content TEXT NOT NULL,
                 run_id TEXT,
                 parent_event_id TEXT,
                 conversation_root_event_id TEXT,
                 debate_depth INTEGER NOT NULL DEFAULT 0,
                 group_enabled INTEGER NOT NULL DEFAULT 0,
                 conversation_mode TEXT NOT NULL DEFAULT 'chat',
                 idempotency_key TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 invalidated_at TEXT,
                 execution_working_directory TEXT,
                 UNIQUE(room_id, sequence),
                 UNIQUE(room_id, idempotency_key)
             );
             CREATE INDEX IF NOT EXISTS room_events_room_seq_idx
                 ON room_events(room_id, sequence);
             CREATE TABLE IF NOT EXISTS room_event_recipients (
                 event_id TEXT NOT NULL REFERENCES room_events(event_id) ON DELETE CASCADE,
                 member_id TEXT NOT NULL REFERENCES brain_members(member_id),
                 PRIMARY KEY(event_id, member_id)
             );
             CREATE INDEX IF NOT EXISTS room_event_recipients_member_idx
                 ON room_event_recipients(member_id, event_id);
             CREATE TABLE IF NOT EXISTS member_inbox_items (
                 inbox_item_id TEXT PRIMARY KEY,
                 member_id TEXT NOT NULL REFERENCES brain_members(member_id),
                 source_event_id TEXT NOT NULL REFERENCES room_events(event_id),
                 thread_key TEXT NOT NULL DEFAULT 'room',
                 purpose TEXT NOT NULL DEFAULT 'direct',
                 conversation_root_event_id TEXT,
                 context_through_seq INTEGER NOT NULL DEFAULT 0,
                 mode TEXT NOT NULL,
                 state TEXT NOT NULL,
                 task_run_id TEXT,
                 run_id TEXT,
                 idempotency_key TEXT,
                 expected_member_version INTEGER NOT NULL DEFAULT 1,
                 cancel_requested INTEGER NOT NULL DEFAULT 0,
                 reply_event_id TEXT REFERENCES room_events(event_id),
                 error TEXT,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 started_at TEXT,
                 completed_at TEXT,
                 lease_expires_at TEXT,
                 UNIQUE(member_id, source_event_id)
             );
             CREATE INDEX IF NOT EXISTS member_inbox_state_idx
                 ON member_inbox_items(state, created_at);
             CREATE INDEX IF NOT EXISTS member_inbox_member_state_idx
                 ON member_inbox_items(member_id, state, created_at);
             CREATE TABLE IF NOT EXISTS room_event_deliveries (
                 event_id TEXT NOT NULL REFERENCES room_events(event_id) ON DELETE CASCADE,
                 member_id TEXT NOT NULL REFERENCES brain_members(member_id),
                 delivery_kind TEXT NOT NULL,
                 state TEXT NOT NULL,
                 inbox_item_id TEXT,
                 decision_reason TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY(event_id, member_id)
             );
             CREATE INDEX IF NOT EXISTS room_event_deliveries_member_idx
                 ON room_event_deliveries(member_id, state, updated_at);
             CREATE TABLE IF NOT EXISTS room_summary_snapshots (
                 summary_id TEXT PRIMARY KEY,
                 room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id) ON DELETE CASCADE,
                 content_ref TEXT NOT NULL,
                 through_sequence INTEGER NOT NULL,
                 base_version INTEGER NOT NULL,
                 content_hash TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(room_id, through_sequence)
             );
             CREATE TABLE IF NOT EXISTS member_summary_snapshots (
                 summary_id TEXT PRIMARY KEY,
                 member_id TEXT NOT NULL REFERENCES brain_members(member_id) ON DELETE CASCADE,
                 content_ref TEXT NOT NULL,
                 through_sequence INTEGER NOT NULL,
                 base_version INTEGER NOT NULL,
                 content_hash TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(member_id, through_sequence)
             );
             CREATE TABLE IF NOT EXISTS member_cursors (
                 member_id TEXT NOT NULL REFERENCES brain_members(member_id) ON DELETE CASCADE,
                 cursor_kind TEXT NOT NULL,
                 event_sequence INTEGER NOT NULL DEFAULT 0,
                 version INTEGER NOT NULL DEFAULT 1,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY(member_id, cursor_kind)
             );
             CREATE TABLE IF NOT EXISTS runtime_outbox_events (
                 outbox_event_id TEXT PRIMARY KEY,
                 room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id) ON DELETE CASCADE,
                 event_kind TEXT NOT NULL,
                 aggregate_kind TEXT NOT NULL,
                 aggregate_id TEXT NOT NULL,
                 idempotency_key TEXT NOT NULL UNIQUE,
                 state TEXT NOT NULL DEFAULT 'pending',
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 published_at TEXT
             );
             CREATE INDEX IF NOT EXISTS runtime_outbox_pending_idx
                 ON runtime_outbox_events(state, created_at);",
        )?;
        let mut version: u32 = connection.query_row(
            "SELECT version FROM collaboration_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if version == 1 {
            if !table_has_column(&connection, "member_inbox_items", "lease_expires_at")? {
                connection.execute(
                    "ALTER TABLE member_inbox_items ADD COLUMN lease_expires_at TEXT",
                    [],
                )?;
            }
            connection.execute(
                "UPDATE collaboration_schema SET version = 2 WHERE singleton = 1",
                [],
            )?;
            version = 2;
        }
        if version == 2 {
            for (table, column, definition) in [
                (
                    "collaboration_rooms",
                    "workspace_id",
                    "workspace_id TEXT NOT NULL DEFAULT 'local'",
                ),
                (
                    "collaboration_rooms",
                    "room_summary_ref",
                    "room_summary_ref TEXT",
                ),
                (
                    "collaboration_rooms",
                    "room_summary_through_seq",
                    "room_summary_through_seq INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "brain_members",
                    "template_id",
                    "template_id TEXT NOT NULL DEFAULT 'general-member'",
                ),
                (
                    "brain_members",
                    "member_scope",
                    "member_scope TEXT NOT NULL DEFAULT 'member-private'",
                ),
                (
                    "brain_members",
                    "private_summary_ref",
                    "private_summary_ref TEXT",
                ),
                (
                    "brain_members",
                    "private_summary_through_seq",
                    "private_summary_through_seq INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "room_events",
                    "visibility",
                    "visibility TEXT NOT NULL DEFAULT 'room'",
                ),
                (
                    "member_inbox_items",
                    "thread_key",
                    "thread_key TEXT NOT NULL DEFAULT 'room'",
                ),
                ("member_inbox_items", "task_run_id", "task_run_id TEXT"),
                (
                    "member_inbox_items",
                    "idempotency_key",
                    "idempotency_key TEXT",
                ),
                (
                    "member_inbox_items",
                    "expected_member_version",
                    "expected_member_version INTEGER NOT NULL DEFAULT 1",
                ),
            ] {
                if !table_has_column(&connection, table, column)? {
                    connection
                        .execute(&format!("ALTER TABLE {table} ADD COLUMN {definition}"), [])?;
                }
            }
            connection.execute_batch(
                "CREATE INDEX IF NOT EXISTS member_inbox_thread_state_idx
                     ON member_inbox_items(member_id, thread_key, state, created_at);",
            )?;
            connection.execute(
                "UPDATE collaboration_schema SET version = 3 WHERE singleton = 1",
                [],
            )?;
            version = 3;
        }
        if version == 3 {
            for (table, column, definition) in [
                ("room_events", "parent_event_id", "parent_event_id TEXT"),
                (
                    "room_events",
                    "conversation_root_event_id",
                    "conversation_root_event_id TEXT",
                ),
                (
                    "room_events",
                    "debate_depth",
                    "debate_depth INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "room_events",
                    "group_enabled",
                    "group_enabled INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "room_events",
                    "conversation_mode",
                    "conversation_mode TEXT NOT NULL DEFAULT 'chat'",
                ),
                (
                    "member_inbox_items",
                    "purpose",
                    "purpose TEXT NOT NULL DEFAULT 'direct'",
                ),
                (
                    "member_inbox_items",
                    "conversation_root_event_id",
                    "conversation_root_event_id TEXT",
                ),
                (
                    "member_inbox_items",
                    "context_through_seq",
                    "context_through_seq INTEGER NOT NULL DEFAULT 0",
                ),
            ] {
                if !table_has_column(&connection, table, column)? {
                    connection
                        .execute(&format!("ALTER TABLE {table} ADD COLUMN {definition}"), [])?;
                }
            }
            connection.execute(
                "UPDATE room_events
                 SET conversation_root_event_id = event_id
                 WHERE conversation_root_event_id IS NULL",
                [],
            )?;
            connection.execute(
                "UPDATE member_inbox_items
                 SET conversation_root_event_id = source_event_id
                 WHERE conversation_root_event_id IS NULL",
                [],
            )?;
            connection.execute(
                "UPDATE collaboration_schema SET version = 4 WHERE singleton = 1",
                [],
            )?;
            version = 4;
        }
        if version == 4 {
            if !table_has_column(&connection, "room_events", "invalidated_at")? {
                connection.execute("ALTER TABLE room_events ADD COLUMN invalidated_at TEXT", [])?;
            }
            connection.execute(
                "UPDATE collaboration_schema SET version = 5 WHERE singleton = 1",
                [],
            )?;
            version = 5;
        }
        if version == 5 {
            normalize_active_member_display_names(&connection)?;
            connection.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS brain_members_active_display_name_idx
                     ON brain_members(room_id, display_name COLLATE NOCASE)
                     WHERE availability != 'archived';",
            )?;
            connection.execute(
                "UPDATE collaboration_schema SET version = 6 WHERE singleton = 1",
                [],
            )?;
            version = 6;
        }
        if version == 6 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !table_has_column(&transaction, "collaboration_rooms", "working_directory")? {
                transaction.execute(
                    "ALTER TABLE collaboration_rooms
                     ADD COLUMN working_directory TEXT NOT NULL DEFAULT ''",
                    [],
                )?;
            }
            if !table_has_column(&transaction, "room_events", "execution_working_directory")? {
                transaction.execute(
                    "ALTER TABLE room_events ADD COLUMN execution_working_directory TEXT",
                    [],
                )?;
            }
            let startup_working_directory = self.startup_working_directory.display().to_string();
            transaction.execute(
                "UPDATE collaboration_rooms SET working_directory = ?1",
                [&startup_working_directory],
            )?;
            transaction.execute(
                "UPDATE room_events SET execution_working_directory = ?1",
                [&startup_working_directory],
            )?;
            transaction.execute(
                "UPDATE room_principal_memberships
                 SET capabilities_json = ?1, capability_version = 2,
                     version = version + 1, updated_at = ?2
                 WHERE principal_id = ?3 AND role = 'owner'",
                params![
                    serialize_capabilities(&RoomCapability::owner_capabilities())?,
                    Utc::now().to_rfc3339(),
                    LOCAL_PRINCIPAL_ID,
                ],
            )?;
            transaction.execute(
                "UPDATE collaboration_schema SET version = 7 WHERE singleton = 1",
                [],
            )?;
            transaction.commit()?;
            version = 7;
        }
        if version != SCHEMA_VERSION {
            return Err(CollaborationError::Config(format!(
                "不支持的协作存储版本 {version}"
            )));
        }
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS member_inbox_thread_state_idx
                 ON member_inbox_items(member_id, thread_key, state, created_at);
             CREATE UNIQUE INDEX IF NOT EXISTS member_active_participation_root_idx
                 ON member_inbox_items(member_id, conversation_root_event_id)
                 WHERE purpose = 'participation'
                   AND state IN ('pending', 'leased', 'running');
             CREATE INDEX IF NOT EXISTS room_event_deliveries_member_idx
                 ON room_event_deliveries(member_id, state, updated_at);
             CREATE INDEX IF NOT EXISTS room_events_visible_sequence_idx
                 ON room_events(room_id, invalidated_at, sequence);",
        )?;
        let now = Utc::now().to_rfc3339();
        let allowed_models = serde_json::to_string(&self.config.allowed_model_policies)
            .map_err(|error| CollaborationError::Config(error.to_string()))?;
        let allowed_depths = serde_json::to_string(&self.config.allowed_reasoning_depths)
            .map_err(|error| CollaborationError::Config(error.to_string()))?;
        connection.execute(
            "INSERT INTO member_templates(
                 template_id, display_name, profile_id, default_model_policy,
                 default_reasoning_depth, allowed_model_policies_json,
                 allowed_reasoning_depths_json, version, created_at
             ) VALUES (?1, '通用智脑', ?2, ?3, ?4, ?5, ?6, 1, ?7)
             ON CONFLICT(template_id) DO UPDATE SET
                 default_model_policy = excluded.default_model_policy,
                 default_reasoning_depth = excluded.default_reasoning_depth,
                 allowed_model_policies_json = excluded.allowed_model_policies_json,
                 allowed_reasoning_depths_json = excluded.allowed_reasoning_depths_json",
            params![
                DEFAULT_MEMBER_TEMPLATE_ID,
                DEFAULT_PROFILE_ID,
                self.config.default_model_policy,
                self.config.default_reasoning_depth,
                allowed_models,
                allowed_depths,
                now,
            ],
        )?;
        let owner_capabilities = serialize_capabilities(&RoomCapability::owner_capabilities())?;
        connection.execute(
            "INSERT INTO room_principal_memberships(
                 room_id, principal_id, role, capabilities_json, capability_version,
                 version, created_at, updated_at
             )
             SELECT room_id, ?1, 'owner', ?2, 2, 1, ?3, ?3
             FROM collaboration_rooms
             WHERE 1
             ON CONFLICT(room_id, principal_id) DO NOTHING",
            params![LOCAL_PRINCIPAL_ID, owner_capabilities, now],
        )?;
        connection.execute(
            "UPDATE member_inbox_items
             SET idempotency_key = (
                 SELECT e.idempotency_key || ':' || member_inbox_items.member_id
                 FROM room_events e
                 WHERE e.event_id = member_inbox_items.source_event_id
             )
             WHERE idempotency_key IS NULL",
            [],
        )?;
        if table_exists(&connection, "task_runs")? {
            connection.execute(
                "UPDATE member_inbox_items
                 SET task_run_id = (
                     SELECT t.task_run_id
                     FROM task_runs t
                     WHERE t.origin_kind = 'member_inbox'
                       AND t.origin_id = member_inbox_items.inbox_item_id
                 )
                 WHERE task_run_id IS NULL
                   AND EXISTS (
                       SELECT 1 FROM task_runs t
                       WHERE t.origin_kind = 'member_inbox'
                         AND t.origin_id = member_inbox_items.inbox_item_id
                   )",
                [],
            )?;
        }
        Ok(())
    }

    pub fn ensure_room(
        &self,
        room_id: &str,
        title: &str,
        legacy_messages: &[LegacyMessageSeed],
    ) -> Result<RoomSnapshot> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let default_member_id = format!("member-{room_id}-main");
        let now = Utc::now();
        transaction.execute(
            "INSERT INTO collaboration_rooms(
                 room_id, title, working_directory, default_member_id,
                 latest_event_seq, version, created_at
             ) VALUES (?1, ?2, ?3, ?4, 0, 1, ?5)
             ON CONFLICT(room_id) DO UPDATE SET title = excluded.title",
            params![
                room_id,
                title,
                self.startup_working_directory.display().to_string(),
                default_member_id,
                now.to_rfc3339(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO brain_members(
                 member_id, room_id, display_name, template_id, profile_id, model_policy,
                 reasoning_depth, availability, version, created_at, last_woken_at
             ) VALUES (?1, ?2, '智脑 A', ?3, ?4, ?5, ?6, 'active', 1, ?7, ?7)
             ON CONFLICT(member_id) DO NOTHING",
            params![
                default_member_id,
                room_id,
                DEFAULT_MEMBER_TEMPLATE_ID,
                DEFAULT_PROFILE_ID,
                self.config.default_model_policy,
                self.config.default_reasoning_depth,
                now.to_rfc3339(),
            ],
        )?;
        ensure_local_owner_membership(&transaction, room_id, &now)?;
        let execution_working_directory: String = transaction.query_row(
            "SELECT working_directory FROM collaboration_rooms WHERE room_id = ?1",
            [room_id],
            |row| row.get(0),
        )?;
        let execution_working_directory =
            required_execution_working_directory(execution_working_directory)?;

        for (index, message) in legacy_messages.iter().enumerate() {
            if message.hidden || !matches!(message.role.as_str(), "user" | "assistant") {
                continue;
            }
            let stable_id = if message.id.trim().is_empty() {
                format!("index-{index}")
            } else {
                message.id.clone()
            };
            let idempotency_key = format!("legacy:{stable_id}");
            if event_by_idempotency(&transaction, room_id, &idempotency_key)?.is_some() {
                continue;
            }
            let sequence = allocate_room_sequence(&transaction, room_id)?;
            let event_id = format!("legacy-{room_id}-{stable_id}");
            let (sender_kind, sender_id, sender_name, kind) = if message.role == "user" {
                ("user", "user", "用户", "user_message")
            } else {
                (
                    "member",
                    default_member_id.as_str(),
                    "智脑 A",
                    "member_message",
                )
            };
            transaction.execute(
                "INSERT INTO room_events(
                 event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                     kind, content, run_id, idempotency_key,
                     execution_working_directory, created_at
                  ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11)",
                params![
                    event_id,
                    room_id,
                    sequence,
                    sender_kind,
                    sender_id,
                    sender_name,
                    kind,
                    message.content,
                    idempotency_key,
                    execution_working_directory.display().to_string(),
                    message.timestamp.to_rfc3339(),
                ],
            )?;
            if message.role == "user" {
                transaction.execute(
                    "INSERT INTO room_event_recipients(event_id, member_id) VALUES (?1, ?2)",
                    params![event_id, default_member_id],
                )?;
            }
        }
        enqueue_room_changed(
            &transaction,
            room_id,
            "room",
            room_id,
            &format!("room-ensured:{room_id}"),
        )?;
        transaction.commit()?;
        self.snapshot(room_id)
    }

    pub fn update_room_working_directory(
        &self,
        room_id: &str,
        working_directory: &str,
        expected_version: u64,
    ) -> Result<CollaborationRoomView> {
        self.update_room_working_directory_as(
            &CollaborationActor::local(),
            room_id,
            working_directory,
            expected_version,
        )
    }

    pub fn update_room_working_directory_as(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        working_directory: &str,
        expected_version: u64,
    ) -> Result<CollaborationRoomView> {
        {
            let connection = self.connect()?;
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM collaboration_rooms WHERE room_id = ?1)",
                [room_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(CollaborationError::RoomNotFound(room_id.into()));
            }
            require_capability(&connection, actor, room_id, RoomCapability::ConfigureRoom)?;
        }

        let working_directory = working_directory.trim();
        if working_directory.is_empty() {
            return Err(CollaborationError::Config("房间工作目录不能为空".into()));
        }
        let requested_path = Path::new(working_directory);
        let candidate = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            self.startup_working_directory.join(requested_path)
        };
        let canonical = candidate.canonicalize()?;
        if !canonical.is_dir() {
            return Err(CollaborationError::Config(format!(
                "房间工作目录不是目录: {}",
                canonical.display()
            )));
        }
        let canonical = canonical.display().to_string();

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_capability(&transaction, actor, room_id, RoomCapability::ConfigureRoom)?;
        let actual_version = transaction
            .query_row(
                "SELECT version FROM collaboration_rooms WHERE room_id = ?1",
                [room_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .ok_or_else(|| CollaborationError::RoomNotFound(room_id.into()))?;
        ensure_version("room", room_id, expected_version, actual_version)?;
        transaction.execute(
            "UPDATE collaboration_rooms
             SET working_directory = ?1, version = version + 1
             WHERE room_id = ?2",
            params![canonical, room_id],
        )?;
        enqueue_room_changed(
            &transaction,
            room_id,
            "room",
            room_id,
            &format!("room-working-directory:{room_id}:{}", Uuid::new_v4()),
        )?;
        let room = room_from_connection(&transaction, room_id)?;
        transaction.commit()?;
        Ok(room)
    }

    pub fn create_member(
        &self,
        room_id: &str,
        display_name: &str,
        model_policy: Option<&str>,
        reasoning_depth: Option<&str>,
    ) -> Result<BrainMemberView> {
        self.create_member_as(
            &CollaborationActor::local(),
            room_id,
            display_name,
            DEFAULT_MEMBER_TEMPLATE_ID,
            model_policy,
            reasoning_depth,
        )
    }

    pub fn create_member_as(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        display_name: &str,
        template_id: &str,
        model_policy: Option<&str>,
        reasoning_depth: Option<&str>,
    ) -> Result<BrainMemberView> {
        let display_name = display_name.trim();
        if display_name.is_empty() {
            return Err(CollaborationError::EmptyMemberName);
        }

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_exists(&transaction, room_id)?;
        require_capability(&transaction, actor, room_id, RoomCapability::CreateMember)?;
        let template = member_template_from_connection(&transaction, template_id)?;
        let model_policy = model_policy.unwrap_or(&template.default_model_policy);
        let reasoning_depth = reasoning_depth.unwrap_or(&template.default_reasoning_depth);
        validate_template_policy(&template, model_policy, reasoning_depth)?;
        if model_policy != template.default_model_policy {
            require_capability(
                &transaction,
                actor,
                room_id,
                RoomCapability::OverrideMemberModel,
            )?;
        }
        if reasoning_depth != template.default_reasoning_depth {
            require_capability(
                &transaction,
                actor,
                room_id,
                RoomCapability::OverrideMemberReasoning,
            )?;
        }
        let count: usize = transaction.query_row(
            "SELECT COUNT(*) FROM brain_members WHERE room_id = ?1 AND availability != 'archived'",
            [room_id],
            |row| row.get(0),
        )?;
        if count >= self.config.max_members_per_room {
            return Err(CollaborationError::MemberCapacity(
                self.config.max_members_per_room,
            ));
        }
        ensure_unique_active_member_display_name(&transaction, room_id, display_name, None)?;
        let member_id = format!("member-{}", Uuid::new_v4());
        let now = Utc::now();
        transaction.execute(
            "INSERT INTO brain_members(
                 member_id, room_id, display_name, template_id, profile_id, model_policy,
                 reasoning_depth, availability, version, created_at, last_woken_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 1, ?8, ?8)",
            params![
                member_id,
                room_id,
                display_name,
                template.template_id,
                template.profile_id,
                model_policy,
                reasoning_depth,
                now.to_rfc3339(),
            ],
        )?;
        append_service_event(
            &transaction,
            room_id,
            &format!("member-created:{member_id}"),
            "member_created",
            &format!("已创建成员 {display_name}"),
        )?;
        enqueue_room_changed(
            &transaction,
            room_id,
            "member",
            &member_id,
            &format!("member-created:{member_id}"),
        )?;
        transaction.commit()?;
        self.member(room_id, &member_id)
    }

    pub fn configure_member(
        &self,
        room_id: &str,
        member_id: &str,
        display_name: &str,
        model_policy: &str,
        reasoning_depth: &str,
        expected_version: u64,
    ) -> Result<BrainMemberView> {
        self.configure_member_as(
            &CollaborationActor::local(),
            room_id,
            member_id,
            display_name,
            model_policy,
            reasoning_depth,
            expected_version,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn configure_member_as(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        member_id: &str,
        display_name: &str,
        model_policy: &str,
        reasoning_depth: &str,
        expected_version: u64,
    ) -> Result<BrainMemberView> {
        let display_name = display_name.trim();
        if display_name.is_empty() {
            return Err(CollaborationError::EmptyMemberName);
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_capability(
            &transaction,
            actor,
            room_id,
            RoomCapability::ConfigureMember,
        )?;
        let (template_id, current_model, current_depth, actual_version) = transaction
            .query_row(
                "SELECT template_id, model_policy, reasoning_depth, version
                 FROM brain_members WHERE room_id = ?1 AND member_id = ?2",
                params![room_id, member_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| CollaborationError::MemberNotFound(member_id.into()))?;
        ensure_version("member", member_id, expected_version, actual_version)?;
        let template = member_template_from_connection(&transaction, &template_id)?;
        validate_template_policy(&template, model_policy, reasoning_depth)?;
        if model_policy != current_model {
            require_capability(
                &transaction,
                actor,
                room_id,
                RoomCapability::OverrideMemberModel,
            )?;
        }
        if reasoning_depth != current_depth {
            require_capability(
                &transaction,
                actor,
                room_id,
                RoomCapability::OverrideMemberReasoning,
            )?;
        }
        ensure_unique_active_member_display_name(
            &transaction,
            room_id,
            display_name,
            Some(member_id),
        )?;
        let updated = transaction.execute(
            "UPDATE brain_members
             SET display_name = ?1, model_policy = ?2, reasoning_depth = ?3, version = version + 1
             WHERE room_id = ?4 AND member_id = ?5 AND version = ?6 AND availability != 'archived'",
            params![
                display_name,
                model_policy,
                reasoning_depth,
                room_id,
                member_id,
                expected_version,
            ],
        )?;
        if updated == 0 {
            return Err(CollaborationError::VersionConflict {
                entity: "member",
                id: member_id.into(),
                expected: expected_version,
                actual: member_version(&transaction, room_id, member_id)?,
            });
        }
        append_service_event(
            &transaction,
            room_id,
            &format!("member-configured:{member_id}:{}", expected_version + 1),
            "member_configured",
            &format!("已更新成员 {display_name} 的运行配置"),
        )?;
        enqueue_room_changed(
            &transaction,
            room_id,
            "member",
            member_id,
            &format!("member-configured:{member_id}:{}", expected_version + 1),
        )?;
        transaction.commit()?;
        self.member(room_id, member_id)
    }

    pub fn wake_member(&self, room_id: &str, member_id: &str) -> Result<BrainMemberView> {
        let version = self.member(room_id, member_id)?.version;
        self.wake_member_checked(&CollaborationActor::local(), room_id, member_id, version)
    }

    pub fn sleep_member(&self, room_id: &str, member_id: &str) -> Result<BrainMemberView> {
        let version = self.member(room_id, member_id)?.version;
        self.sleep_member_checked(&CollaborationActor::local(), room_id, member_id, version)
    }

    pub fn archive_member(&self, room_id: &str, member_id: &str) -> Result<BrainMemberView> {
        let version = self.member(room_id, member_id)?.version;
        self.archive_member_checked(&CollaborationActor::local(), room_id, member_id, version)
    }

    pub fn restore_member(&self, room_id: &str, member_id: &str) -> Result<BrainMemberView> {
        let version = self.member(room_id, member_id)?.version;
        self.restore_member_checked(&CollaborationActor::local(), room_id, member_id, version)
    }

    pub fn wake_member_checked(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        member_id: &str,
        expected_version: u64,
    ) -> Result<BrainMemberView> {
        self.set_member_availability(
            actor,
            room_id,
            member_id,
            expected_version,
            LifecycleCommand::Wake,
        )
    }

    pub fn sleep_member_checked(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        member_id: &str,
        expected_version: u64,
    ) -> Result<BrainMemberView> {
        self.set_member_availability(
            actor,
            room_id,
            member_id,
            expected_version,
            LifecycleCommand::Sleep,
        )
    }

    pub fn archive_member_checked(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        member_id: &str,
        expected_version: u64,
    ) -> Result<BrainMemberView> {
        self.set_member_availability(
            actor,
            room_id,
            member_id,
            expected_version,
            LifecycleCommand::Archive,
        )
    }

    pub fn restore_member_checked(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        member_id: &str,
        expected_version: u64,
    ) -> Result<BrainMemberView> {
        self.set_member_availability(
            actor,
            room_id,
            member_id,
            expected_version,
            LifecycleCommand::Restore,
        )
    }

    fn set_member_availability(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        member_id: &str,
        expected_version: u64,
        command: LifecycleCommand,
    ) -> Result<BrainMemberView> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let capability = match command {
            LifecycleCommand::Wake | LifecycleCommand::Sleep => RoomCapability::WakeSleepMember,
            LifecycleCommand::Archive | LifecycleCommand::Restore => {
                RoomCapability::ArchiveRestoreMember
            }
        };
        require_capability(&transaction, actor, room_id, capability)?;
        let (current, display_name, actual_version): (String, String, u64) = transaction
            .query_row(
                "SELECT availability, display_name, version FROM brain_members
                 WHERE room_id = ?1 AND member_id = ?2",
                params![room_id, member_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| CollaborationError::MemberNotFound(member_id.into()))?;
        ensure_version("member", member_id, expected_version, actual_version)?;
        let current = MemberAvailability::from_db(&current);
        let running: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM member_inbox_items
                 WHERE member_id = ?1 AND state IN ('leased', 'running')
             )",
            [member_id],
            |row| row.get(0),
        )?;
        let next = match command {
            LifecycleCommand::Wake => match current {
                MemberAvailability::Archived => {
                    return Err(CollaborationError::MemberArchived(member_id.into()))
                }
                _ => MemberAvailability::Active,
            },
            LifecycleCommand::Sleep => {
                if running {
                    MemberAvailability::SleepAfterCurrent
                } else {
                    MemberAvailability::Sleeping
                }
            }
            LifecycleCommand::Archive => MemberAvailability::Archived,
            LifecycleCommand::Restore => {
                if current == MemberAvailability::Archived {
                    MemberAvailability::Active
                } else {
                    current
                }
            }
        };
        if current == MemberAvailability::Archived && next == MemberAvailability::Active {
            ensure_unique_active_member_display_name(
                &transaction,
                room_id,
                &display_name,
                Some(member_id),
            )?;
        }
        let now = Utc::now().to_rfc3339();
        let updated = transaction.execute(
            "UPDATE brain_members
             SET availability = ?1,
                 last_woken_at = CASE WHEN ?1 = 'active' THEN ?2 ELSE last_woken_at END,
                 version = version + 1
             WHERE room_id = ?3 AND member_id = ?4 AND version = ?5",
            params![next.as_db(), now, room_id, member_id, expected_version],
        )?;
        if updated != 1 {
            return Err(CollaborationError::VersionConflict {
                entity: "member",
                id: member_id.into(),
                expected: expected_version,
                actual: member_version(&transaction, room_id, member_id)?,
            });
        }
        append_service_event(
            &transaction,
            room_id,
            &format!(
                "member-lifecycle:{member_id}:{}:{}",
                next.as_db(),
                Uuid::new_v4()
            ),
            "member_lifecycle",
            &format!("成员状态已切换为 {}", next.as_db()),
        )?;
        enqueue_room_changed(
            &transaction,
            room_id,
            "member",
            member_id,
            &format!(
                "member-lifecycle:{member_id}:{}:{}",
                next.as_db(),
                expected_version + 1
            ),
        )?;
        transaction.commit()?;
        self.member(room_id, member_id)
    }

    pub fn post_message(
        &self,
        room_id: &str,
        recipient_ids: &[String],
        content: &str,
        mode: RoomInputMode,
        idempotency_key: &str,
    ) -> Result<PostMessageResult> {
        self.post_message_in_thread(
            room_id,
            recipient_ids,
            content,
            mode,
            DEFAULT_THREAD_KEY,
            idempotency_key,
        )
    }

    pub fn post_message_in_thread(
        &self,
        room_id: &str,
        recipient_ids: &[String],
        content: &str,
        mode: RoomInputMode,
        thread_key: &str,
        idempotency_key: &str,
    ) -> Result<PostMessageResult> {
        let snapshot = self.snapshot(room_id)?;
        let recipients = recipient_ids
            .iter()
            .map(|member_id| {
                snapshot
                    .members
                    .iter()
                    .find(|member| member.member_id == *member_id)
                    .map(|member| MemberAddress {
                        member_id: member_id.clone(),
                        expected_version: member.version,
                    })
                    .ok_or_else(|| CollaborationError::MemberNotFound(member_id.clone()))
            })
            .collect::<Result<Vec<_>>>()?;
        self.post_message_checked(
            &CollaborationActor::local(),
            room_id,
            &recipients,
            content,
            mode,
            thread_key,
            snapshot.room.version,
            idempotency_key,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn post_message_checked(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        recipients: &[MemberAddress],
        content: &str,
        mode: RoomInputMode,
        thread_key: &str,
        expected_room_version: u64,
        idempotency_key: &str,
    ) -> Result<PostMessageResult> {
        self.post_message_checked_internal(
            actor,
            room_id,
            recipients,
            content,
            mode,
            thread_key,
            expected_room_version,
            idempotency_key,
            None,
            false,
        )
    }

    pub fn post_group_message(
        &self,
        room_id: &str,
        recipient_ids: &[String],
        content: &str,
        mode: RoomInputMode,
        idempotency_key: &str,
    ) -> Result<PostMessageResult> {
        let snapshot = self.snapshot(room_id)?;
        let recipients = recipient_ids
            .iter()
            .map(|member_id| {
                snapshot
                    .members
                    .iter()
                    .find(|member| member.member_id == *member_id)
                    .map(|member| MemberAddress {
                        member_id: member_id.clone(),
                        expected_version: member.version,
                    })
                    .ok_or_else(|| CollaborationError::MemberNotFound(member_id.clone()))
            })
            .collect::<Result<Vec<_>>>()?;
        self.post_group_message_checked(
            &CollaborationActor::local(),
            room_id,
            &recipients,
            content,
            mode,
            DEFAULT_THREAD_KEY,
            snapshot.room.version,
            idempotency_key,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn post_group_message_checked(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        recipients: &[MemberAddress],
        content: &str,
        mode: RoomInputMode,
        thread_key: &str,
        expected_room_version: u64,
        idempotency_key: &str,
    ) -> Result<PostMessageResult> {
        self.post_group_message_checked_with_reply(
            actor,
            room_id,
            recipients,
            content,
            mode,
            thread_key,
            expected_room_version,
            idempotency_key,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn post_group_message_checked_with_reply(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        recipients: &[MemberAddress],
        content: &str,
        mode: RoomInputMode,
        thread_key: &str,
        expected_room_version: u64,
        idempotency_key: &str,
        reply_to_event_id: Option<&str>,
    ) -> Result<PostMessageResult> {
        self.post_message_checked_internal(
            actor,
            room_id,
            recipients,
            content,
            mode,
            thread_key,
            expected_room_version,
            idempotency_key,
            reply_to_event_id,
            true,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn post_message_checked_internal(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        recipients: &[MemberAddress],
        content: &str,
        mode: RoomInputMode,
        thread_key: &str,
        expected_room_version: u64,
        idempotency_key: &str,
        reply_to_event_id: Option<&str>,
        group_enabled: bool,
    ) -> Result<PostMessageResult> {
        let content = content.trim();
        if content.is_empty() {
            return Err(CollaborationError::EmptyMessage);
        }
        let thread_key = thread_key.trim();
        if thread_key.is_empty() {
            return Err(CollaborationError::EmptyThreadKey);
        }
        let recipients = recipients
            .iter()
            .filter(|value| !value.member_id.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>();
        if recipients.is_empty() {
            return Err(CollaborationError::EmptyRecipients);
        }
        if recipients.len() > self.config.max_recipients_per_message {
            return Err(CollaborationError::RecipientCapacity(
                self.config.max_recipients_per_message,
            ));
        }

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_exists(&transaction, room_id)?;
        require_capability(&transaction, actor, room_id, RoomCapability::RoomPost)?;
        require_capability(
            &transaction,
            actor,
            room_id,
            match mode {
                RoomInputMode::Chat => RoomCapability::MentionMember,
                RoomInputMode::Task => RoomCapability::SubmitTask,
            },
        )?;
        if let Some(event) = event_by_idempotency(&transaction, room_id, idempotency_key)? {
            let inbox_items = inbox_for_event(&transaction, &event.event_id)?;
            transaction.commit()?;
            return Ok(PostMessageResult {
                event,
                inbox_items,
                duplicate: true,
            });
        }
        let (actual_room_version, execution_working_directory): (u64, String) = transaction
            .query_row(
                "SELECT version, working_directory FROM collaboration_rooms WHERE room_id = ?1",
                [room_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        ensure_version("room", room_id, expected_room_version, actual_room_version)?;
        let execution_working_directory =
            required_execution_working_directory(execution_working_directory)?;
        let reply_target = reply_to_event_id
            .map(|event_id| validated_reply_target(&transaction, room_id, event_id))
            .transpose()?;

        let room_pending: usize = transaction.query_row(
            "SELECT COUNT(*)
             FROM member_inbox_items i
             JOIN brain_members m ON m.member_id = i.member_id
             WHERE m.room_id = ?1 AND i.state IN ('pending', 'leased', 'running')",
            [room_id],
            |row| row.get(0),
        )?;
        if room_pending + recipients.len() > self.config.max_pending_items_per_room {
            return Err(CollaborationError::RoomInboxCapacity(
                self.config.max_pending_items_per_room,
            ));
        }

        let mut recipient_list = recipients;
        recipient_list.sort_by(|left, right| left.member_id.cmp(&right.member_id));
        for duplicate in recipient_list.windows(2) {
            if duplicate[0].member_id == duplicate[1].member_id {
                return Err(CollaborationError::Config(format!(
                    "收件人成员重复: {}",
                    duplicate[0].member_id
                )));
            }
        }
        for recipient in &recipient_list {
            let member_id = &recipient.member_id;
            let (availability, actual_member_version): (String, u64) = transaction
                .query_row(
                    "SELECT availability, version FROM brain_members
                     WHERE room_id = ?1 AND member_id = ?2",
                    params![room_id, member_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| CollaborationError::MemberNotFound(member_id.clone()))?;
            ensure_version(
                "member",
                member_id,
                recipient.expected_version,
                actual_member_version,
            )?;
            let availability = MemberAvailability::from_db(&availability);
            if availability != MemberAvailability::Active {
                return Err(CollaborationError::MemberUnavailable {
                    member_id: member_id.clone(),
                    availability: availability.to_string(),
                });
            }
            let member_pending: usize = transaction.query_row(
                "SELECT COUNT(*) FROM member_inbox_items
                 WHERE member_id = ?1 AND state IN ('pending', 'leased', 'running')",
                [member_id],
                |row| row.get(0),
            )?;
            if member_pending >= self.config.max_pending_items_per_member {
                return Err(CollaborationError::InboxCapacity {
                    member_id: member_id.clone(),
                    limit: self.config.max_pending_items_per_member,
                });
            }
        }

        let event_id = format!("event-{}", Uuid::new_v4());
        let sequence = allocate_room_sequence(&transaction, room_id)?;
        if reply_target
            .as_ref()
            .is_some_and(|target| target.reference.sequence >= sequence)
        {
            return Err(CollaborationError::Config(
                "回复目标序号必须早于新消息".into(),
            ));
        }
        let (parent_event_id, conversation_root_event_id, reply_reference) =
            match reply_target.as_ref() {
                Some(target) => (
                    Some(target.reference.event_id.clone()),
                    target.conversation_root_event_id.clone(),
                    Some(target.reference.clone()),
                ),
                None => (None, event_id.clone(), None),
            };
        let now = Utc::now();
        transaction.execute(
            "INSERT INTO room_events(
                 event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                 kind, content, run_id, parent_event_id, conversation_root_event_id,
                 debate_depth, group_enabled, conversation_mode, idempotency_key,
                 execution_working_directory, created_at
              ) VALUES (
                  ?1, ?2, ?3, 'user', 'user', '用户', 'user_message', ?4, NULL,
                  ?5, ?6, 0, ?7, ?8, ?9, ?10, ?11
              )",
            params![
                event_id,
                room_id,
                sequence,
                content,
                parent_event_id.as_deref(),
                conversation_root_event_id,
                group_enabled,
                mode.as_db(),
                idempotency_key,
                execution_working_directory.display().to_string(),
                now.to_rfc3339(),
            ],
        )?;
        let mut inbox_items = Vec::with_capacity(recipient_list.len());
        for recipient in &recipient_list {
            let member_id = &recipient.member_id;
            transaction.execute(
                "INSERT INTO room_event_recipients(event_id, member_id) VALUES (?1, ?2)",
                params![event_id, member_id],
            )?;
            let item = insert_inbox_item(
                &transaction,
                member_id,
                &event_id,
                thread_key,
                mode,
                InboxPurpose::Direct,
                &conversation_root_event_id,
                recipient.expected_version,
                &format!("{idempotency_key}:{member_id}"),
                &now,
            )?;
            if group_enabled {
                insert_delivery(
                    &transaction,
                    &event_id,
                    member_id,
                    DeliveryKind::Direct,
                    DeliveryState::Queued,
                    Some(&item.inbox_item_id),
                    None,
                    &now,
                )?;
            }
            inbox_items.push(item);
        }

        transaction.execute(
            "UPDATE collaboration_rooms
             SET title = CASE WHEN title = 'New Session' THEN ?1 ELSE title END,
                 version = version + 1
             WHERE room_id = ?2",
            params![truncate_title(content), room_id],
        )?;
        enqueue_room_changed(
            &transaction,
            room_id,
            "room_event",
            &event_id,
            &format!("room-message:{room_id}:{idempotency_key}"),
        )?;
        let event = RoomEventView {
            event_id: event_id.clone(),
            room_id: room_id.into(),
            sequence,
            sender_kind: "user".into(),
            sender_id: "user".into(),
            sender_name: "用户".into(),
            recipients: recipient_list
                .into_iter()
                .map(|recipient| recipient.member_id)
                .collect(),
            audience: Vec::new(),
            kind: "user_message".into(),
            content: content.into(),
            run_id: None,
            parent_event_id,
            reply_reference,
            conversation_root_event_id,
            debate_depth: 0,
            group_enabled,
            conversation_mode: mode,
            created_at: now,
        };
        transaction.commit()?;
        Ok(PostMessageResult {
            event,
            inbox_items,
            duplicate: false,
        })
    }

    pub fn claim_next(&self) -> Result<Option<ClaimedInboxItem>> {
        let Some(lease) = self.lease_next()? else {
            return Ok(None);
        };
        self.activate_lease(&lease).map(Some)
    }

    #[allow(clippy::too_many_lines)]
    pub fn lease_next(&self) -> Result<Option<ClaimedInboxItem>> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate = transaction
            .query_row(
                "SELECT i.inbox_item_id AS inbox_item_id,
                        e.room_id AS room_id,
                        i.member_id AS member_id,
                        m.display_name AS member_name,
                        m.profile_id AS profile_id,
                        m.model_policy AS model_policy,
                        m.reasoning_depth AS reasoning_depth,
                        i.source_event_id AS source_event_id,
                        e.event_id IS NOT NULL AS source_event_found,
                        e.sequence AS source_event_seq,
                        e.content AS input,
                        i.mode AS mode,
                        i.task_run_id AS task_run_id,
                        i.version AS version,
                        i.thread_key AS thread_key,
                        i.purpose AS purpose,
                        COALESCE(i.conversation_root_event_id, i.source_event_id)
                            AS conversation_root_event_id,
                        CASE WHEN i.purpose = 'participation'
                             THEN CASE WHEN i.context_through_seq > 0
                                       THEN i.context_through_seq ELSE room.latest_event_seq END
                             ELSE e.sequence END AS context_through_seq,
                        CASE WHEN i.purpose = 'participation' THEN COALESCE((
                            SELECT delivered.event_id
                            FROM room_event_deliveries delivery
                            JOIN room_events delivered ON delivered.event_id = delivery.event_id
                            WHERE delivery.member_id = i.member_id
                              AND COALESCE(delivered.conversation_root_event_id, delivered.event_id)
                                  = COALESCE(i.conversation_root_event_id, i.source_event_id)
                              AND delivered.sequence <= CASE WHEN i.context_through_seq > 0
                                  THEN i.context_through_seq ELSE room.latest_event_seq END
                               AND delivered.invalidated_at IS NULL
                             ORDER BY delivered.sequence DESC LIMIT 1
                        ), i.source_event_id) ELSE i.source_event_id END AS response_to_event_id,
                        e.group_enabled AS group_enabled,
                        e.execution_working_directory AS execution_working_directory,
                        e.parent_event_id AS parent_event_id,
                        parent.event_id AS reply_event_id,
                        parent.room_id AS reply_room_id,
                        parent.sequence AS reply_sequence,
                        parent.sender_kind AS reply_sender_kind,
                        parent.sender_id AS reply_sender_id,
                        parent.sender_name AS reply_sender_name,
                        parent.kind AS reply_kind,
                        parent.content AS reply_content,
                        parent.created_at AS reply_created_at
                 FROM member_inbox_items i
                 JOIN brain_members m ON m.member_id = i.member_id
                 LEFT JOIN room_events e ON e.event_id = i.source_event_id
                 LEFT JOIN collaboration_rooms room ON room.room_id = e.room_id
                 LEFT JOIN room_events parent ON parent.event_id = e.parent_event_id
                 WHERE i.state = 'pending'
                   AND e.invalidated_at IS NULL
                   AND m.availability = 'active'
                   AND NOT EXISTS (
                       SELECT 1 FROM member_inbox_items active
                       WHERE active.member_id = i.member_id
                         AND active.thread_key = i.thread_key
                         AND active.state IN ('leased', 'running')
                   )
                 ORDER BY CASE i.purpose WHEN 'direct' THEN 0 ELSE 1 END,
                          i.created_at, i.rowid
                 LIMIT 1",
                [],
                claimed_inbox_from_row,
            )
            .optional()?;
        let Some(candidate) = candidate else {
            transaction.commit()?;
            return Ok(None);
        };
        let run_id = format!("run-{}", Uuid::new_v4());
        let claim =
            match claimed_inbox_from_candidate(&candidate, run_id.clone(), candidate.version + 1) {
                Ok(claim) => claim,
                Err(error @ CollaborationError::Config(_)) => {
                    let quarantined =
                        quarantine_corrupt_claim(&transaction, &candidate, &error.to_string())?;
                    transaction.commit()?;
                    if quarantined {
                        return Err(error);
                    }
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
        let lease_expires_at = Utc::now() + chrono::Duration::minutes(5);
        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'leased', run_id = ?1, lease_expires_at = ?2,
                 context_through_seq = ?3, version = version + 1
             WHERE inbox_item_id = ?4 AND state = 'pending' AND version = ?5",
            params![
                &run_id,
                lease_expires_at.to_rfc3339(),
                claim.context_through_seq,
                &candidate.inbox_item_id,
                candidate.version
            ],
        )?;
        if updated == 0 {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute(
            "UPDATE room_event_deliveries
             SET state = 'running', updated_at = ?1
             WHERE inbox_item_id = ?2 AND state IN ('queued', 'deferred')",
            params![Utc::now().to_rfc3339(), &candidate.inbox_item_id],
        )?;
        enqueue_room_changed(
            &transaction,
            &claim.room_id,
            "inbox",
            &candidate.inbox_item_id,
            &format!("inbox-leased:{}:{run_id}", candidate.inbox_item_id),
        )?;
        transaction.commit()?;
        Ok(Some(claim))
    }

    pub fn claim_for_reconciliation(
        &self,
        inbox_item_id: &str,
        task_run_id: &str,
        durable_run_id: &str,
    ) -> Result<Option<ClaimedInboxItem>> {
        let connection = self.connect()?;
        let candidate = connection
            .query_row(
                "SELECT i.inbox_item_id AS inbox_item_id,
                        e.room_id AS room_id,
                        i.member_id AS member_id,
                        m.display_name AS member_name,
                        m.profile_id AS profile_id,
                        m.model_policy AS model_policy,
                        m.reasoning_depth AS reasoning_depth,
                        i.source_event_id AS source_event_id,
                        e.event_id IS NOT NULL AS source_event_found,
                        e.sequence AS source_event_seq,
                        e.content AS input,
                        i.mode AS mode,
                        i.task_run_id AS task_run_id,
                        i.version AS version,
                        i.thread_key AS thread_key,
                        i.purpose AS purpose,
                        COALESCE(i.conversation_root_event_id, i.source_event_id)
                            AS conversation_root_event_id,
                        CASE WHEN i.purpose = 'participation' AND i.context_through_seq > 0
                             THEN i.context_through_seq ELSE e.sequence END AS context_through_seq,
                        CASE WHEN i.purpose = 'participation' THEN COALESCE((
                            SELECT delivered.event_id
                            FROM room_event_deliveries delivery
                            JOIN room_events delivered ON delivered.event_id = delivery.event_id
                            WHERE delivery.member_id = i.member_id
                              AND COALESCE(delivered.conversation_root_event_id, delivered.event_id)
                                  = COALESCE(i.conversation_root_event_id, i.source_event_id)
                              AND delivered.sequence <= CASE WHEN i.context_through_seq > 0
                                  THEN i.context_through_seq ELSE e.sequence END
                               AND delivered.invalidated_at IS NULL
                             ORDER BY delivered.sequence DESC LIMIT 1
                        ), i.source_event_id) ELSE i.source_event_id END AS response_to_event_id,
                        e.group_enabled AS group_enabled,
                        e.execution_working_directory AS execution_working_directory,
                        e.parent_event_id AS parent_event_id,
                        parent.event_id AS reply_event_id,
                        parent.room_id AS reply_room_id,
                        parent.sequence AS reply_sequence,
                        parent.sender_kind AS reply_sender_kind,
                        parent.sender_id AS reply_sender_id,
                        parent.sender_name AS reply_sender_name,
                        parent.kind AS reply_kind,
                        parent.content AS reply_content,
                        parent.created_at AS reply_created_at
                 FROM member_inbox_items i
                 JOIN brain_members m ON m.member_id = i.member_id
                 LEFT JOIN room_events e ON e.event_id = i.source_event_id
                 LEFT JOIN room_events parent ON parent.event_id = e.parent_event_id
                 WHERE i.inbox_item_id = ?1 AND i.task_run_id = ?2
                   AND i.state NOT IN ('completed', 'cancelled')
                   AND e.invalidated_at IS NULL",
                params![inbox_item_id, task_run_id],
                claimed_inbox_from_row,
            )
            .optional()?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        claimed_inbox_from_candidate(&candidate, durable_run_id.into(), candidate.version).map(Some)
    }

    pub fn activate_lease(&self, claim: &ClaimedInboxItem) -> Result<ClaimedInboxItem> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'running', started_at = ?1, lease_expires_at = NULL,
                 version = version + 1
             WHERE inbox_item_id = ?2 AND run_id = ?3 AND state = 'leased' AND version = ?4",
            params![
                Utc::now().to_rfc3339(),
                claim.inbox_item_id,
                claim.run_id,
                claim.version,
            ],
        )?;
        if updated != 1 {
            return Err(CollaborationError::RunNotActive(claim.run_id.clone()));
        }
        enqueue_room_changed(
            &transaction,
            &claim.room_id,
            "inbox",
            &claim.inbox_item_id,
            &format!("inbox-running:{}:{}", claim.inbox_item_id, claim.run_id),
        )?;
        transaction.commit()?;
        let mut active = claim.clone();
        active.version += 1;
        Ok(active)
    }

    pub fn release_lease(&self, claim: &ClaimedInboxItem) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'pending', run_id = NULL, lease_expires_at = NULL,
                 version = version + 1
             WHERE inbox_item_id = ?1 AND run_id = ?2 AND state = 'leased' AND version = ?3",
            params![claim.inbox_item_id, claim.run_id, claim.version],
        )?;
        if updated == 1 {
            enqueue_room_changed(
                &transaction,
                &claim.room_id,
                "inbox",
                &claim.inbox_item_id,
                &format!("inbox-released:{}:{}", claim.inbox_item_id, claim.run_id),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// 将尚未提交结果的运行中 Claim 精确回退为待重试状态。
    ///
    /// 仅匹配同一 run，并在同一事务中读取当前版本后执行 CAS；
    /// 已经回退或已终结时按幂等成功处理。
    pub fn release_active_for_retry(&self, claim: &ClaimedInboxItem) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT state, run_id, version
                 FROM member_inbox_items WHERE inbox_item_id = ?1",
                [claim.inbox_item_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((current_state, current_run_id, current_version)) = current else {
            return Err(CollaborationError::RunNotActive(claim.run_id.clone()));
        };
        if matches!(
            current_state.as_str(),
            "pending" | "completed" | "failed" | "cancelled"
        ) {
            transaction.commit()?;
            return Ok(());
        }
        if current_state != "running" || current_run_id.as_deref() != Some(claim.run_id.as_str()) {
            return Err(CollaborationError::RunNotActive(claim.run_id.clone()));
        }

        let now = Utc::now().to_rfc3339();
        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'pending', run_id = NULL, started_at = NULL,
                 lease_expires_at = NULL, error = NULL, version = version + 1
             WHERE inbox_item_id = ?1 AND run_id = ?2
               AND state = 'running' AND version = ?3",
            params![claim.inbox_item_id, claim.run_id, current_version],
        )?;
        if updated == 1 {
            transaction.execute(
                "UPDATE room_event_deliveries
                 SET state = 'queued', decision_reason = NULL, updated_at = ?1
                 WHERE inbox_item_id = ?2 AND state = 'running'",
                params![now, claim.inbox_item_id],
            )?;
            enqueue_room_changed(
                &transaction,
                &claim.room_id,
                "inbox",
                &claim.inbox_item_id,
                &format!(
                    "inbox-active-released:{}:{}",
                    claim.inbox_item_id, claim.run_id
                ),
            )?;
            transaction.commit()?;
            return Ok(());
        }

        let actual_version = transaction
            .query_row(
                "SELECT version FROM member_inbox_items WHERE inbox_item_id = ?1",
                [claim.inbox_item_id.as_str()],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Err(CollaborationError::VersionConflict {
            entity: "member inbox run",
            id: claim.run_id.clone(),
            expected: current_version,
            actual: actual_version,
        })
    }

    /// 持久结果投影失败时，将受控打开的历史 Failed Inbox 恢复原状。
    fn restore_failed_reconciliation(
        &self,
        claim: &ClaimedInboxItem,
        original_run_id: Option<&str>,
    ) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT state, run_id, version
                 FROM member_inbox_items WHERE inbox_item_id = ?1",
                [claim.inbox_item_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((current_state, current_run_id, current_version)) = current else {
            return Err(CollaborationError::RunNotActive(claim.run_id.clone()));
        };
        if current_state == "failed" {
            if current_run_id.as_deref() == original_run_id {
                transaction.commit()?;
                return Ok(());
            }
            return Err(CollaborationError::RunNotActive(claim.run_id.clone()));
        }
        if current_state != "running" || current_run_id.as_deref() != Some(claim.run_id.as_str()) {
            return Err(CollaborationError::RunNotActive(claim.run_id.clone()));
        }
        if current_version != claim.version {
            return Err(CollaborationError::VersionConflict {
                entity: "member inbox run",
                id: claim.run_id.clone(),
                expected: claim.version,
                actual: current_version,
            });
        }

        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'failed', run_id = ?1, lease_expires_at = NULL,
                 version = version + 1
             WHERE inbox_item_id = ?2 AND run_id = ?3
               AND state = 'running' AND version = ?4",
            params![
                original_run_id,
                claim.inbox_item_id,
                claim.run_id,
                claim.version
            ],
        )?;
        if updated != 1 {
            return Err(CollaborationError::VersionConflict {
                entity: "member inbox run",
                id: claim.run_id.clone(),
                expected: claim.version,
                actual: transaction
                    .query_row(
                        "SELECT version FROM member_inbox_items WHERE inbox_item_id = ?1",
                        [claim.inbox_item_id.as_str()],
                        |row| row.get::<_, u64>(0),
                    )
                    .optional()?
                    .unwrap_or(0),
            });
        }
        enqueue_room_changed(
            &transaction,
            &claim.room_id,
            "inbox",
            &claim.inbox_item_id,
            &format!(
                "inbox-reconciliation-restored:{}:{}",
                claim.inbox_item_id, claim.run_id
            ),
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub fn member_history(&self, claim: &ClaimedInboxItem) -> Result<Vec<MemberHistoryMessage>> {
        let connection = self.connect()?;
        if claim.purpose == InboxPurpose::Participation {
            let mut statement = connection.prepare(
                "SELECT e.event_id, e.sequence, e.sender_kind, e.sender_id,
                        e.sender_name, e.content
                 FROM room_events e
                 WHERE e.room_id = ?1
                   AND e.sequence <= ?2
                   AND e.invalidated_at IS NULL
                   AND e.kind IN ('user_message', 'member_message')
                   AND COALESCE(e.conversation_root_event_id, e.event_id) = ?3
                   AND (
                       (e.sender_kind = 'member' AND e.sender_id = ?4)
                       OR EXISTS (
                           SELECT 1 FROM room_event_deliveries d
                           WHERE d.event_id = e.event_id AND d.member_id = ?4
                       )
                   )
                 ORDER BY e.sequence DESC
                 LIMIT ?5",
            )?;
            let rows = statement.query_map(
                params![
                    claim.room_id,
                    claim.context_through_seq,
                    claim.conversation_root_event_id,
                    claim.member_id,
                    self.config.max_history_events_per_run,
                ],
                |row| {
                    let event_id: String = row.get(0)?;
                    let sequence: u64 = row.get(1)?;
                    let sender_kind: String = row.get(2)?;
                    let sender_id: String = row.get(3)?;
                    let sender_name: String = row.get(4)?;
                    let content: String = row.get(5)?;
                    let own_message = sender_kind == "member" && sender_id == claim.member_id;
                    Ok(MemberHistoryMessage {
                        event_id,
                        sequence,
                        role: if own_message {
                            "assistant".into()
                        } else {
                            "user".into()
                        },
                        content_hash: history_content_hash(&content),
                        content: if own_message {
                            content
                        } else {
                            format!("[{sender_name}] {content}")
                        },
                    })
                },
            )?;
            let mut history = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            history.reverse();
            return Ok(history);
        }
        if claim.group_enabled {
            let mut statement = connection.prepare(
                "WITH recent_user_events AS (
                    SELECT e.sequence
                    FROM room_events e
                    WHERE e.room_id = ?1
                      AND e.sequence <= ?2
                      AND e.invalidated_at IS NULL
                      AND e.kind = 'user_message'
                    ORDER BY e.sequence DESC
                    LIMIT 3
                 ), window_start AS (
                    SELECT MIN(sequence) AS sequence FROM recent_user_events
                 )
                 SELECT e.event_id, e.sequence, e.sender_kind, e.sender_id,
                        e.sender_name, e.content
                 FROM room_events e
                 CROSS JOIN window_start w
                 WHERE e.room_id = ?1
                   AND e.sequence >= w.sequence
                   AND e.sequence < ?2
                   AND e.invalidated_at IS NULL
                   AND e.kind IN ('user_message', 'member_message')
                   AND (
                       e.sender_kind = 'user'
                       OR (e.sender_kind = 'member' AND e.sender_id = ?3)
                   )
                 ORDER BY e.sequence ASC",
            )?;
            let rows = statement.query_map(
                params![claim.room_id, claim.source_event_seq, claim.member_id],
                |row| {
                    let event_id: String = row.get(0)?;
                    let sequence: u64 = row.get(1)?;
                    let sender_kind: String = row.get(2)?;
                    let sender_id: String = row.get(3)?;
                    let sender_name: String = row.get(4)?;
                    let content: String = row.get(5)?;
                    let own_message = sender_kind == "member" && sender_id == claim.member_id;
                    Ok(MemberHistoryMessage {
                        event_id,
                        sequence,
                        role: if own_message {
                            "assistant".into()
                        } else {
                            "user".into()
                        },
                        content_hash: history_content_hash(&content),
                        content: if own_message {
                            content
                        } else {
                            format!("[{sender_name}] {content}")
                        },
                    })
                },
            )?;
            return rows
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into);
        }
        let mut statement = connection.prepare(
            "SELECT e.event_id, e.sequence, e.sender_kind, e.content
             FROM room_events e
             WHERE e.room_id = ?1
               AND e.sequence < ?2
               AND e.invalidated_at IS NULL
               AND e.kind IN ('user_message', 'member_message')
               AND (
                   (e.sender_kind = 'member' AND e.sender_id = ?3)
                   OR
                   (e.sender_kind = 'user' AND EXISTS (
                       SELECT 1 FROM room_event_recipients r
                       WHERE r.event_id = e.event_id AND r.member_id = ?3
                   ))
               )
             ORDER BY e.sequence DESC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                claim.room_id,
                claim.source_event_seq,
                claim.member_id,
                self.config.max_history_events_per_run,
            ],
            |row| {
                let event_id: String = row.get(0)?;
                let sequence: u64 = row.get(1)?;
                let sender_kind: String = row.get(2)?;
                let content: String = row.get(3)?;
                Ok(MemberHistoryMessage {
                    event_id,
                    sequence,
                    role: if sender_kind == "user" {
                        "user".into()
                    } else {
                        "assistant".into()
                    },
                    content_hash: history_content_hash(&content),
                    content,
                })
            },
        )?;
        let mut history = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        history.reverse();
        Ok(history)
    }

    fn append_member_reply_event(
        &self,
        transaction: &Transaction<'_>,
        context: MemberReplyEventContext<'_>,
        answer: &str,
        idempotency_key: &str,
        now: &DateTime<Utc>,
    ) -> Result<RoomEventView> {
        let parent = transaction
            .query_row(
                "SELECT event_id, COALESCE(conversation_root_event_id, event_id),
                        debate_depth, group_enabled, conversation_mode
                 FROM room_events
                 WHERE room_id = ?1 AND event_id = ?2",
                params![context.room_id, context.response_to_event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u32>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| CollaborationError::Config("群聊回复父事件不存在".into()))?;
        let (parent_event_id, conversation_root_event_id, parent_depth, group_enabled, mode) =
            parent;
        let conversation_mode = if mode == "task" {
            RoomInputMode::Task
        } else {
            RoomInputMode::Chat
        };
        let debate_depth = if group_enabled {
            parent_depth.saturating_add(1)
        } else {
            0
        };
        let event_id = format!("event-{}", Uuid::new_v4());
        let sequence = allocate_room_sequence(transaction, context.room_id)?;
        transaction.execute(
            "INSERT INTO room_events(
                 event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                 kind, content, run_id, parent_event_id, conversation_root_event_id,
                 debate_depth, group_enabled, conversation_mode, idempotency_key,
                 execution_working_directory, created_at
              ) VALUES (
                  ?1, ?2, ?3, 'member', ?4, ?5, 'member_message', ?6, ?7, ?8,
                  ?9, ?10, ?11, ?12, ?13, ?14, ?15
              )",
            params![
                event_id,
                context.room_id,
                sequence,
                context.member_id,
                context.member_name,
                answer,
                context.run_id,
                parent_event_id,
                conversation_root_event_id,
                debate_depth,
                group_enabled,
                conversation_mode.as_db(),
                idempotency_key,
                context.execution_working_directory.display().to_string(),
                now.to_rfc3339(),
            ],
        )?;
        let event = RoomEventView {
            event_id: event_id.clone(),
            room_id: context.room_id.into(),
            sequence,
            sender_kind: "member".into(),
            sender_id: context.member_id.into(),
            sender_name: context.member_name.into(),
            recipients: Vec::new(),
            audience: Vec::new(),
            kind: "member_message".into(),
            content: answer.into(),
            run_id: Some(context.run_id.into()),
            parent_event_id: Some(parent_event_id),
            reply_reference: None,
            conversation_root_event_id,
            debate_depth,
            group_enabled,
            conversation_mode,
            created_at: *now,
        };
        hydrate_events(transaction, vec![event])?
            .pop()
            .ok_or_else(|| CollaborationError::Config("成员回复事件水合失败".into()))
    }

    #[allow(clippy::too_many_lines)]
    fn queue_latest_deferred_participation(
        &self,
        transaction: &Transaction<'_>,
        claim: &ClaimedInboxItem,
        now: &DateTime<Utc>,
    ) -> Result<()> {
        let deferred = transaction
            .query_row(
                "SELECT delivered.event_id, delivered.debate_depth, delivered.conversation_mode
                 FROM room_event_deliveries delivery
                 JOIN room_events delivered ON delivered.event_id = delivery.event_id
                 WHERE delivery.member_id = ?1
                   AND delivery.state = 'deferred'
                   AND delivered.room_id = ?2
                   AND COALESCE(delivered.conversation_root_event_id, delivered.event_id) = ?3
                   AND delivered.sequence > ?4
                   AND delivered.invalidated_at IS NULL
                 ORDER BY delivered.sequence DESC LIMIT 1",
                params![
                    claim.member_id,
                    claim.room_id,
                    claim.conversation_root_event_id,
                    claim.context_through_seq,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((event_id, debate_depth, mode)) = deferred else {
            return Ok(());
        };
        if mode != "chat" || debate_depth >= self.config.max_debate_depth {
            return Ok(());
        }
        let already_pending: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM member_inbox_items
                 WHERE member_id = ?1
                   AND COALESCE(conversation_root_event_id, source_event_id) = ?2
                   AND purpose = 'participation'
                   AND state IN ('pending', 'leased', 'running')
             )",
            params![claim.member_id, claim.conversation_root_event_id],
            |row| row.get(0),
        )?;
        if already_pending {
            return Ok(());
        }
        let (member_version, member_pending): (u64, usize) = transaction.query_row(
            "SELECT version,
                    (SELECT COUNT(*) FROM member_inbox_items i
                     WHERE i.member_id = brain_members.member_id
                       AND i.state IN ('pending', 'leased', 'running'))
             FROM brain_members
             WHERE member_id = ?1 AND availability != 'archived'",
            [claim.member_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let room_pending: usize = transaction.query_row(
            "SELECT COUNT(*) FROM member_inbox_items i
             JOIN brain_members m ON m.member_id = i.member_id
             WHERE m.room_id = ?1 AND i.state IN ('pending', 'leased', 'running')",
            [claim.room_id.as_str()],
            |row| row.get(0),
        )?;
        let (total_replies, member_replies): (usize, usize) = transaction.query_row(
            "SELECT
                 (SELECT COUNT(*) FROM room_events response
                  WHERE response.room_id = ?1
                    AND COALESCE(response.conversation_root_event_id, response.event_id) = ?2
                    AND response.kind = 'member_message'
                    AND response.invalidated_at IS NULL),
                 (SELECT COUNT(*) FROM room_events response
                  WHERE response.room_id = ?1
                    AND COALESCE(response.conversation_root_event_id, response.event_id) = ?2
                    AND response.kind = 'member_message'
                    AND response.sender_id = ?3
                    AND response.invalidated_at IS NULL)",
            params![
                claim.room_id,
                claim.conversation_root_event_id,
                claim.member_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if member_pending >= self.config.max_pending_items_per_member
            || room_pending >= self.config.max_pending_items_per_room
            || total_replies >= self.config.max_group_replies_per_conversation
            || member_replies >= self.config.max_group_replies_per_member
        {
            return Ok(());
        }
        let item = insert_inbox_item(
            transaction,
            &claim.member_id,
            &event_id,
            &claim.thread_key,
            RoomInputMode::Chat,
            InboxPurpose::Participation,
            &claim.conversation_root_event_id,
            member_version,
            &format!("participation:{event_id}:{}", claim.member_id),
            now,
        )?;
        transaction.execute(
            "UPDATE room_event_deliveries
             SET state = 'queued', inbox_item_id = ?1, decision_reason = NULL, updated_at = ?2
             WHERE event_id = ?3 AND member_id = ?4 AND state = 'deferred'",
            params![
                item.inbox_item_id,
                now.to_rfc3339(),
                event_id,
                claim.member_id,
            ],
        )?;
        Ok(())
    }

    pub fn complete_item(
        &self,
        claim: &ClaimedInboxItem,
        answer: &str,
    ) -> Result<Option<RoomEventView>> {
        if answer.trim().is_empty() {
            return Err(CollaborationError::EmptyMemberReply);
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cancel_requested: bool = transaction
            .query_row(
                "SELECT cancel_requested FROM member_inbox_items
                 WHERE inbox_item_id = ?1 AND run_id = ?2 AND state = 'running'",
                params![claim.inbox_item_id, claim.run_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| CollaborationError::RunNotActive(claim.run_id.clone()))?;
        let now = Utc::now();
        let event = if cancel_requested {
            transaction.execute(
                "UPDATE member_inbox_items
                 SET state = 'cancelled', completed_at = ?1, error = NULL,
                     version = version + 1
                 WHERE inbox_item_id = ?2 AND run_id = ?3",
                params![now.to_rfc3339(), claim.inbox_item_id, claim.run_id],
            )?;
            None
        } else {
            let event = self.append_member_reply_event(
                &transaction,
                claim.into(),
                answer,
                &format!("run-result:{}", claim.run_id),
                &now,
            )?;
            transaction.execute(
                "UPDATE member_inbox_items
                 SET state = 'completed', reply_event_id = ?1, completed_at = ?2,
                     error = NULL, version = version + 1
                 WHERE inbox_item_id = ?3 AND run_id = ?4 AND state = 'running'",
                params![
                    event.event_id,
                    now.to_rfc3339(),
                    claim.inbox_item_id,
                    claim.run_id
                ],
            )?;
            transaction.execute(
                "UPDATE room_event_deliveries
                 SET state = 'replied', decision_reason = 'direct_reply', updated_at = ?1
                 WHERE inbox_item_id = ?2",
                params![now.to_rfc3339(), claim.inbox_item_id],
            )?;
            Some(event)
        };
        settle_sleep_after_current(&transaction, &claim.member_id)?;
        enqueue_room_changed(
            &transaction,
            &claim.room_id,
            "inbox",
            &claim.inbox_item_id,
            &format!("inbox-completed:{}:{}", claim.inbox_item_id, claim.run_id),
        )?;
        transaction.commit()?;
        Ok(event)
    }

    #[allow(clippy::too_many_lines)]
    pub fn complete_participation_item(
        &self,
        claim: &ClaimedInboxItem,
        answer: Option<&str>,
    ) -> Result<ParticipationCompletion> {
        if claim.purpose != InboxPurpose::Participation {
            return Err(CollaborationError::Config(
                "只有群聊参与判断可以潜水完成".into(),
            ));
        }
        let answer = answer.map(str::trim);
        if answer.is_some_and(str::is_empty) {
            return Err(CollaborationError::EmptyMemberReply);
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cancel_requested: bool = transaction
            .query_row(
                "SELECT cancel_requested FROM member_inbox_items
                 WHERE inbox_item_id = ?1 AND run_id = ?2 AND state = 'running'",
                params![claim.inbox_item_id, claim.run_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| CollaborationError::RunNotActive(claim.run_id.clone()))?;
        let now = Utc::now();
        let (disposition, event, reason) = if cancel_requested {
            (ParticipationDisposition::Cancelled, None, "cancelled")
        } else if let Some(answer) = answer {
            let (parent_depth, total_replies, member_replies): (u32, usize, usize) = transaction
                .query_row(
                    "SELECT parent.debate_depth,
                            (SELECT COUNT(*) FROM room_events response
                             WHERE response.room_id = parent.room_id
                               AND COALESCE(response.conversation_root_event_id, response.event_id)
                                   = COALESCE(parent.conversation_root_event_id, parent.event_id)
                               AND response.kind = 'member_message'
                               AND response.invalidated_at IS NULL),
                            (SELECT COUNT(*) FROM room_events response
                             WHERE response.room_id = parent.room_id
                               AND COALESCE(response.conversation_root_event_id, response.event_id)
                                   = COALESCE(parent.conversation_root_event_id, parent.event_id)
                               AND response.kind = 'member_message'
                               AND response.sender_id = ?3
                               AND response.invalidated_at IS NULL)
                     FROM room_events parent
                     WHERE parent.room_id = ?1 AND parent.event_id = ?2
                       AND parent.invalidated_at IS NULL",
                    params![claim.room_id, claim.response_to_event_id, claim.member_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            let within_limits = parent_depth < self.config.max_debate_depth
                && total_replies < self.config.max_group_replies_per_conversation
                && member_replies < self.config.max_group_replies_per_member;
            if within_limits {
                let event = self.append_member_reply_event(
                    &transaction,
                    claim.into(),
                    answer,
                    &format!("run-result:{}", claim.run_id),
                    &now,
                )?;
                (ParticipationDisposition::Replied, Some(event), "replied")
            } else {
                (ParticipationDisposition::Suppressed, None, "debate_limit")
            }
        } else {
            (ParticipationDisposition::Silent, None, "not_relevant")
        };
        let state = if disposition == ParticipationDisposition::Cancelled {
            "cancelled"
        } else {
            "completed"
        };
        transaction.execute(
            "UPDATE member_inbox_items
             SET state = ?1, reply_event_id = ?2, completed_at = ?3,
                 lease_expires_at = NULL, error = NULL, version = version + 1
             WHERE inbox_item_id = ?4 AND run_id = ?5 AND state = 'running'",
            params![
                state,
                event.as_ref().map(|value| value.event_id.as_str()),
                now.to_rfc3339(),
                claim.inbox_item_id,
                claim.run_id,
            ],
        )?;
        transaction.execute(
            "UPDATE room_event_deliveries
             SET state = ?1, decision_reason = ?2, updated_at = ?3
             WHERE member_id = ?4
               AND event_id IN (
                   SELECT delivered.event_id FROM room_events delivered
                   WHERE delivered.room_id = ?5
                     AND COALESCE(delivered.conversation_root_event_id, delivered.event_id) = ?6
                     AND delivered.sequence <= ?7
               )
               AND state IN ('queued', 'running', 'deferred')",
            params![
                match disposition {
                    ParticipationDisposition::Replied => DeliveryState::Replied.as_db(),
                    ParticipationDisposition::Silent => DeliveryState::Silent.as_db(),
                    ParticipationDisposition::Suppressed | ParticipationDisposition::Cancelled =>
                        DeliveryState::Suppressed.as_db(),
                },
                reason,
                now.to_rfc3339(),
                claim.member_id,
                claim.room_id,
                claim.conversation_root_event_id,
                claim.context_through_seq,
            ],
        )?;
        self.queue_latest_deferred_participation(&transaction, claim, &now)?;
        settle_sleep_after_current(&transaction, &claim.member_id)?;
        enqueue_room_changed(
            &transaction,
            &claim.room_id,
            "inbox",
            &claim.inbox_item_id,
            &format!(
                "participation-completed:{}:{}",
                claim.inbox_item_id, claim.run_id
            ),
        )?;
        transaction.commit()?;
        Ok(ParticipationCompletion { disposition, event })
    }

    pub fn reconcile_claim_result(
        &self,
        claim: &ClaimedInboxItem,
        durable_run_id: &str,
        answer: Option<&str>,
    ) -> Result<ParticipationCompletion> {
        if claim.purpose == InboxPurpose::Direct && answer.is_none() {
            return Err(CollaborationError::EmptyMemberReply);
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_state, current_run_id, current_version, cancel_requested) = transaction
            .query_row(
                "SELECT state, run_id, version, cancel_requested
                 FROM member_inbox_items WHERE inbox_item_id = ?1",
                [claim.inbox_item_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, u64>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| CollaborationError::RunNotActive(durable_run_id.into()))?;
        if matches!(current_state.as_str(), "completed" | "cancelled") {
            transaction.commit()?;
            return Ok(ParticipationCompletion {
                disposition: if current_state == "cancelled" {
                    ParticipationDisposition::Cancelled
                } else if answer.is_none() {
                    ParticipationDisposition::Silent
                } else {
                    ParticipationDisposition::Suppressed
                },
                event: None,
            });
        }
        if !matches!(
            current_state.as_str(),
            "pending" | "leased" | "running" | "failed"
        ) {
            return Err(CollaborationError::RunNotActive(durable_run_id.into()));
        }
        if current_state == "running" && current_run_id.as_deref() != Some(durable_run_id) {
            return Err(CollaborationError::RunNotActive(durable_run_id.into()));
        }
        let transition_version = if current_version == claim.version {
            current_version
        } else if current_state == "running"
            && current_run_id.as_deref() == Some(durable_run_id)
            && claim.run_id == durable_run_id
            && cancel_requested
        {
            // interrupt 只会提升同一运行的版本；投影仍以事务内当前版本做 CAS。
            current_version
        } else {
            return Err(CollaborationError::VersionConflict {
                entity: "member inbox run",
                id: durable_run_id.into(),
                expected: claim.version,
                actual: current_version,
            });
        };
        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'running', run_id = ?1, lease_expires_at = NULL,
                 version = version + 1
             WHERE inbox_item_id = ?2 AND state = ?3 AND version = ?4",
            params![
                durable_run_id,
                claim.inbox_item_id,
                current_state,
                transition_version
            ],
        )?;
        if updated != 1 {
            return Err(CollaborationError::VersionConflict {
                entity: "member inbox run",
                id: durable_run_id.into(),
                expected: transition_version,
                actual: transaction
                    .query_row(
                        "SELECT version FROM member_inbox_items WHERE inbox_item_id = ?1",
                        [claim.inbox_item_id.as_str()],
                        |row| row.get::<_, u64>(0),
                    )
                    .optional()?
                    .unwrap_or(0),
            });
        }
        transaction.execute(
            "UPDATE room_event_deliveries
             SET state = 'running', updated_at = ?1
             WHERE inbox_item_id = ?2 AND state IN ('queued', 'deferred', 'running')",
            params![Utc::now().to_rfc3339(), claim.inbox_item_id],
        )?;
        transaction.commit()?;

        let mut durable_claim = claim.clone();
        durable_claim.run_id = durable_run_id.into();
        durable_claim.version = transition_version + 1;
        let completion = if durable_claim.purpose == InboxPurpose::Participation {
            self.complete_participation_item(&durable_claim, answer)
        } else {
            self.complete_item(
                &durable_claim,
                answer.ok_or(CollaborationError::EmptyMemberReply)?,
            )
            .map(|event| ParticipationCompletion {
                disposition: if event.is_some() {
                    ParticipationDisposition::Replied
                } else {
                    ParticipationDisposition::Cancelled
                },
                event,
            })
        };
        match completion {
            Ok(completion) => Ok(completion),
            Err(error) => {
                let rollback = if current_state == "failed" {
                    self.restore_failed_reconciliation(&durable_claim, current_run_id.as_deref())
                } else {
                    self.release_active_for_retry(&durable_claim)
                };
                if let Err(release_error) = rollback {
                    return Err(CollaborationError::Config(format!(
                        "投影持久结果失败且无法释放运行中 Claim: {error}; {release_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn reconcile_completed_item(
        &self,
        inbox_item_id: &str,
        run_id: &str,
        answer: &str,
    ) -> Result<Option<RoomEventView>> {
        if answer.trim().is_empty() {
            return Err(CollaborationError::EmptyMemberReply);
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let item = transaction
            .query_row(
                "SELECT e.room_id, i.member_id, m.display_name, i.cancel_requested,
                        i.reply_event_id, i.state, e.execution_working_directory,
                        CASE WHEN i.purpose = 'participation' THEN COALESCE((
                            SELECT delivered.event_id
                            FROM room_event_deliveries delivery
                            JOIN room_events delivered ON delivered.event_id = delivery.event_id
                            WHERE delivery.member_id = i.member_id
                              AND COALESCE(delivered.conversation_root_event_id, delivered.event_id)
                                  = COALESCE(i.conversation_root_event_id, i.source_event_id)
                              AND delivered.sequence <= CASE WHEN i.context_through_seq > 0
                                  THEN i.context_through_seq ELSE room.latest_event_seq END
                              AND delivered.invalidated_at IS NULL
                            ORDER BY delivered.sequence DESC LIMIT 1
                        ), i.source_event_id) ELSE i.source_event_id END
                 FROM member_inbox_items i
                 JOIN room_events e ON e.event_id = i.source_event_id
                 JOIN brain_members m ON m.member_id = i.member_id
                 JOIN collaboration_rooms room ON room.room_id = e.room_id
                 WHERE i.inbox_item_id = ?1 AND i.run_id = ?2
                   AND e.invalidated_at IS NULL",
                params![inbox_item_id, run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            room_id,
            member_id,
            member_name,
            cancel_requested,
            reply_event_id,
            state,
            execution_working_directory,
            response_to_event_id,
        )) = item
        else {
            return Err(CollaborationError::RunNotActive(run_id.into()));
        };
        let execution_working_directory =
            required_execution_working_directory(execution_working_directory.unwrap_or_default())?;
        if reply_event_id.is_some() || state == "completed" {
            transaction.commit()?;
            return Ok(None);
        }
        if state == "cancelled" {
            transaction.commit()?;
            return Ok(None);
        }
        if cancel_requested {
            let updated = transaction.execute(
                "UPDATE member_inbox_items
                 SET state = 'cancelled', completed_at = ?1, lease_expires_at = NULL,
                     version = version + 1
                 WHERE inbox_item_id = ?2 AND state NOT IN ('completed', 'cancelled')",
                params![Utc::now().to_rfc3339(), inbox_item_id],
            )?;
            if updated == 1 {
                settle_sleep_after_current(&transaction, &member_id)?;
                enqueue_room_changed(
                    &transaction,
                    &room_id,
                    "inbox",
                    inbox_item_id,
                    &format!("inbox-reconciled-cancelled:{inbox_item_id}:{run_id}"),
                )?;
            }
            transaction.commit()?;
            return Ok(None);
        }

        let idempotency_key = format!("run-result:{run_id}");
        let event =
            if let Some(event) = event_by_idempotency(&transaction, &room_id, &idempotency_key)? {
                event
            } else {
                let now = Utc::now();
                self.append_member_reply_event(
                    &transaction,
                    MemberReplyEventContext {
                        room_id: &room_id,
                        member_id: &member_id,
                        member_name: &member_name,
                        run_id,
                        response_to_event_id: &response_to_event_id,
                        execution_working_directory: &execution_working_directory,
                    },
                    answer,
                    &idempotency_key,
                    &now,
                )?
            };
        transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'completed', run_id = ?1, reply_event_id = ?2,
                 completed_at = ?3, lease_expires_at = NULL, error = NULL,
                 version = version + 1
             WHERE inbox_item_id = ?4 AND state <> 'cancelled'",
            params![
                run_id,
                event.event_id,
                Utc::now().to_rfc3339(),
                inbox_item_id,
            ],
        )?;
        settle_sleep_after_current(&transaction, &member_id)?;
        enqueue_room_changed(
            &transaction,
            &room_id,
            "inbox",
            inbox_item_id,
            &format!("inbox-reconciled:{inbox_item_id}:{run_id}"),
        )?;
        transaction.commit()?;
        Ok(Some(event))
    }

    pub fn fail_item(&self, claim: &ClaimedInboxItem, error: &str) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cancel_requested: bool = transaction
            .query_row(
                "SELECT cancel_requested FROM member_inbox_items
                 WHERE inbox_item_id = ?1 AND run_id = ?2",
                params![claim.inbox_item_id, claim.run_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let state = if cancel_requested {
            "cancelled"
        } else {
            "failed"
        };
        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET state = ?1, error = ?2, completed_at = ?3, version = version + 1
             WHERE inbox_item_id = ?4 AND run_id = ?5 AND state = 'running'",
            params![
                state,
                error,
                Utc::now().to_rfc3339(),
                claim.inbox_item_id,
                claim.run_id,
            ],
        )?;
        if updated != 1 {
            return Err(CollaborationError::RunNotActive(claim.run_id.clone()));
        }
        settle_sleep_after_current(&transaction, &claim.member_id)?;
        enqueue_room_changed(
            &transaction,
            &claim.room_id,
            "inbox",
            &claim.inbox_item_id,
            &format!("inbox-failed:{}:{}", claim.inbox_item_id, claim.run_id),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn request_interrupt(&self, room_id: &str, member_id: &str, run_id: &str) -> Result<()> {
        let connection = self.connect()?;
        let version = connection
            .query_row(
                "SELECT version FROM member_inbox_items
                 WHERE run_id = ?1 AND member_id = ?2 AND state = 'running'
                   AND member_id IN (SELECT member_id FROM brain_members WHERE room_id = ?3)",
                params![run_id, member_id, room_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .ok_or_else(|| CollaborationError::RunNotActive(run_id.into()))?;
        drop(connection);
        self.request_interrupt_checked(
            &CollaborationActor::local(),
            room_id,
            member_id,
            run_id,
            version,
        )
    }

    pub fn request_interrupt_checked(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        member_id: &str,
        run_id: &str,
        expected_version: u64,
    ) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_capability(
            &transaction,
            actor,
            room_id,
            RoomCapability::InterruptAnyRun,
        )?;
        let actual_version = transaction
            .query_row(
                "SELECT version FROM member_inbox_items
                 WHERE run_id = ?1 AND member_id = ?2 AND state = 'running'
                   AND member_id IN (SELECT member_id FROM brain_members WHERE room_id = ?3)",
                params![run_id, member_id, room_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .ok_or_else(|| CollaborationError::RunNotActive(run_id.into()))?;
        ensure_version("member inbox run", run_id, expected_version, actual_version)?;
        let updated = transaction.execute(
            "UPDATE member_inbox_items
             SET cancel_requested = 1, version = version + 1
             WHERE run_id = ?1 AND member_id = ?2 AND state = 'running'
               AND version = ?3
               AND member_id IN (SELECT member_id FROM brain_members WHERE room_id = ?4)",
            params![run_id, member_id, expected_version, room_id],
        )?;
        if updated == 0 {
            return Err(CollaborationError::VersionConflict {
                entity: "member inbox run",
                id: run_id.into(),
                expected: expected_version,
                actual: transaction
                    .query_row(
                        "SELECT version FROM member_inbox_items WHERE run_id = ?1",
                        [run_id],
                        |row| row.get::<_, u64>(0),
                    )
                    .optional()?
                    .unwrap_or(0),
            });
        }
        enqueue_room_changed(
            &transaction,
            room_id,
            "instance_run",
            run_id,
            &format!("run-interrupt:{run_id}"),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn interrupt_requested(&self, claim: &ClaimedInboxItem) -> Result<bool> {
        let connection = self.connect()?;
        let requested = connection
            .query_row(
                "SELECT cancel_requested FROM member_inbox_items
                 WHERE inbox_item_id = ?1 AND run_id = ?2 AND state = 'running'",
                params![claim.inbox_item_id, claim.run_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        Ok(requested)
    }

    pub fn recover_inflight(&self) -> Result<usize> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT DISTINCT e.room_id
             FROM member_inbox_items i
             JOIN room_events e ON e.event_id = i.source_event_id
             WHERE i.state IN ('leased', 'running')",
        )?;
        let recovered_rooms = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        let recovered = transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'pending', run_id = NULL, cancel_requested = 0,
                 started_at = NULL, lease_expires_at = NULL, version = version + 1
             WHERE state IN ('leased', 'running')",
            [],
        )?;
        transaction.execute(
            "UPDATE brain_members
             SET availability = 'sleeping', version = version + 1
             WHERE availability = 'sleep_after_current'
               AND NOT EXISTS (
                   SELECT 1 FROM member_inbox_items i
                   WHERE i.member_id = brain_members.member_id
                     AND i.state IN ('leased', 'running')
               )",
            [],
        )?;
        for room_id in recovered_rooms {
            enqueue_room_changed(
                &transaction,
                &room_id,
                "recovery",
                &room_id,
                &format!("inbox-recovery:{room_id}:{}", Uuid::new_v4()),
            )?;
        }
        transaction.commit()?;
        Ok(recovered)
    }

    pub fn upsert_membership(
        &self,
        actor: &CollaborationActor,
        room_id: &str,
        principal_id: &str,
        role: RoomRole,
        capabilities: &[RoomCapability],
        expected_version: Option<u64>,
    ) -> Result<RoomMembershipView> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_exists(&transaction, room_id)?;
        require_capability(
            &transaction,
            actor,
            room_id,
            RoomCapability::ManageMembership,
        )?;
        let current_version = transaction
            .query_row(
                "SELECT version FROM room_principal_memberships
                 WHERE room_id = ?1 AND principal_id = ?2",
                params![room_id, principal_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?;
        match (current_version, expected_version) {
            (Some(actual), Some(expected)) => {
                ensure_version("room membership", principal_id, expected, actual)?;
            }
            (Some(actual), None) => {
                return Err(CollaborationError::VersionConflict {
                    entity: "room membership",
                    id: principal_id.into(),
                    expected: 0,
                    actual,
                });
            }
            (None, Some(expected)) if expected != 0 => {
                return Err(CollaborationError::VersionConflict {
                    entity: "room membership",
                    id: principal_id.into(),
                    expected,
                    actual: 0,
                });
            }
            _ => {}
        }
        let now = Utc::now().to_rfc3339();
        let serialized = serialize_capabilities(capabilities)?;
        if let Some(version) = current_version {
            transaction.execute(
                "UPDATE room_principal_memberships
                 SET role = ?1, capabilities_json = ?2,
                     capability_version = capability_version + 1,
                     version = version + 1, updated_at = ?3
                 WHERE room_id = ?4 AND principal_id = ?5 AND version = ?6",
                params![
                    role.as_db(),
                    serialized,
                    now,
                    room_id,
                    principal_id,
                    version,
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO room_principal_memberships(
                     room_id, principal_id, role, capabilities_json,
                     capability_version, version, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, ?5)",
                params![room_id, principal_id, role.as_db(), serialized, now],
            )?;
        }
        enqueue_room_changed(
            &transaction,
            room_id,
            "membership",
            principal_id,
            &format!(
                "membership:{room_id}:{principal_id}:{}",
                current_version.unwrap_or(0) + 1
            ),
        )?;
        let membership = membership_from_connection(&transaction, room_id, principal_id)?;
        transaction.commit()?;
        Ok(membership)
    }

    pub fn events_after(
        &self,
        room_id: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<RoomEventView>> {
        let connection = self.connect()?;
        require_capability(
            &connection,
            &CollaborationActor::local(),
            room_id,
            RoomCapability::RoomRead,
        )?;
        events_after_from_connection(&connection, room_id, after_sequence, limit)
    }

    pub fn events_before(
        &self,
        room_id: &str,
        before_sequence: u64,
        limit: usize,
    ) -> Result<RoomEventPage> {
        let connection = self.connect()?;
        room_from_connection(&connection, room_id)?;
        require_capability(
            &connection,
            &CollaborationActor::local(),
            room_id,
            RoomCapability::RoomRead,
        )?;
        events_before_from_connection(&connection, room_id, before_sequence, limit)
    }

    /// 返回不晚于指定上下文边界的公共群消息，按发送顺序排列。
    pub fn events_through(
        &self,
        room_id: &str,
        through_sequence: u64,
        limit: usize,
    ) -> Result<Vec<RoomEventView>> {
        let connection = self.connect()?;
        require_capability(
            &connection,
            &CollaborationActor::local(),
            room_id,
            RoomCapability::RoomRead,
        )?;
        events_through_from_connection(&connection, room_id, through_sequence, limit)
    }

    pub fn retry_last_user_event(&self, room_id: &str, event_id: &str) -> Result<RoomSnapshot> {
        self.retry_last_user_event_with_runs(room_id, event_id)
            .map(|(snapshot, _)| snapshot)
    }

    pub fn retry_last_user_event_with_runs(
        &self,
        room_id: &str,
        event_id: &str,
    ) -> Result<(RoomSnapshot, Vec<(String, String)>)> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_exists(&transaction, room_id)?;
        let retained = transaction
            .query_row(
                "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                        kind, content, run_id, parent_event_id,
                        COALESCE(conversation_root_event_id, event_id), debate_depth,
                        group_enabled, conversation_mode, created_at
                 FROM room_events
                 WHERE room_id = ?1 AND event_id = ?2 AND invalidated_at IS NULL",
                params![room_id, event_id],
                event_from_row_without_recipients,
            )
            .optional()?
            .ok_or_else(|| CollaborationError::Config("待重试的用户消息不存在或已经失效".into()))?;
        if retained.sender_kind != "user" {
            return Err(CollaborationError::Config(
                "只能重试最后一条用户消息".into(),
            ));
        }
        let last_user_event_id: String = transaction.query_row(
            "SELECT event_id FROM room_events
             WHERE room_id = ?1 AND sender_kind = 'user' AND invalidated_at IS NULL
             ORDER BY sequence DESC LIMIT 1",
            [room_id],
            |row| row.get(0),
        )?;
        if last_user_event_id != retained.event_id {
            return Err(CollaborationError::Config(
                "只能重试最后一条用户消息".into(),
            ));
        }

        let replaced_runs = {
            let mut statement = transaction.prepare(
                "SELECT i.member_id, i.run_id
                 FROM member_inbox_items i
                 JOIN brain_members m ON m.member_id = i.member_id
                 WHERE m.room_id = ?1
                   AND i.state IN ('leased', 'running')
                   AND i.run_id IS NOT NULL",
            )?;
            let runs = statement
                .query_map([room_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            runs
        };
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        transaction.execute(
            "UPDATE room_events
             SET invalidated_at = ?1
             WHERE room_id = ?2 AND sequence > ?3 AND invalidated_at IS NULL",
            params![now_text, room_id, retained.sequence],
        )?;
        transaction.execute(
            "UPDATE member_inbox_items
             SET state = 'cancelled', cancel_requested = 1,
                 completed_at = COALESCE(completed_at, ?1),
                 lease_expires_at = NULL, version = version + 1
             WHERE source_event_id IN (
                 SELECT event_id FROM room_events
                 WHERE room_id = ?2 AND invalidated_at IS NOT NULL
             ) AND state NOT IN ('completed', 'cancelled')",
            params![now_text, room_id],
        )?;

        let retry_key = format!("retry-{}", Uuid::new_v4());
        let retained_inbox_ids = {
            let mut statement = transaction.prepare(
                "SELECT inbox_item_id
                 FROM member_inbox_items
                 WHERE source_event_id = ?1
                 ORDER BY created_at, inbox_item_id",
            )?;
            let inbox_ids = statement
                .query_map([retained.event_id.as_str()], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            inbox_ids
        };
        for old_inbox_item_id in retained_inbox_ids {
            let new_inbox_item_id = format!("inbox-{}", Uuid::new_v4());
            let new_task_run_id = format!("task-{new_inbox_item_id}");
            transaction.execute(
                "UPDATE room_event_deliveries
                 SET inbox_item_id = ?1, state = 'queued',
                     decision_reason = NULL, updated_at = ?2
                 WHERE inbox_item_id = ?3",
                params![new_inbox_item_id, now_text, old_inbox_item_id],
            )?;
            transaction.execute(
                "UPDATE member_inbox_items
                 SET inbox_item_id = ?1, state = 'pending', task_run_id = ?2,
                     run_id = NULL, idempotency_key = ?3 || ':' || member_id,
                     cancel_requested = 0, reply_event_id = NULL, error = NULL,
                     started_at = NULL, completed_at = NULL, lease_expires_at = NULL,
                     version = version + 1
                 WHERE inbox_item_id = ?4",
                params![
                    new_inbox_item_id,
                    new_task_run_id,
                    retry_key,
                    old_inbox_item_id
                ],
            )?;
        }
        transaction.execute(
            "UPDATE room_event_deliveries
             SET state = 'queued', decision_reason = NULL, updated_at = ?1
             WHERE event_id = ?2 AND inbox_item_id IS NOT NULL",
            params![now_text, retained.event_id],
        )?;
        transaction.execute(
            "UPDATE collaboration_rooms SET version = version + 1 WHERE room_id = ?1",
            [room_id],
        )?;
        enqueue_room_changed(
            &transaction,
            room_id,
            "room_event",
            &retained.event_id,
            &format!("room-retry:{room_id}:{retry_key}"),
        )?;
        transaction.commit()?;
        Ok((self.snapshot(room_id)?, replaced_runs))
    }

    pub fn advance_member_cursor(
        &self,
        member_id: &str,
        cursor_kind: MemberCursorKind,
        expected_version: u64,
        event_sequence: u64,
    ) -> Result<MemberCursorView> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM brain_members WHERE member_id = ?1)",
            [member_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(CollaborationError::MemberNotFound(member_id.into()));
        }
        let current = transaction
            .query_row(
                "SELECT event_sequence, version FROM member_cursors
                 WHERE member_id = ?1 AND cursor_kind = ?2",
                params![member_id, cursor_kind.as_db()],
                |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
            )
            .optional()?;
        let (current_sequence, actual_version) = current.unwrap_or((0, 0));
        ensure_version(
            "member cursor",
            &format!("{member_id}:{}", cursor_kind.as_db()),
            expected_version,
            actual_version,
        )?;
        if event_sequence < current_sequence {
            return Err(CollaborationError::Config(format!(
                "成员 cursor 不能从 {current_sequence} 回退到 {event_sequence}"
            )));
        }
        let now = Utc::now().to_rfc3339();
        if actual_version == 0 {
            transaction.execute(
                "INSERT INTO member_cursors(
                     member_id, cursor_kind, event_sequence, version, updated_at
                 ) VALUES (?1, ?2, ?3, 1, ?4)",
                params![member_id, cursor_kind.as_db(), event_sequence, now],
            )?;
        } else {
            transaction.execute(
                "UPDATE member_cursors
                 SET event_sequence = ?1, version = version + 1, updated_at = ?2
                 WHERE member_id = ?3 AND cursor_kind = ?4 AND version = ?5",
                params![
                    event_sequence,
                    now,
                    member_id,
                    cursor_kind.as_db(),
                    expected_version,
                ],
            )?;
        }
        let cursor = member_cursor_from_connection(&transaction, member_id, cursor_kind)?;
        transaction.commit()?;
        Ok(cursor)
    }

    pub fn member_cursors(&self, member_id: &str) -> Result<Vec<MemberCursorView>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT member_id, cursor_kind, event_sequence, version, updated_at
             FROM member_cursors WHERE member_id = ?1 ORDER BY cursor_kind",
        )?;
        let cursors = statement
            .query_map([member_id], member_cursor_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(cursors)
    }

    pub fn pending_outbox_events(&self, limit: usize) -> Result<Vec<CollaborationOutboxEvent>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT outbox_event_id, room_id, event_kind, aggregate_kind,
                    aggregate_id, version, created_at
             FROM runtime_outbox_events
             WHERE state = 'pending' ORDER BY created_at, rowid LIMIT ?1",
        )?;
        let events = statement
            .query_map([limit], outbox_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(events)
    }

    pub fn mark_outbox_published(
        &self,
        outbox_event_id: &str,
        expected_version: u64,
    ) -> Result<()> {
        let connection = self.connect()?;
        let updated = connection.execute(
            "UPDATE runtime_outbox_events
             SET state = 'published', version = version + 1, published_at = ?1
             WHERE outbox_event_id = ?2 AND state = 'pending' AND version = ?3",
            params![Utc::now().to_rfc3339(), outbox_event_id, expected_version],
        )?;
        if updated != 1 {
            let actual = connection
                .query_row(
                    "SELECT version FROM runtime_outbox_events WHERE outbox_event_id = ?1",
                    [outbox_event_id],
                    |row| row.get::<_, u64>(0),
                )
                .optional()?
                .unwrap_or(0);
            return Err(CollaborationError::VersionConflict {
                entity: "runtime outbox event",
                id: outbox_event_id.into(),
                expected: expected_version,
                actual,
            });
        }
        Ok(())
    }

    pub fn snapshot(&self, room_id: &str) -> Result<RoomSnapshot> {
        self.snapshot_as(&CollaborationActor::local(), room_id)
    }

    pub fn snapshot_as(&self, actor: &CollaborationActor, room_id: &str) -> Result<RoomSnapshot> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        require_capability(&transaction, actor, room_id, RoomCapability::RoomRead)?;
        let room = room_from_connection(&transaction, room_id)?;
        let members = members_from_connection(&transaction, room_id)?;
        let mut events = events_from_connection(&transaction, room_id, 301)?;
        let has_earlier_events = events.len() > 300;
        if has_earlier_events {
            events.remove(0);
        }
        let inbox = inbox_from_connection(&transaction, room_id, 200)?;
        let deliveries = deliveries_from_connection(&transaction, room_id, 1_000)?;
        let member_templates = member_templates_from_connection(&transaction)?;
        let snapshot = RoomSnapshot {
            room,
            members,
            events,
            has_earlier_events,
            inbox,
            deliveries,
            member_templates,
            model_policies: self.config.allowed_model_policies.clone(),
            model_policy_details: Vec::new(),
            reasoning_depths: self.config.allowed_reasoning_depths.clone(),
            max_members: self.config.max_members_per_room,
            max_workers: self.config.max_workers,
        };
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn member(&self, room_id: &str, member_id: &str) -> Result<BrainMemberView> {
        self.snapshot(room_id)?
            .members
            .into_iter()
            .find(|member| member.member_id == member_id)
            .ok_or_else(|| CollaborationError::MemberNotFound(member_id.into()))
    }
}

#[derive(Debug, Clone, Copy)]
enum LifecycleCommand {
    Wake,
    Sleep,
    Archive,
    Restore,
}

fn serialize_capabilities(capabilities: &[RoomCapability]) -> Result<String> {
    serde_json::to_string(capabilities)
        .map_err(|error| CollaborationError::Config(error.to_string()))
}

fn deserialize_capabilities(value: &str) -> Result<Vec<RoomCapability>> {
    serde_json::from_str(value).map_err(|error| CollaborationError::Config(error.to_string()))
}

fn ensure_local_owner_membership(
    transaction: &Transaction<'_>,
    room_id: &str,
    now: &DateTime<Utc>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO room_principal_memberships(
             room_id, principal_id, role, capabilities_json, capability_version,
             version, created_at, updated_at
         ) VALUES (?1, ?2, 'owner', ?3, 2, 1, ?4, ?4)
         ON CONFLICT(room_id, principal_id) DO NOTHING",
        params![
            room_id,
            LOCAL_PRINCIPAL_ID,
            serialize_capabilities(&RoomCapability::owner_capabilities())?,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn require_capability(
    connection: &Connection,
    actor: &CollaborationActor,
    room_id: &str,
    capability: RoomCapability,
) -> Result<()> {
    let capabilities = connection
        .query_row(
            "SELECT capabilities_json FROM room_principal_memberships
             WHERE room_id = ?1 AND principal_id = ?2",
            params![room_id, actor.principal_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|serialized| deserialize_capabilities(&serialized))
        .transpose()?
        .unwrap_or_default();
    if capabilities.contains(&capability) {
        Ok(())
    } else {
        Err(CollaborationError::CapabilityDenied {
            principal_id: actor.principal_id.clone(),
            room_id: room_id.into(),
            capability,
        })
    }
}

fn ensure_version(entity: &'static str, id: &str, expected: u64, actual: u64) -> Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(CollaborationError::VersionConflict {
            entity,
            id: id.into(),
            expected,
            actual,
        })
    }
}

fn normalize_active_member_display_names(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT room_id, member_id, display_name
         FROM brain_members
         WHERE availability != 'archived'
         ORDER BY room_id, created_at, member_id",
    )?;
    let members = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut names_by_room = std::collections::HashMap::<String, Vec<(String, String)>>::new();
    for (room_id, member_id, display_name) in members {
        names_by_room
            .entry(room_id)
            .or_default()
            .push((member_id, display_name));
    }
    for (room_id, members) in names_by_room {
        let mut used = members
            .iter()
            .map(|(_, display_name)| display_name.to_lowercase())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        for (member_id, display_name) in members {
            if seen.insert(display_name.to_lowercase()) {
                continue;
            }
            let mut suffix = 2_u32;
            let replacement = loop {
                let candidate = format!("{display_name} ({suffix})");
                if used.insert(candidate.to_lowercase()) {
                    break candidate;
                }
                suffix += 1;
            };
            connection.execute(
                "UPDATE brain_members SET display_name = ?1 WHERE room_id = ?2 AND member_id = ?3",
                params![replacement, room_id, member_id],
            )?;
        }
    }
    Ok(())
}

fn ensure_unique_active_member_display_name(
    connection: &Connection,
    room_id: &str,
    display_name: &str,
    excluding_member_id: Option<&str>,
) -> Result<()> {
    let duplicate: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM brain_members
             WHERE room_id = ?1
               AND availability != 'archived'
               AND display_name = ?2 COLLATE NOCASE
               AND (?3 IS NULL OR member_id != ?3)
         )",
        params![room_id, display_name, excluding_member_id],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(CollaborationError::DuplicateMemberName(display_name.into()));
    }
    Ok(())
}

fn member_version(connection: &Connection, room_id: &str, member_id: &str) -> Result<u64> {
    connection
        .query_row(
            "SELECT version FROM brain_members WHERE room_id = ?1 AND member_id = ?2",
            params![room_id, member_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| CollaborationError::MemberNotFound(member_id.into()))
}

fn validate_template_policy(
    template: &MemberTemplateView,
    model_policy: &str,
    reasoning_depth: &str,
) -> Result<()> {
    if !template
        .allowed_model_policies
        .iter()
        .any(|allowed| allowed == model_policy)
    {
        return Err(CollaborationError::ModelPolicyNotAllowed(
            model_policy.into(),
        ));
    }
    if !template
        .allowed_reasoning_depths
        .iter()
        .any(|allowed| allowed == reasoning_depth)
    {
        return Err(CollaborationError::ReasoningDepthNotAllowed(
            reasoning_depth.into(),
        ));
    }
    Ok(())
}

fn member_template_from_connection(
    connection: &Connection,
    template_id: &str,
) -> Result<MemberTemplateView> {
    let raw = connection
        .query_row(
            "SELECT template_id, display_name, profile_id, default_model_policy,
                    default_reasoning_depth, allowed_model_policies_json,
                    allowed_reasoning_depths_json, version
             FROM member_templates WHERE template_id = ?1",
            [template_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, u64>(7)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| CollaborationError::MemberTemplateNotFound(template_id.into()))?;
    Ok(MemberTemplateView {
        template_id: raw.0,
        display_name: raw.1,
        profile_id: raw.2,
        default_model_policy: raw.3,
        default_reasoning_depth: raw.4,
        allowed_model_policies: serde_json::from_str(&raw.5)
            .map_err(|error| CollaborationError::Config(error.to_string()))?,
        allowed_reasoning_depths: serde_json::from_str(&raw.6)
            .map_err(|error| CollaborationError::Config(error.to_string()))?,
        version: raw.7,
    })
}

fn member_templates_from_connection(connection: &Connection) -> Result<Vec<MemberTemplateView>> {
    let mut statement =
        connection.prepare("SELECT template_id FROM member_templates ORDER BY template_id")?;
    let template_ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    template_ids
        .iter()
        .map(|template_id| member_template_from_connection(connection, template_id))
        .collect()
}

fn membership_from_connection(
    connection: &Connection,
    room_id: &str,
    principal_id: &str,
) -> Result<RoomMembershipView> {
    let raw = connection
        .query_row(
            "SELECT role, capabilities_json, capability_version, version
             FROM room_principal_memberships
             WHERE room_id = ?1 AND principal_id = ?2",
            params![room_id, principal_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| CollaborationError::CapabilityDenied {
            principal_id: principal_id.into(),
            room_id: room_id.into(),
            capability: RoomCapability::RoomRead,
        })?;
    Ok(RoomMembershipView {
        room_id: room_id.into(),
        principal_id: principal_id.into(),
        role: RoomRole::from_db(&raw.0),
        capabilities: deserialize_capabilities(&raw.1)?,
        capability_version: raw.2,
        version: raw.3,
    })
}

fn enqueue_room_changed(
    transaction: &Transaction<'_>,
    room_id: &str,
    aggregate_kind: &str,
    aggregate_id: &str,
    idempotency_key: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO runtime_outbox_events(
             outbox_event_id, room_id, event_kind, aggregate_kind,
             aggregate_id, idempotency_key, state, version, created_at
         ) VALUES (?1, ?2, 'room_changed', ?3, ?4, ?5, 'pending', 1, ?6)
         ON CONFLICT(idempotency_key) DO NOTHING",
        params![
            format!("outbox-{}", Uuid::new_v4()),
            room_id,
            aggregate_kind,
            aggregate_id,
            idempotency_key,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn ensure_room_exists(transaction: &Transaction<'_>, room_id: &str) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM collaboration_rooms WHERE room_id = ?1)",
        [room_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(CollaborationError::RoomNotFound(room_id.into()))
    }
}

fn claimed_inbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClaimCandidate> {
    Ok(ClaimCandidate {
        inbox_item_id: row.get("inbox_item_id")?,
        room_id: row.get("room_id")?,
        member_id: row.get("member_id")?,
        member_name: row.get("member_name")?,
        profile_id: row.get("profile_id")?,
        model_policy: row.get("model_policy")?,
        reasoning_depth: row.get("reasoning_depth")?,
        source_event_id: row.get("source_event_id")?,
        source_event_found: row.get("source_event_found")?,
        source_event_seq: row.get("source_event_seq")?,
        input: row.get("input")?,
        mode: row.get("mode")?,
        task_run_id: row.get("task_run_id")?,
        version: row.get("version")?,
        thread_key: row.get("thread_key")?,
        purpose: row.get("purpose")?,
        conversation_root_event_id: row.get("conversation_root_event_id")?,
        context_through_seq: row.get("context_through_seq")?,
        response_to_event_id: row.get("response_to_event_id")?,
        group_enabled: row.get("group_enabled")?,
        execution_working_directory: row.get("execution_working_directory")?,
        parent_event_id: row.get("parent_event_id")?,
        reply_event_id: row.get("reply_event_id")?,
        reply_room_id: row.get("reply_room_id")?,
        reply_sequence: row.get("reply_sequence")?,
        reply_sender_kind: row.get("reply_sender_kind")?,
        reply_sender_id: row.get("reply_sender_id")?,
        reply_sender_name: row.get("reply_sender_name")?,
        reply_kind: row.get("reply_kind")?,
        reply_content: row.get("reply_content")?,
        reply_created_at: row.get("reply_created_at")?,
    })
}

fn claimed_inbox_from_candidate(
    candidate: &ClaimCandidate,
    run_id: String,
    version: u64,
) -> Result<ClaimedInboxItem> {
    if !candidate.source_event_found {
        return Err(CollaborationError::Config(format!(
            "Inbox 来源用户事件 {} 不存在",
            candidate.source_event_id
        )));
    }
    let missing_source_field = |field: &str| {
        CollaborationError::Config(format!(
            "Inbox 来源用户事件 {} 缺少{field}",
            candidate.source_event_id
        ))
    };
    let room_id = candidate
        .room_id
        .clone()
        .ok_or_else(|| missing_source_field("房间"))?;
    let source_event_seq = candidate
        .source_event_seq
        .ok_or_else(|| missing_source_field("事件序号"))?;
    let input = candidate
        .input
        .clone()
        .ok_or_else(|| missing_source_field("正文"))?;
    let context_through_seq = candidate
        .context_through_seq
        .ok_or_else(|| missing_source_field("上下文截止序号"))?;
    let group_enabled = candidate
        .group_enabled
        .ok_or_else(|| missing_source_field("群聊标记"))?;
    let execution_working_directory = required_execution_working_directory(
        candidate
            .execution_working_directory
            .clone()
            .unwrap_or_default(),
    )?;
    let reply_reference = reply_reference_from_candidate(candidate, &room_id)?;
    Ok(ClaimedInboxItem {
        inbox_item_id: candidate.inbox_item_id.clone(),
        room_id,
        member_id: candidate.member_id.clone(),
        member_name: candidate.member_name.clone(),
        profile_id: candidate.profile_id.clone(),
        model_policy: candidate.model_policy.clone(),
        reasoning_depth: candidate.reasoning_depth.clone(),
        source_event_id: candidate.source_event_id.clone(),
        source_event_seq,
        thread_key: candidate.thread_key.clone(),
        purpose: InboxPurpose::from_db(&candidate.purpose),
        conversation_root_event_id: candidate.conversation_root_event_id.clone(),
        context_through_seq,
        response_to_event_id: candidate.response_to_event_id.clone(),
        group_enabled,
        execution_working_directory,
        reply_reference,
        input,
        mode: if candidate.mode == "task" {
            RoomInputMode::Task
        } else {
            RoomInputMode::Chat
        },
        run_id,
        task_run_id: candidate.task_run_id.clone(),
        version,
    })
}

fn quarantine_corrupt_claim(
    transaction: &Transaction<'_>,
    candidate: &ClaimCandidate,
    error: &str,
) -> Result<bool> {
    let now = Utc::now().to_rfc3339();
    let updated = transaction.execute(
        "UPDATE member_inbox_items
         SET state = 'failed', run_id = NULL, lease_expires_at = NULL,
             error = ?1, completed_at = ?2, version = version + 1
         WHERE inbox_item_id = ?3 AND state = 'pending' AND version = ?4",
        params![error, &now, &candidate.inbox_item_id, candidate.version],
    )?;
    if updated != 1 {
        return Ok(false);
    }
    transaction.execute(
        "UPDATE room_event_deliveries
         SET state = 'suppressed', decision_reason = ?1, updated_at = ?2
         WHERE inbox_item_id = ?3 AND state IN ('queued', 'deferred', 'running')",
        params![error, &now, &candidate.inbox_item_id],
    )?;
    if let Some(room_id) = candidate.room_id.as_deref() {
        let room_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM collaboration_rooms WHERE room_id = ?1)",
            [room_id],
            |row| row.get(0),
        )?;
        if room_exists {
            enqueue_room_changed(
                transaction,
                room_id,
                "inbox",
                &candidate.inbox_item_id,
                &format!(
                    "inbox-corrupt:{}:{}",
                    candidate.inbox_item_id,
                    candidate.version + 1
                ),
            )?;
        }
    }
    Ok(true)
}

fn reply_reference_from_candidate(
    candidate: &ClaimCandidate,
    room_id: &str,
) -> Result<Option<RoomEventReferenceView>> {
    let Some(parent_event_id) = candidate.parent_event_id.as_deref() else {
        if candidate.reply_event_id.is_some() {
            return Err(CollaborationError::Config(format!(
                "Inbox 来源事件 {} 的回复目标关联异常",
                candidate.source_event_id
            )));
        }
        return Ok(None);
    };
    let reply_event_id = candidate.reply_event_id.as_deref().ok_or_else(|| {
        CollaborationError::Config(format!(
            "Inbox 来源事件 {} 的回复目标 {} 不存在",
            candidate.source_event_id, parent_event_id
        ))
    })?;
    if reply_event_id != parent_event_id {
        return Err(CollaborationError::Config(format!(
            "Inbox 来源事件 {} 的回复目标关联异常",
            candidate.source_event_id
        )));
    }
    let reply_room_id = candidate.reply_room_id.as_deref().ok_or_else(|| {
        CollaborationError::Config(format!(
            "Inbox 来源事件 {} 的回复目标 {} 缺少房间",
            candidate.source_event_id, parent_event_id
        ))
    })?;
    if reply_room_id != room_id {
        return Err(CollaborationError::Config(format!(
            "Inbox 来源事件 {} 的回复目标 {} 不属于同一房间",
            candidate.source_event_id, parent_event_id
        )));
    }
    let missing_column = |column: &str| {
        CollaborationError::Config(format!(
            "Inbox 来源事件 {} 的回复目标 {} 缺少{column}",
            candidate.source_event_id, parent_event_id
        ))
    };
    let content = candidate
        .reply_content
        .clone()
        .ok_or_else(|| missing_column("正文"))?;
    let created_at = candidate
        .reply_created_at
        .as_deref()
        .ok_or_else(|| missing_column("创建时间"))?;
    let created_at = DateTime::parse_from_rfc3339(created_at)
        .map_err(|_| missing_column("有效创建时间"))?
        .with_timezone(&Utc);
    Ok(Some(RoomEventReferenceView {
        event_id: reply_event_id.into(),
        sequence: candidate
            .reply_sequence
            .ok_or_else(|| missing_column("事件序号"))?,
        sender_kind: candidate
            .reply_sender_kind
            .clone()
            .ok_or_else(|| missing_column("发送者类型"))?,
        sender_id: candidate
            .reply_sender_id
            .clone()
            .ok_or_else(|| missing_column("发送者 ID"))?,
        sender_name: candidate
            .reply_sender_name
            .clone()
            .ok_or_else(|| missing_column("发送者名称"))?,
        kind: candidate
            .reply_kind
            .clone()
            .ok_or_else(|| missing_column("事件类型"))?,
        content_hash: history_content_hash(&content),
        content,
        created_at,
    }))
}

fn required_execution_working_directory(value: String) -> Result<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CollaborationError::Config(
            "房间事件缺少冻结的执行工作目录".into(),
        ));
    }
    Ok(PathBuf::from(value))
}

fn validated_reply_target(
    connection: &Connection,
    room_id: &str,
    event_id: &str,
) -> Result<ValidatedReplyTarget> {
    let target = connection
        .query_row(
            "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                    kind, content, COALESCE(conversation_root_event_id, event_id),
                    invalidated_at, created_at
             FROM room_events WHERE event_id = ?1",
            [event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| CollaborationError::ReplyTargetNotFound(event_id.into()))?;
    let (
        event_id,
        target_room_id,
        sequence,
        sender_kind,
        sender_id,
        sender_name,
        kind,
        content,
        conversation_root_event_id,
        invalidated_at,
        created_at,
    ) = target;
    if target_room_id != room_id {
        return Err(CollaborationError::ReplyTargetRoomMismatch {
            event_id,
            room_id: room_id.into(),
            target_room_id,
        });
    }
    if invalidated_at.is_some() {
        return Err(CollaborationError::ReplyTargetInvalidated(event_id));
    }
    let supported = matches!(
        (sender_kind.as_str(), kind.as_str()),
        ("user", "user_message") | ("member", "member_message")
    );
    if !supported {
        return Err(CollaborationError::ReplyTargetTypeUnsupported {
            event_id,
            sender_kind,
            kind,
        });
    }
    Ok(ValidatedReplyTarget {
        reference: RoomEventReferenceView {
            event_id,
            sequence,
            sender_kind,
            sender_id,
            sender_name,
            kind,
            content_hash: history_content_hash(&content),
            content,
            created_at: parse_datetime(&created_at),
        },
        conversation_root_event_id,
    })
}

fn allocate_room_sequence(transaction: &Transaction<'_>, room_id: &str) -> Result<u64> {
    let updated = transaction.execute(
        "UPDATE collaboration_rooms
         SET latest_event_seq = latest_event_seq + 1
         WHERE room_id = ?1",
        [room_id],
    )?;
    if updated == 0 {
        return Err(CollaborationError::RoomNotFound(room_id.into()));
    }
    Ok(transaction.query_row(
        "SELECT latest_event_seq FROM collaboration_rooms WHERE room_id = ?1",
        [room_id],
        |row| row.get(0),
    )?)
}

#[allow(clippy::too_many_arguments)]
fn insert_inbox_item(
    transaction: &Transaction<'_>,
    member_id: &str,
    source_event_id: &str,
    thread_key: &str,
    mode: RoomInputMode,
    purpose: InboxPurpose,
    conversation_root_event_id: &str,
    expected_member_version: u64,
    idempotency_key: &str,
    now: &DateTime<Utc>,
) -> Result<InboxItemView> {
    let inbox_item_id = format!("inbox-{}", Uuid::new_v4());
    let task_run_id = format!("task-{inbox_item_id}");
    transaction.execute(
        "INSERT INTO member_inbox_items(
             inbox_item_id, member_id, source_event_id, thread_key, purpose,
             conversation_root_event_id, context_through_seq, mode, state,
             run_id, task_run_id, idempotency_key, expected_member_version,
             cancel_requested, version, created_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, 'pending', NULL, ?8, ?9, ?10, 0, 1, ?11
         )",
        params![
            inbox_item_id,
            member_id,
            source_event_id,
            thread_key,
            purpose.as_db(),
            conversation_root_event_id,
            mode.as_db(),
            task_run_id,
            idempotency_key,
            expected_member_version,
            now.to_rfc3339(),
        ],
    )?;
    Ok(InboxItemView {
        inbox_item_id,
        member_id: member_id.into(),
        source_event_id: source_event_id.into(),
        thread_key: thread_key.into(),
        purpose,
        conversation_root_event_id: conversation_root_event_id.into(),
        context_through_seq: 0,
        state: InboxState::Pending,
        mode,
        run_id: None,
        task_run_id: Some(task_run_id),
        error: None,
        created_at: *now,
        started_at: None,
        completed_at: None,
        version: 1,
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_delivery(
    transaction: &Transaction<'_>,
    event_id: &str,
    member_id: &str,
    kind: DeliveryKind,
    state: DeliveryState,
    inbox_item_id: Option<&str>,
    decision_reason: Option<&str>,
    now: &DateTime<Utc>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO room_event_deliveries(
             event_id, member_id, delivery_kind, state, inbox_item_id,
             decision_reason, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            event_id,
            member_id,
            kind.as_db(),
            state.as_db(),
            inbox_item_id,
            decision_reason,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn append_service_event(
    transaction: &Transaction<'_>,
    room_id: &str,
    idempotency_key: &str,
    kind: &str,
    content: &str,
) -> Result<RoomEventView> {
    if let Some(event) = event_by_idempotency(transaction, room_id, idempotency_key)? {
        return Ok(event);
    }
    let execution_working_directory: String = transaction
        .query_row(
            "SELECT working_directory FROM collaboration_rooms WHERE room_id = ?1",
            [room_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| CollaborationError::RoomNotFound(room_id.into()))?;
    let execution_working_directory =
        required_execution_working_directory(execution_working_directory)?;
    let event_id = format!("event-{}", Uuid::new_v4());
    let sequence = allocate_room_sequence(transaction, room_id)?;
    let now = Utc::now();
    transaction.execute(
        "INSERT INTO room_events(
             event_id, room_id, sequence, sender_kind, sender_id, sender_name,
             kind, content, run_id, idempotency_key,
             execution_working_directory, created_at
          ) VALUES (?1, ?2, ?3, 'service', 'service', '系统', ?4, ?5, NULL, ?6, ?7, ?8)",
        params![
            event_id,
            room_id,
            sequence,
            kind,
            content,
            idempotency_key,
            execution_working_directory.display().to_string(),
            now.to_rfc3339(),
        ],
    )?;
    Ok(RoomEventView {
        event_id: event_id.clone(),
        room_id: room_id.into(),
        sequence,
        sender_kind: "service".into(),
        sender_id: "service".into(),
        sender_name: "系统".into(),
        recipients: Vec::new(),
        audience: Vec::new(),
        kind: kind.into(),
        content: content.into(),
        run_id: None,
        parent_event_id: None,
        reply_reference: None,
        conversation_root_event_id: event_id.clone(),
        debate_depth: 0,
        group_enabled: false,
        conversation_mode: RoomInputMode::Chat,
        created_at: now,
    })
}

fn event_by_idempotency(
    connection: &Connection,
    room_id: &str,
    idempotency_key: &str,
) -> Result<Option<RoomEventView>> {
    let event = connection
        .query_row(
            "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                    kind, content, run_id, parent_event_id,
                    COALESCE(conversation_root_event_id, event_id), debate_depth,
                    group_enabled, conversation_mode, created_at
             FROM room_events WHERE room_id = ?1 AND idempotency_key = ?2",
            params![room_id, idempotency_key],
            event_from_row_without_recipients,
        )
        .optional()?;
    let Some(event) = event else {
        return Ok(None);
    };
    Ok(hydrate_events(connection, vec![event])?.pop())
}

fn event_by_id(connection: &Connection, event_id: &str) -> Result<Option<RoomEventView>> {
    let event = connection
        .query_row(
            "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                    kind, content, run_id, parent_event_id,
                    COALESCE(conversation_root_event_id, event_id), debate_depth,
                    group_enabled, conversation_mode, created_at
             FROM room_events WHERE event_id = ?1",
            [event_id],
            event_from_row_without_recipients,
        )
        .optional()?;
    let Some(event) = event else {
        return Ok(None);
    };
    Ok(hydrate_events(connection, vec![event])?.pop())
}

fn hydrate_events(
    connection: &Connection,
    mut events: Vec<RoomEventView>,
) -> Result<Vec<RoomEventView>> {
    if events.is_empty() {
        return Ok(events);
    }
    let event_ids = events
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<Vec<_>>();
    let event_placeholders = vec!["?"; event_ids.len()].join(", ");

    let mut recipients_by_event = HashMap::<String, Vec<String>>::new();
    {
        let mut statement = connection.prepare(&format!(
            "SELECT event_id, member_id FROM room_event_recipients
             WHERE event_id IN ({event_placeholders}) ORDER BY event_id, member_id"
        ))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(event_ids.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (event_id, member_id) in rows {
            recipients_by_event
                .entry(event_id)
                .or_default()
                .push(member_id);
        }
    }

    let mut audience_by_event = HashMap::<String, Vec<String>>::new();
    {
        let mut statement = connection.prepare(&format!(
            "SELECT event_id, member_id FROM room_event_deliveries
             WHERE event_id IN ({event_placeholders}) ORDER BY event_id, member_id"
        ))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(event_ids.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (event_id, member_id) in rows {
            audience_by_event
                .entry(event_id)
                .or_default()
                .push(member_id);
        }
    }

    let mut parent_ids = events
        .iter()
        .filter_map(|event| event.parent_event_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    parent_ids.sort();
    let mut parents = HashMap::<String, (String, RoomEventReferenceView)>::new();
    if !parent_ids.is_empty() {
        let parent_placeholders = vec!["?"; parent_ids.len()].join(", ");
        let mut statement = connection.prepare(&format!(
            "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                    kind, content, created_at
             FROM room_events WHERE event_id IN ({parent_placeholders})"
        ))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(parent_ids.iter()), |row| {
                let content = row.get::<_, String>(7)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    RoomEventReferenceView {
                        event_id: row.get(0)?,
                        sequence: row.get(2)?,
                        sender_kind: row.get(3)?,
                        sender_id: row.get(4)?,
                        sender_name: row.get(5)?,
                        kind: row.get(6)?,
                        content_hash: history_content_hash(&content),
                        content,
                        created_at: parse_datetime(&row.get::<_, String>(8)?),
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (event_id, room_id, reference) in rows {
            parents.insert(event_id, (room_id, reference));
        }
    }

    for event in &mut events {
        event.recipients = recipients_by_event
            .remove(&event.event_id)
            .unwrap_or_default();
        event.audience = audience_by_event
            .remove(&event.event_id)
            .unwrap_or_default();
        let Some(parent_event_id) = event.parent_event_id.as_ref() else {
            event.reply_reference = None;
            continue;
        };
        let (parent_room_id, reference) = parents.get(parent_event_id).ok_or_else(|| {
            CollaborationError::Config(format!("事件 {parent_event_id} 的回复目标不存在"))
        })?;
        if parent_room_id != &event.room_id {
            return Err(CollaborationError::Config(format!(
                "事件 {} 的回复目标 {} 不属于同一房间",
                event.event_id, parent_event_id
            )));
        }
        event.reply_reference = Some(reference.clone());
    }
    Ok(events)
}

fn inbox_for_event(connection: &Connection, event_id: &str) -> Result<Vec<InboxItemView>> {
    let mut statement = connection.prepare(
        "SELECT inbox_item_id, member_id, source_event_id, thread_key, purpose,
                COALESCE(conversation_root_event_id, source_event_id), context_through_seq,
                state, mode, run_id, task_run_id, error, created_at, started_at,
                completed_at, version
         FROM member_inbox_items WHERE source_event_id = ?1 ORDER BY created_at, inbox_item_id",
    )?;
    let items = statement
        .query_map([event_id], inbox_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(items)
}

fn room_from_connection(connection: &Connection, room_id: &str) -> Result<CollaborationRoomView> {
    connection
        .query_row(
            "SELECT room_id, title, working_directory, default_member_id,
                    latest_event_seq, version, created_at
             FROM collaboration_rooms WHERE room_id = ?1",
            [room_id],
            |row| {
                Ok(CollaborationRoomView {
                    room_id: row.get(0)?,
                    title: row.get(1)?,
                    working_directory: row.get(2)?,
                    default_member_id: row.get(3)?,
                    latest_event_seq: row.get(4)?,
                    version: row.get(5)?,
                    created_at: parse_datetime(&row.get::<_, String>(6)?),
                })
            },
        )
        .optional()?
        .ok_or_else(|| CollaborationError::RoomNotFound(room_id.into()))
}

fn members_from_connection(connection: &Connection, room_id: &str) -> Result<Vec<BrainMemberView>> {
    let mut statement = connection.prepare(
        "SELECT m.member_id, m.room_id, m.display_name, m.template_id, m.profile_id,
                m.model_policy, m.reasoning_depth, m.availability, m.version,
                m.created_at, m.last_woken_at,
                (SELECT COUNT(*) FROM member_inbox_items i
                 WHERE i.member_id = m.member_id
                   AND i.state IN ('pending', 'leased')) AS pending_count,
                (SELECT i.run_id FROM member_inbox_items i
                 WHERE i.member_id = m.member_id AND i.state = 'running' LIMIT 1) AS active_run_id,
                (SELECT COUNT(*) FROM member_inbox_items i
                 WHERE i.member_id = m.member_id AND i.state = 'failed') AS failed_count
         FROM brain_members m
         WHERE m.room_id = ?1
         ORDER BY m.created_at, m.member_id",
    )?;
    let members = statement
        .query_map([room_id], |row| {
            let availability = MemberAvailability::from_db(&row.get::<_, String>(7)?);
            let pending_count = row.get::<_, usize>(11)?;
            let active_run_id = row.get::<_, Option<String>>(12)?;
            let failed_count = row.get::<_, usize>(13)?;
            let activity = if active_run_id.is_some() {
                MemberActivity::Running
            } else if pending_count > 0 {
                MemberActivity::Queued
            } else if failed_count > 0 {
                MemberActivity::Failed
            } else {
                MemberActivity::Idle
            };
            Ok(BrainMemberView {
                member_id: row.get(0)?,
                room_id: row.get(1)?,
                display_name: row.get(2)?,
                template_id: row.get(3)?,
                profile_id: row.get(4)?,
                model_policy: row.get(5)?,
                reasoning_depth: row.get(6)?,
                availability,
                activity,
                active_run_id,
                pending_count,
                version: row.get(8)?,
                created_at: parse_datetime(&row.get::<_, String>(9)?),
                last_woken_at: row
                    .get::<_, Option<String>>(10)?
                    .map(|value| parse_datetime(&value)),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(members)
}

fn events_from_connection(
    connection: &Connection,
    room_id: &str,
    limit: usize,
) -> Result<Vec<RoomEventView>> {
    let mut statement = connection.prepare(
        "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                kind, content, run_id, parent_event_id,
                COALESCE(conversation_root_event_id, event_id), debate_depth,
                group_enabled, conversation_mode, created_at
         FROM room_events
         WHERE room_id = ?1 AND invalidated_at IS NULL
         ORDER BY sequence DESC LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![room_id, limit], event_from_row_without_recipients)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut events = hydrate_events(connection, rows)?;
    events.reverse();
    Ok(events)
}

fn events_after_from_connection(
    connection: &Connection,
    room_id: &str,
    after_sequence: u64,
    limit: usize,
) -> Result<Vec<RoomEventView>> {
    let mut statement = connection.prepare(
        "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                kind, content, run_id, parent_event_id,
                COALESCE(conversation_root_event_id, event_id), debate_depth,
                group_enabled, conversation_mode, created_at
         FROM room_events
         WHERE room_id = ?1 AND sequence > ?2 AND invalidated_at IS NULL
         ORDER BY sequence LIMIT ?3",
    )?;
    let events = statement
        .query_map(
            params![room_id, after_sequence, limit],
            event_from_row_without_recipients,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    hydrate_events(connection, events)
}

fn events_before_from_connection(
    connection: &Connection,
    room_id: &str,
    before_sequence: u64,
    limit: usize,
) -> Result<RoomEventPage> {
    let requested = limit.clamp(1, 100);
    let mut statement = connection.prepare(
        "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                kind, content, run_id, parent_event_id,
                COALESCE(conversation_root_event_id, event_id), debate_depth,
                group_enabled, conversation_mode, created_at
         FROM room_events
         WHERE room_id = ?1 AND sequence < ?2 AND invalidated_at IS NULL
         ORDER BY sequence DESC LIMIT ?3",
    )?;
    let mut events = statement
        .query_map(
            params![room_id, before_sequence, requested + 1],
            event_from_row_without_recipients,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = events.len() > requested;
    events.truncate(requested);
    let mut events = hydrate_events(connection, events)?;
    events.reverse();
    Ok(RoomEventPage { events, has_more })
}

fn events_through_from_connection(
    connection: &Connection,
    room_id: &str,
    through_sequence: u64,
    limit: usize,
) -> Result<Vec<RoomEventView>> {
    let mut statement = connection.prepare(
        "SELECT event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                kind, content, run_id, parent_event_id,
                COALESCE(conversation_root_event_id, event_id), debate_depth,
                group_enabled, conversation_mode, created_at
         FROM room_events
         WHERE room_id = ?1 AND sequence <= ?2 AND invalidated_at IS NULL
         ORDER BY sequence DESC LIMIT ?3",
    )?;
    let rows = statement
        .query_map(
            params![room_id, through_sequence, limit],
            event_from_row_without_recipients,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut events = hydrate_events(connection, rows)?;
    events.reverse();
    Ok(events)
}

fn inbox_from_connection(
    connection: &Connection,
    room_id: &str,
    limit: usize,
) -> Result<Vec<InboxItemView>> {
    let mut statement = connection.prepare(
        "SELECT i.inbox_item_id, i.member_id, i.source_event_id, i.thread_key, i.purpose,
                COALESCE(i.conversation_root_event_id, i.source_event_id),
                i.context_through_seq, i.state, i.mode, i.run_id, i.task_run_id, i.error,
                i.created_at, i.started_at, i.completed_at, i.version
         FROM member_inbox_items i
         JOIN brain_members m ON m.member_id = i.member_id
         JOIN room_events e ON e.event_id = i.source_event_id
         WHERE m.room_id = ?1 AND e.invalidated_at IS NULL
         ORDER BY i.created_at DESC, i.rowid DESC LIMIT ?2",
    )?;
    let mut items = statement
        .query_map(params![room_id, limit], inbox_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    items.reverse();
    Ok(items)
}

fn event_from_row_without_recipients(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoomEventView> {
    Ok(RoomEventView {
        event_id: row.get(0)?,
        room_id: row.get(1)?,
        sequence: row.get(2)?,
        sender_kind: row.get(3)?,
        sender_id: row.get(4)?,
        sender_name: row.get(5)?,
        recipients: Vec::new(),
        audience: Vec::new(),
        kind: row.get(6)?,
        content: row.get(7)?,
        run_id: row.get(8)?,
        parent_event_id: row.get(9)?,
        reply_reference: None,
        conversation_root_event_id: row.get(10)?,
        debate_depth: row.get(11)?,
        group_enabled: row.get(12)?,
        conversation_mode: if row.get::<_, String>(13)? == "task" {
            RoomInputMode::Task
        } else {
            RoomInputMode::Chat
        },
        created_at: parse_datetime(&row.get::<_, String>(14)?),
    })
}

fn inbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItemView> {
    let mode = row.get::<_, String>(8)?;
    Ok(InboxItemView {
        inbox_item_id: row.get(0)?,
        member_id: row.get(1)?,
        source_event_id: row.get(2)?,
        thread_key: row.get(3)?,
        purpose: InboxPurpose::from_db(&row.get::<_, String>(4)?),
        conversation_root_event_id: row.get(5)?,
        context_through_seq: row.get(6)?,
        state: InboxState::from_db(&row.get::<_, String>(7)?),
        mode: if mode == "task" {
            RoomInputMode::Task
        } else {
            RoomInputMode::Chat
        },
        run_id: row.get(9)?,
        task_run_id: row.get(10)?,
        error: row.get(11)?,
        created_at: parse_datetime(&row.get::<_, String>(12)?),
        started_at: row
            .get::<_, Option<String>>(13)?
            .map(|value| parse_datetime(&value)),
        completed_at: row
            .get::<_, Option<String>>(14)?
            .map(|value| parse_datetime(&value)),
        version: row.get(15)?,
    })
}

fn deliveries_from_connection(
    connection: &Connection,
    room_id: &str,
    limit: usize,
) -> Result<Vec<RoomEventDeliveryView>> {
    let mut statement = connection.prepare(
        "SELECT d.event_id, d.member_id, d.delivery_kind, d.state, d.inbox_item_id,
                d.decision_reason, d.created_at, d.updated_at
         FROM room_event_deliveries d
         JOIN room_events e ON e.event_id = d.event_id
         WHERE e.room_id = ?1 AND e.invalidated_at IS NULL
         ORDER BY e.sequence DESC, d.member_id
         LIMIT ?2",
    )?;
    let mut deliveries = statement
        .query_map(params![room_id, limit], |row| {
            Ok(RoomEventDeliveryView {
                event_id: row.get(0)?,
                member_id: row.get(1)?,
                kind: DeliveryKind::from_db(&row.get::<_, String>(2)?),
                state: DeliveryState::from_db(&row.get::<_, String>(3)?),
                inbox_item_id: row.get(4)?,
                decision_reason: row.get(5)?,
                created_at: parse_datetime(&row.get::<_, String>(6)?),
                updated_at: parse_datetime(&row.get::<_, String>(7)?),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    deliveries.reverse();
    Ok(deliveries)
}

fn member_cursor_from_connection(
    connection: &Connection,
    member_id: &str,
    cursor_kind: MemberCursorKind,
) -> Result<MemberCursorView> {
    connection
        .query_row(
            "SELECT member_id, cursor_kind, event_sequence, version, updated_at
             FROM member_cursors WHERE member_id = ?1 AND cursor_kind = ?2",
            params![member_id, cursor_kind.as_db()],
            member_cursor_from_row,
        )
        .optional()?
        .ok_or_else(|| {
            CollaborationError::Config(format!(
                "成员 cursor 不存在: {member_id}:{}",
                cursor_kind.as_db()
            ))
        })
}

fn member_cursor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemberCursorView> {
    Ok(MemberCursorView {
        member_id: row.get(0)?,
        cursor_kind: MemberCursorKind::from_db(&row.get::<_, String>(1)?),
        event_sequence: row.get(2)?,
        version: row.get(3)?,
        updated_at: parse_datetime(&row.get::<_, String>(4)?),
    })
}

fn outbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CollaborationOutboxEvent> {
    Ok(CollaborationOutboxEvent {
        outbox_event_id: row.get(0)?,
        room_id: row.get(1)?,
        event_kind: row.get(2)?,
        aggregate_kind: row.get(3)?,
        aggregate_id: row.get(4)?,
        version: row.get(5)?,
        created_at: parse_datetime(&row.get::<_, String>(6)?),
    })
}

fn settle_sleep_after_current(transaction: &Transaction<'_>, member_id: &str) -> Result<()> {
    transaction.execute(
        "UPDATE brain_members SET availability = 'sleeping', version = version + 1
         WHERE member_id = ?1 AND availability = 'sleep_after_current'",
        [member_id],
    )?;
    Ok(())
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn parse_datetime(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map_or_else(|_| Utc::now(), |parsed| parsed.with_timezone(&Utc))
}

fn truncate_title(content: &str) -> String {
    let mut title = content.chars().take(30).collect::<String>();
    if content.chars().count() > 30 {
        title.push_str("...");
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> (tempfile::TempDir, CollaborationRepository) {
        let directory = tempfile::tempdir().unwrap();
        let repository = CollaborationRepository::new(
            directory.path(),
            CollaborationConfig {
                max_members_per_room: 3,
                max_pending_items_per_member: 3,
                max_pending_items_per_room: 4,
                max_recipients_per_message: 3,
                max_workers: 2,
                max_history_events_per_run: 20,
                ..CollaborationConfig::default()
            },
        )
        .unwrap();
        (directory, repository)
    }

    fn ensure(repository: &CollaborationRepository) -> RoomSnapshot {
        repository.ensure_room("room-1", "Test Room", &[]).unwrap()
    }

    #[test]
    fn room_working_directory_defaults_to_startup_and_survives_reopen() {
        let runtime = tempfile::tempdir().unwrap();
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        let canonical_a = workspace_a.path().canonicalize().unwrap();

        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            workspace_a.path(),
        )
        .unwrap();
        let created = repository.ensure_room("room-1", "Room", &[]).unwrap();
        assert_eq!(Path::new(&created.room.working_directory), canonical_a);
        drop(repository);

        let reopened = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            workspace_b.path(),
        )
        .unwrap();
        assert_eq!(
            Path::new(&reopened.snapshot("room-1").unwrap().room.working_directory),
            canonical_a
        );
    }

    #[test]
    fn room_working_directory_update_is_versioned_persisted_and_not_reset_by_ensure() {
        let runtime = tempfile::tempdir().unwrap();
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        let canonical_b = workspace_b.path().canonicalize().unwrap();
        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            workspace_a.path(),
        )
        .unwrap();
        let created = repository.ensure_room("room-1", "Room", &[]).unwrap();

        let updated = repository
            .update_room_working_directory(
                "room-1",
                &workspace_b.path().to_string_lossy(),
                created.room.version,
            )
            .unwrap();
        assert_eq!(updated.version, created.room.version + 1);
        assert_eq!(Path::new(&updated.working_directory), canonical_b);

        let ensured = repository
            .ensure_room("room-1", "Renamed Room", &[])
            .unwrap();
        assert_eq!(ensured.room.title, "Renamed Room");
        assert_eq!(Path::new(&ensured.room.working_directory), canonical_b);
        assert_eq!(ensured.room.version, updated.version);
        drop(repository);

        let reopened = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            workspace_a.path(),
        )
        .unwrap();
        assert_eq!(
            Path::new(&reopened.snapshot("room-1").unwrap().room.working_directory),
            canonical_b
        );
    }

    #[test]
    fn message_freezes_room_working_directory_for_claim_release_retry_and_new_messages() {
        let runtime = tempfile::tempdir().unwrap();
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        let canonical_a = workspace_a.path().canonicalize().unwrap();
        let canonical_b = workspace_b.path().canonicalize().unwrap();
        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            workspace_a.path(),
        )
        .unwrap();
        let snapshot = repository.ensure_room("room-1", "Room", &[]).unwrap();
        let member_id = snapshot.room.default_member_id;
        let first = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "U1",
                RoomInputMode::Chat,
                "freeze-u1",
            )
            .unwrap();

        repository
            .update_room_working_directory(
                "room-1",
                &workspace_b.path().to_string_lossy(),
                repository.snapshot("room-1").unwrap().room.version,
            )
            .unwrap();

        let first_lease = repository.lease_next().unwrap().unwrap();
        assert_eq!(first_lease.source_event_id, first.event.event_id);
        assert_eq!(first_lease.execution_working_directory, canonical_a);
        repository.release_lease(&first_lease).unwrap();

        let released_lease = repository.lease_next().unwrap().unwrap();
        assert_eq!(released_lease.source_event_id, first.event.event_id);
        assert_eq!(released_lease.execution_working_directory, canonical_a);

        repository
            .retry_last_user_event("room-1", &first.event.event_id)
            .unwrap();
        let retry_lease = repository.lease_next().unwrap().unwrap();
        assert_eq!(retry_lease.source_event_id, first.event.event_id);
        assert_eq!(retry_lease.execution_working_directory, canonical_a);
        let retry_claim = repository.activate_lease(&retry_lease).unwrap();
        let reply = repository
            .reconcile_completed_item(&retry_claim.inbox_item_id, &retry_claim.run_id, "U1 reply")
            .unwrap()
            .unwrap();

        let second = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "U2",
                RoomInputMode::Chat,
                "freeze-u2",
            )
            .unwrap();
        let second_claim = repository.claim_next().unwrap().unwrap();
        assert_eq!(second_claim.source_event_id, second.event.event_id);
        assert_eq!(second_claim.execution_working_directory, canonical_b);

        repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap();
        let ensured = repository
            .ensure_room(
                "room-1",
                "Room",
                &[LegacyMessageSeed {
                    id: "frozen-legacy".into(),
                    role: "assistant".into(),
                    content: "legacy reply".into(),
                    timestamp: Utc::now(),
                    hidden: false,
                }],
            )
            .unwrap();
        assert_eq!(Path::new(&ensured.room.working_directory), canonical_b);

        let connection = repository.connect().unwrap();
        let frozen_directory = |event_id: &str| {
            connection
                .query_row(
                    "SELECT execution_working_directory FROM room_events WHERE event_id = ?1",
                    [event_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap()
                .map(PathBuf::from)
        };
        assert_eq!(
            frozen_directory(&first.event.event_id),
            Some(canonical_a.clone())
        );
        assert_eq!(frozen_directory(&reply.event_id), Some(canonical_a));
        assert_eq!(
            frozen_directory(&second.event.event_id),
            Some(canonical_b.clone())
        );
        assert_eq!(
            frozen_directory("legacy-room-1-frozen-legacy"),
            Some(canonical_b.clone())
        );
        let service_directory: Option<String> = connection
            .query_row(
                "SELECT execution_working_directory FROM room_events
                 WHERE room_id = 'room-1' AND kind = 'member_created'
                 ORDER BY sequence DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(service_directory.map(PathBuf::from), Some(canonical_b));
    }

    #[test]
    fn message_freezes_room_working_directory_for_deferred_participation_reply() {
        let runtime = tempfile::tempdir().unwrap();
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        let canonical_a = workspace_a.path().canonicalize().unwrap();
        let canonical_b = workspace_b.path().canonicalize().unwrap();
        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            workspace_a.path(),
        )
        .unwrap();
        let created = repository.ensure_room("room-1", "Room", &[]).unwrap();
        let member_id = created.room.default_member_id;
        let member_version = created
            .members
            .iter()
            .find(|member| member.member_id == member_id)
            .unwrap()
            .version;
        repository
            .update_room_working_directory(
                "room-1",
                &workspace_b.path().to_string_lossy(),
                created.room.version,
            )
            .unwrap();

        let now = Utc::now();
        let mut connection = repository.connect().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute(
                "INSERT INTO room_events(
                     event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                     kind, content, parent_event_id, conversation_root_event_id,
                     debate_depth, group_enabled, conversation_mode, idempotency_key,
                     execution_working_directory, created_at
                 ) VALUES (
                     'participation-source', 'room-1', 1, 'user', 'user', '用户',
                     'user_message', '来源事件', NULL, 'participation-source',
                     0, 1, 'chat', 'participation-source', ?1, ?2
                 )",
                params![canonical_a.display().to_string(), now.to_rfc3339(),],
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO room_events(
                     event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                     kind, content, parent_event_id, conversation_root_event_id,
                     debate_depth, group_enabled, conversation_mode, idempotency_key,
                     execution_working_directory, created_at
                 ) VALUES (
                     'participation-target', 'room-1', 2, 'member', 'other-member', '智脑 B',
                     'member_message', '延迟投递的回复目标', 'participation-source',
                     'participation-source', 1, 1, 'chat', 'participation-target', ?1, ?2
                 )",
                params![canonical_b.display().to_string(), now.to_rfc3339(),],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE collaboration_rooms SET latest_event_seq = 2 WHERE room_id = 'room-1'",
                [],
            )
            .unwrap();
        let item = insert_inbox_item(
            &transaction,
            &member_id,
            "participation-source",
            "room:room-1",
            RoomInputMode::Chat,
            InboxPurpose::Participation,
            "participation-source",
            member_version,
            "participation-directory-freeze",
            &now,
        )
        .unwrap();
        insert_delivery(
            &transaction,
            "participation-target",
            &member_id,
            DeliveryKind::Ambient,
            DeliveryState::Deferred,
            None,
            Some("等待当前任务完成"),
            &now,
        )
        .unwrap();
        transaction
            .execute(
                "UPDATE room_event_deliveries
                 SET state = 'queued', inbox_item_id = ?1, decision_reason = NULL
                 WHERE event_id = 'participation-target' AND member_id = ?2",
                params![item.inbox_item_id, member_id],
            )
            .unwrap();
        transaction.commit().unwrap();
        drop(connection);

        let claim = repository.claim_next().unwrap().unwrap();
        assert_eq!(claim.purpose, InboxPurpose::Participation);
        assert_eq!(claim.source_event_id, "participation-source");
        assert_eq!(claim.response_to_event_id, "participation-target");
        assert_eq!(claim.execution_working_directory, canonical_a);

        let completion = repository
            .complete_participation_item(&claim, Some("参与回复"))
            .unwrap();
        assert_eq!(completion.disposition, ParticipationDisposition::Replied);
        let reply = completion.event.unwrap();
        assert_eq!(
            reply.parent_event_id.as_deref(),
            Some("participation-target")
        );
        let persisted_directory: String = repository
            .connect()
            .unwrap()
            .query_row(
                "SELECT execution_working_directory FROM room_events WHERE event_id = ?1",
                [reply.event_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(PathBuf::from(persisted_directory), canonical_a);
    }

    #[test]
    fn message_freezes_room_working_directory_rejects_missing_or_empty_event_directory() {
        let workspace = tempfile::tempdir().unwrap();
        for (index, invalid) in [None, Some("")].into_iter().enumerate() {
            let runtime = tempfile::tempdir().unwrap();
            let repository = CollaborationRepository::new_with_startup_working_directory(
                runtime.path(),
                CollaborationConfig::default(),
                workspace.path(),
            )
            .unwrap();
            let snapshot = repository.ensure_room("room-1", "Room", &[]).unwrap();
            let posted = repository
                .post_message(
                    "room-1",
                    &[snapshot.room.default_member_id],
                    "U1",
                    RoomInputMode::Chat,
                    &format!("freeze-invalid-directory-{index}"),
                )
                .unwrap();
            repository
                .connect()
                .unwrap()
                .execute(
                    "UPDATE room_events SET execution_working_directory = ?1 WHERE event_id = ?2",
                    params![invalid, posted.event.event_id],
                )
                .unwrap();
            let error = repository.lease_next().unwrap_err();
            assert!(matches!(
                error,
                CollaborationError::Config(message)
                    if message == "房间事件缺少冻结的执行工作目录"
            ));
            let error = repository
                .claim_for_reconciliation(
                    &posted.inbox_items[0].inbox_item_id,
                    posted.inbox_items[0].task_run_id.as_deref().unwrap(),
                    "invalid-directory-reconciliation",
                )
                .unwrap_err();
            assert!(matches!(
                error,
                CollaborationError::Config(message)
                    if message == "房间事件缺少冻结的执行工作目录"
            ));
        }
    }

    #[test]
    fn claim_preserves_reply_and_directory_rejects_missing_source_event() {
        let lease_runtime = tempfile::tempdir().unwrap();
        let lease_repository =
            CollaborationRepository::new(lease_runtime.path(), CollaborationConfig::default())
                .unwrap();
        let lease_room = lease_repository
            .ensure_room("lease-room", "Lease Room", &[])
            .unwrap();
        let lease_post = lease_repository
            .post_group_message(
                "lease-room",
                &[lease_room.room.default_member_id],
                "missing lease source",
                RoomInputMode::Chat,
                "missing-lease-source",
            )
            .unwrap();
        let lease_connection = lease_repository.connect().unwrap();
        lease_connection
            .execute_batch("PRAGMA foreign_keys = OFF")
            .unwrap();
        lease_connection
            .execute(
                "UPDATE member_inbox_items SET source_event_id = 'missing-lease-event'",
                [],
            )
            .unwrap();
        drop(lease_connection);

        let error = lease_repository.lease_next().unwrap_err();
        assert!(matches!(
            error,
            CollaborationError::Config(message)
                if message == "Inbox 来源用户事件 missing-lease-event 不存在"
        ));
        let missing_inbox_id = &lease_post.inbox_items[0].inbox_item_id;
        let connection = lease_repository.connect().unwrap();
        let (state, stored_error, completed_at): (String, Option<String>, Option<String>) =
            connection
                .query_row(
                    "SELECT state, error, completed_at FROM member_inbox_items
                     WHERE inbox_item_id = ?1",
                    [missing_inbox_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
        assert_eq!(state, "failed");
        assert!(stored_error
            .as_deref()
            .is_some_and(|message| message.contains("missing-lease-event")));
        assert!(completed_at.is_some());
        let (delivery_state, delivery_reason): (String, Option<String>) = connection
            .query_row(
                "SELECT state, decision_reason FROM room_event_deliveries
                 WHERE inbox_item_id = ?1",
                [missing_inbox_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(delivery_state, "suppressed");
        assert!(delivery_reason
            .as_deref()
            .is_some_and(|message| message.contains("missing-lease-event")));
        let fabricated_outbox: usize = connection
            .query_row(
                "SELECT COUNT(*) FROM runtime_outbox_events
                 WHERE aggregate_kind = 'inbox' AND aggregate_id = ?1
                   AND idempotency_key LIKE 'inbox-corrupt:%'",
                [missing_inbox_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fabricated_outbox, 0);
        drop(connection);

        let reconcile_runtime = tempfile::tempdir().unwrap();
        let reconcile_repository =
            CollaborationRepository::new(reconcile_runtime.path(), CollaborationConfig::default())
                .unwrap();
        let reconcile_room = reconcile_repository
            .ensure_room("reconcile-room", "Reconcile Room", &[])
            .unwrap();
        reconcile_repository
            .post_message(
                "reconcile-room",
                &[reconcile_room.room.default_member_id],
                "missing reconciliation source",
                RoomInputMode::Chat,
                "missing-reconciliation-source",
            )
            .unwrap();
        let lease = reconcile_repository.lease_next().unwrap().unwrap();
        let reconcile_connection = reconcile_repository.connect().unwrap();
        reconcile_connection
            .execute_batch("PRAGMA foreign_keys = OFF")
            .unwrap();
        reconcile_connection
            .execute(
                "UPDATE member_inbox_items SET source_event_id = 'missing-reconciliation-event'
                 WHERE inbox_item_id = ?1",
                [&lease.inbox_item_id],
            )
            .unwrap();
        drop(reconcile_connection);

        let error = reconcile_repository
            .claim_for_reconciliation(
                &lease.inbox_item_id,
                &lease.task_run_id,
                "missing-source-durable-run",
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CollaborationError::Config(message)
                if message == "Inbox 来源用户事件 missing-reconciliation-event 不存在"
        ));
    }

    #[test]
    fn claim_preserves_reply_and_directory_quarantines_corrupt_head() {
        let (_directory, repository) = repository();
        let room = ensure(&repository);
        let member_id = room.room.default_member_id;
        let first = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "损坏的队首消息",
                RoomInputMode::Chat,
                "corrupt-head-first",
            )
            .unwrap();
        let second = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "健康的后续消息",
                RoomInputMode::Chat,
                "corrupt-head-second",
            )
            .unwrap();
        let first_inbox_id = &first.inbox_items[0].inbox_item_id;
        repository
            .connect()
            .unwrap()
            .execute(
                "UPDATE room_events SET execution_working_directory = '' WHERE event_id = ?1",
                [&first.event.event_id],
            )
            .unwrap();

        let error = repository.lease_next().unwrap_err();
        assert!(matches!(
            error,
            CollaborationError::Config(message)
                if message == "房间事件缺少冻结的执行工作目录"
        ));

        let snapshot = repository.snapshot("room-1").unwrap();
        let quarantined = snapshot
            .inbox
            .iter()
            .find(|item| item.inbox_item_id == *first_inbox_id)
            .unwrap();
        assert_eq!(quarantined.state, InboxState::Failed);
        assert!(quarantined
            .error
            .as_deref()
            .is_some_and(|message| message.contains("冻结的执行工作目录")));
        assert!(quarantined.completed_at.is_some());
        let delivery = snapshot
            .deliveries
            .iter()
            .find(|delivery| delivery.inbox_item_id.as_deref() == Some(first_inbox_id))
            .unwrap();
        assert_eq!(delivery.state, DeliveryState::Suppressed);
        assert!(delivery
            .decision_reason
            .as_deref()
            .is_some_and(|message| message.contains("冻结的执行工作目录")));
        let outbox_count: usize = repository
            .connect()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM runtime_outbox_events
                 WHERE aggregate_kind = 'inbox' AND aggregate_id = ?1
                   AND idempotency_key LIKE 'inbox-corrupt:%'",
                [first_inbox_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outbox_count, 1);

        let healthy = repository.lease_next().unwrap().unwrap();
        assert_eq!(healthy.source_event_id, second.event.event_id);
    }

    mod reply_target {
        use super::*;

        fn repository_with_workspace(
            workspace: &Path,
        ) -> (tempfile::TempDir, CollaborationRepository) {
            let runtime = tempfile::tempdir().unwrap();
            let repository = CollaborationRepository::new_with_startup_working_directory(
                runtime.path(),
                CollaborationConfig {
                    max_members_per_room: 6,
                    max_pending_items_per_member: 1_000,
                    max_pending_items_per_room: 2_000,
                    max_recipients_per_message: 6,
                    max_workers: 6,
                    ..CollaborationConfig::default()
                },
                workspace,
            )
            .unwrap();
            (runtime, repository)
        }

        fn post_reply(
            repository: &CollaborationRepository,
            room_id: &str,
            recipient_ids: &[String],
            content: &str,
            idempotency_key: &str,
            reply_to_event_id: Option<&str>,
        ) -> PostMessageResult {
            let snapshot = repository.snapshot(room_id).unwrap();
            let recipients = recipient_ids
                .iter()
                .map(|member_id| MemberAddress {
                    member_id: member_id.clone(),
                    expected_version: snapshot
                        .members
                        .iter()
                        .find(|member| member.member_id == *member_id)
                        .unwrap()
                        .version,
                })
                .collect::<Vec<_>>();
            repository
                .post_group_message_checked_with_reply(
                    &CollaborationActor::local(),
                    room_id,
                    &recipients,
                    content,
                    RoomInputMode::Chat,
                    DEFAULT_THREAD_KEY,
                    snapshot.room.version,
                    idempotency_key,
                    reply_to_event_id,
                )
                .unwrap()
        }

        fn assert_reference_matches(reference: &RoomEventReferenceView, target: &RoomEventView) {
            assert_eq!(reference.event_id, target.event_id);
            assert_eq!(reference.sequence, target.sequence);
            assert_eq!(reference.sender_kind, target.sender_kind);
            assert_eq!(reference.sender_id, target.sender_id);
            assert_eq!(reference.sender_name, target.sender_name);
            assert_eq!(reference.kind, target.kind);
            assert_eq!(reference.content, target.content);
            assert_eq!(
                reference.content_hash,
                history_content_hash(&target.content)
            );
            assert_eq!(reference.created_at, target.created_at);
        }

        #[test]
        fn reply_to_member_event_preserves_parent_root_and_reference() {
            let workspace = tempfile::tempdir().unwrap();
            let (_runtime, repository) = repository_with_workspace(workspace.path());
            let snapshot = repository.ensure_room("room-1", "Room", &[]).unwrap();
            let member_id = snapshot.room.default_member_id;
            let root = repository
                .post_message(
                    "room-1",
                    std::slice::from_ref(&member_id),
                    "root",
                    RoomInputMode::Chat,
                    "reply-root",
                )
                .unwrap();
            let root_claim = repository.claim_next().unwrap().unwrap();
            let target = repository
                .complete_item(&root_claim, "member target")
                .unwrap()
                .unwrap();

            let posted = post_reply(
                &repository,
                "room-1",
                std::slice::from_ref(&member_id),
                "reply to member",
                "reply-to-member",
                Some(&target.event_id),
            );

            assert_eq!(
                posted.event.parent_event_id.as_deref(),
                Some(target.event_id.as_str())
            );
            assert_eq!(posted.event.conversation_root_event_id, root.event.event_id);
            let reference = posted.event.reply_reference.as_ref().unwrap();
            assert_reference_matches(reference, &target);
            assert!(target.sequence < posted.event.sequence);
            let claim = repository.claim_next().unwrap().unwrap();
            assert_eq!(claim.reply_reference, Some(reference.clone()));
            let recovered = repository
                .claim_for_reconciliation(
                    &claim.inbox_item_id,
                    &claim.task_run_id,
                    "durable-reply-run",
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                recovered.execution_working_directory,
                claim.execution_working_directory
            );
            assert_eq!(recovered.reply_reference, claim.reply_reference);
        }

        #[test]
        fn claim_preserves_reply_and_directory() {
            let workspace = tempfile::tempdir().unwrap();
            let canonical_workspace = workspace.path().canonicalize().unwrap();
            let (_runtime, repository) = repository_with_workspace(workspace.path());
            let snapshot = repository.ensure_room("room-1", "Room", &[]).unwrap();
            let member_id = snapshot.room.default_member_id;
            let _root = repository
                .post_message(
                    "room-1",
                    std::slice::from_ref(&member_id),
                    "root",
                    RoomInputMode::Chat,
                    "claim-reply-root",
                )
                .unwrap();
            let root_claim = repository.claim_next().unwrap().unwrap();
            let target = repository
                .complete_item(&root_claim, "member target")
                .unwrap()
                .unwrap();
            let posted = post_reply(
                &repository,
                "room-1",
                std::slice::from_ref(&member_id),
                "reply to target",
                "claim-reply-source",
                Some(&target.event_id),
            );

            let lease = repository.lease_next().unwrap().unwrap();
            assert_eq!(lease.source_event_id, posted.event.event_id);
            assert_eq!(lease.execution_working_directory, canonical_workspace);
            let reference = lease.reply_reference.as_ref().unwrap();
            assert_eq!(reference.event_id, target.event_id);
            assert_eq!(reference.sequence, target.sequence);
            assert_eq!(
                reference.content_hash,
                history_content_hash(&target.content)
            );

            repository
                .connect()
                .unwrap()
                .execute(
                    "UPDATE room_events SET invalidated_at = ?1 WHERE event_id = ?2",
                    params![Utc::now().to_rfc3339(), target.event_id],
                )
                .unwrap();

            let recovered = repository
                .claim_for_reconciliation(
                    &lease.inbox_item_id,
                    &lease.task_run_id,
                    "durable-claim-reply-run",
                )
                .unwrap()
                .unwrap();
            assert_eq!(recovered.source_event_id, posted.event.event_id);
            assert_eq!(recovered.execution_working_directory, canonical_workspace);
            assert_eq!(recovered.reply_reference, lease.reply_reference);
        }

        #[test]
        fn reply_to_old_user_event_is_projected_outside_snapshot_window() {
            let workspace = tempfile::tempdir().unwrap();
            let canonical_workspace = workspace.path().canonicalize().unwrap();
            let (_runtime, repository) = repository_with_workspace(workspace.path());
            let snapshot = repository.ensure_room("room-1", "Room", &[]).unwrap();
            let member_id = snapshot.room.default_member_id;
            let target = post_reply(
                &repository,
                "room-1",
                std::slice::from_ref(&member_id),
                "old target",
                "old-target",
                None,
            );
            let mut connection = repository.connect().unwrap();
            let transaction = connection.transaction().unwrap();
            for sequence in 2_u64..=301 {
                let event_id = format!("window-filler-{sequence}");
                transaction
                    .execute(
                        "INSERT INTO room_events(
                             event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                             kind, content, conversation_root_event_id, group_enabled,
                             conversation_mode, idempotency_key, execution_working_directory,
                             created_at
                         ) VALUES (
                             ?1, 'room-1', ?2, 'service', 'service', '系统',
                             'window_filler', ?1, ?1, 0, 'chat', ?1, ?3, ?4
                         )",
                        params![
                            event_id,
                            sequence,
                            canonical_workspace.display().to_string(),
                            Utc::now().to_rfc3339()
                        ],
                    )
                    .unwrap();
            }
            transaction
                .execute(
                    "UPDATE collaboration_rooms SET latest_event_seq = 301 WHERE room_id = 'room-1'",
                    [],
                )
                .unwrap();
            transaction.commit().unwrap();

            let posted = post_reply(
                &repository,
                "room-1",
                std::slice::from_ref(&member_id),
                "reply outside window",
                "reply-outside-window",
                Some(&target.event.event_id),
            );
            let snapshot = repository.snapshot("room-1").unwrap();
            assert_eq!(snapshot.events.len(), 300);
            assert!(snapshot
                .events
                .iter()
                .all(|event| event.event_id != target.event.event_id));
            let projected = snapshot
                .events
                .iter()
                .find(|event| event.event_id == posted.event.event_id)
                .unwrap();
            assert_reference_matches(projected.reply_reference.as_ref().unwrap(), &target.event);
        }

        #[test]
        fn all_explicit_recipients_share_the_same_reply_reference() {
            let workspace = tempfile::tempdir().unwrap();
            let (_runtime, repository) = repository_with_workspace(workspace.path());
            let snapshot = repository.ensure_room("room-1", "Room", &[]).unwrap();
            let member_a = snapshot.room.default_member_id;
            let member_b = repository
                .create_member("room-1", "智脑 B", None, None)
                .unwrap()
                .member_id;
            let member_c = repository
                .create_member("room-1", "智脑 C", None, None)
                .unwrap()
                .member_id;
            let target = repository
                .post_message(
                    "room-1",
                    std::slice::from_ref(&member_c),
                    "shared target",
                    RoomInputMode::Chat,
                    "shared-target",
                )
                .unwrap();
            let target_claim = repository.claim_next().unwrap().unwrap();
            repository
                .complete_item(&target_claim, "target handled")
                .unwrap();

            let posted = post_reply(
                &repository,
                "room-1",
                &[member_a.clone(), member_b.clone()],
                "shared reply",
                "shared-reply",
                Some(&target.event.event_id),
            );
            assert_eq!(posted.inbox_items.len(), 2);
            assert!(posted
                .inbox_items
                .iter()
                .all(|item| item.member_id == member_a || item.member_id == member_b));
            assert!(posted
                .inbox_items
                .iter()
                .all(|item| item.member_id != member_c));
            let reference = posted.event.reply_reference.clone().unwrap();
            let first = repository.claim_next().unwrap().unwrap();
            let second = repository.claim_next().unwrap().unwrap();
            assert_ne!(first.member_id, second.member_id);
            assert_eq!(first.reply_reference, Some(reference.clone()));
            assert_eq!(second.reply_reference, Some(reference));
        }

        #[test]
        fn cross_room_invalidated_and_service_reply_targets_are_rejected() {
            let workspace = tempfile::tempdir().unwrap();
            let (_runtime, repository) = repository_with_workspace(workspace.path());
            let room_one = repository.ensure_room("room-1", "Room 1", &[]).unwrap();
            let room_two = repository.ensure_room("room-2", "Room 2", &[]).unwrap();
            let recipient = room_one.room.default_member_id;

            let missing =
                post_reply_error(&repository, &recipient, "missing-target", "reply-missing");
            assert!(matches!(
                missing,
                CollaborationError::ReplyTargetNotFound(event_id)
                    if event_id == "missing-target"
            ));

            let cross_room = post_reply(
                &repository,
                "room-2",
                &[room_two.room.default_member_id],
                "cross-room target",
                "cross-room-target",
                None,
            );
            let cross_room_error = post_reply_error(
                &repository,
                &recipient,
                &cross_room.event.event_id,
                "reply-cross-room",
            );
            assert!(matches!(
                cross_room_error,
                CollaborationError::ReplyTargetRoomMismatch {
                    event_id,
                    room_id,
                    target_room_id,
                } if event_id == cross_room.event.event_id
                    && room_id == "room-1"
                    && target_room_id == "room-2"
            ));

            let invalidated = post_reply(
                &repository,
                "room-1",
                std::slice::from_ref(&recipient),
                "invalidated target",
                "invalidated-target",
                None,
            );
            repository
                .connect()
                .unwrap()
                .execute(
                    "UPDATE room_events SET invalidated_at = ?1 WHERE event_id = ?2",
                    params![Utc::now().to_rfc3339(), invalidated.event.event_id],
                )
                .unwrap();
            let invalidated_error = post_reply_error(
                &repository,
                &recipient,
                &invalidated.event.event_id,
                "reply-invalidated",
            );
            assert!(matches!(
                invalidated_error,
                CollaborationError::ReplyTargetInvalidated(event_id)
                    if event_id == invalidated.event.event_id
            ));

            repository
                .create_member("room-1", "service target member", None, None)
                .unwrap();
            let service_event_id: String = repository
                .connect()
                .unwrap()
                .query_row(
                    "SELECT event_id FROM room_events
                     WHERE room_id = 'room-1' AND sender_kind = 'service'
                     ORDER BY sequence DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let service_error =
                post_reply_error(&repository, &recipient, &service_event_id, "reply-service");
            assert!(matches!(
                service_error,
                CollaborationError::ReplyTargetTypeUnsupported {
                    event_id,
                    sender_kind,
                    kind,
                } if event_id == service_event_id
                    && sender_kind == "service"
                    && kind == "member_created"
            ));
        }

        fn post_reply_error(
            repository: &CollaborationRepository,
            recipient: &str,
            target: &str,
            idempotency_key: &str,
        ) -> CollaborationError {
            let snapshot = repository.snapshot("room-1").unwrap();
            let member = snapshot
                .members
                .iter()
                .find(|member| member.member_id == recipient)
                .unwrap();
            repository
                .post_group_message_checked_with_reply(
                    &CollaborationActor::local(),
                    "room-1",
                    &[MemberAddress {
                        member_id: recipient.into(),
                        expected_version: member.version,
                    }],
                    "invalid reply",
                    RoomInputMode::Chat,
                    DEFAULT_THREAD_KEY,
                    snapshot.room.version,
                    idempotency_key,
                    Some(target),
                )
                .unwrap_err()
        }

        #[test]
        fn idempotent_replay_keeps_original_directory_and_reply_target() {
            let workspace_a = tempfile::tempdir().unwrap();
            let workspace_b = tempfile::tempdir().unwrap();
            let canonical_a = workspace_a.path().canonicalize().unwrap();
            let (_runtime, repository) = repository_with_workspace(workspace_a.path());
            let snapshot = repository.ensure_room("room-1", "Room", &[]).unwrap();
            let member_id = snapshot.room.default_member_id;
            let original_target = repository
                .post_message(
                    "room-1",
                    std::slice::from_ref(&member_id),
                    "original target",
                    RoomInputMode::Chat,
                    "idempotent-original-target",
                )
                .unwrap();
            let target_claim = repository.claim_next().unwrap().unwrap();
            repository
                .complete_item(&target_claim, "target handled")
                .unwrap();
            let first = post_reply(
                &repository,
                "room-1",
                std::slice::from_ref(&member_id),
                "first reply",
                "idempotent-reply",
                Some(&original_target.event.event_id),
            );
            repository
                .update_room_working_directory(
                    "room-1",
                    &workspace_b.path().to_string_lossy(),
                    repository.snapshot("room-1").unwrap().room.version,
                )
                .unwrap();
            let changed_target = post_reply(
                &repository,
                "room-1",
                std::slice::from_ref(&member_id),
                "changed target",
                "idempotent-changed-target",
                None,
            );
            let duplicate = repository
                .post_group_message_checked_with_reply(
                    &CollaborationActor::local(),
                    "room-1",
                    &[MemberAddress {
                        member_id: member_id.clone(),
                        expected_version: u64::MAX,
                    }],
                    "different replay content",
                    RoomInputMode::Task,
                    "different-thread",
                    0,
                    "idempotent-reply",
                    Some(&changed_target.event.event_id),
                )
                .unwrap();

            assert!(duplicate.duplicate);
            assert_eq!(duplicate.event.event_id, first.event.event_id);
            assert_eq!(duplicate.event.parent_event_id, first.event.parent_event_id);
            assert_eq!(duplicate.event.reply_reference, first.event.reply_reference);
            assert_eq!(
                duplicate
                    .inbox_items
                    .iter()
                    .map(|item| &item.inbox_item_id)
                    .collect::<Vec<_>>(),
                first
                    .inbox_items
                    .iter()
                    .map(|item| &item.inbox_item_id)
                    .collect::<Vec<_>>()
            );
            let stored_directory: String = repository
                .connect()
                .unwrap()
                .query_row(
                    "SELECT execution_working_directory FROM room_events WHERE event_id = ?1",
                    [&first.event.event_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(PathBuf::from(stored_directory), canonical_a);
            let claim = repository.claim_next().unwrap().unwrap();
            assert_eq!(claim.source_event_id, first.event.event_id);
            assert_eq!(claim.execution_working_directory, canonical_a);
            assert_eq!(claim.reply_reference, first.event.reply_reference);
        }
    }

    mod events_before {
        use super::*;

        fn paged_repository() -> (tempfile::TempDir, CollaborationRepository) {
            let runtime = tempfile::tempdir().unwrap();
            let workspace = tempfile::tempdir().unwrap();
            let repository = CollaborationRepository::new_with_startup_working_directory(
                runtime.path(),
                CollaborationConfig::default(),
                workspace.path(),
            )
            .unwrap();
            // 仓储只保存规范路径，测试无需继续持有工作目录句柄。
            drop(workspace);
            (runtime, repository)
        }

        fn seed_service_events(
            repository: &CollaborationRepository,
            room_id: &str,
            count: u64,
            invalidated_sequences: &[u64],
        ) {
            let room = repository.snapshot(room_id).unwrap().room;
            let mut connection = repository.connect().unwrap();
            let transaction = connection.transaction().unwrap();
            for sequence in 1..=count {
                let event_id = format!("page-event-{sequence}");
                let invalidated_at = invalidated_sequences
                    .contains(&sequence)
                    .then(|| Utc::now().to_rfc3339());
                transaction
                    .execute(
                        "INSERT INTO room_events(
                             event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                             kind, content, conversation_root_event_id, group_enabled,
                             conversation_mode, idempotency_key, execution_working_directory,
                             created_at, invalidated_at
                         ) VALUES (
                             ?1, ?2, ?3, 'service', 'service', '系统', 'page_event',
                             ?1, ?1, 0, 'chat', ?1, ?4, ?5, ?6
                         )",
                        params![
                            event_id,
                            room_id,
                            sequence,
                            room.working_directory,
                            Utc::now().to_rfc3339(),
                            invalidated_at
                        ],
                    )
                    .unwrap();
            }
            transaction
                .execute(
                    "UPDATE collaboration_rooms SET latest_event_seq = ?1 WHERE room_id = ?2",
                    params![count, room_id],
                )
                .unwrap();
            transaction.commit().unwrap();
        }

        #[test]
        fn events_before_clamps_limit_orders_ascending_and_reports_more() {
            let (_runtime, repository) = paged_repository();
            repository.ensure_room("room-1", "Room", &[]).unwrap();
            seed_service_events(&repository, "room-1", 150, &[120, 140]);

            let newest = repository.events_before("room-1", 151, 200).unwrap();
            assert_eq!(newest.events.len(), 100);
            assert!(newest.has_more);
            assert!(newest
                .events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence));
            assert!(newest.events.iter().all(|event| event.sequence < 151));
            assert!(newest
                .events
                .iter()
                .all(|event| !matches!(event.sequence, 120 | 140)));

            let older = repository
                .events_before("room-1", newest.events[0].sequence, 100)
                .unwrap();
            assert!(!older.has_more);
            let sequences = older
                .events
                .iter()
                .chain(newest.events.iter())
                .map(|event| event.sequence)
                .collect::<Vec<_>>();
            assert_eq!(sequences.len(), 148);
            assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));

            let minimum = repository.events_before("room-1", 151, 0).unwrap();
            assert_eq!(minimum.events.len(), 1);
            assert_eq!(minimum.events[0].sequence, 150);
            assert!(minimum.has_more);
            let empty = repository.events_before("room-1", 0, 10).unwrap();
            assert!(empty.events.is_empty());
            assert!(!empty.has_more);
        }

        #[test]
        fn events_before_hydrates_invalidated_parent_reference_outside_page() {
            let (_runtime, repository) = paged_repository();
            let room = repository.ensure_room("room-1", "Room", &[]).unwrap().room;
            let mut connection = repository.connect().unwrap();
            let transaction = connection.transaction().unwrap();
            let created_at = Utc::now();
            transaction
                .execute(
                    "INSERT INTO room_events(
                         event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                         kind, content, conversation_root_event_id, group_enabled,
                         conversation_mode, idempotency_key, execution_working_directory,
                         created_at, invalidated_at
                     ) VALUES (
                         'page-parent', 'room-1', 1, 'user', 'user', '用户',
                         'user_message', 'historical parent', 'page-parent', 1, 'chat',
                         'page-parent', ?1, ?2, ?3
                     )",
                    params![
                        room.working_directory,
                        created_at.to_rfc3339(),
                        Utc::now().to_rfc3339()
                    ],
                )
                .unwrap();
            for sequence in 2_u64..=10 {
                let event_id = format!("parent-filler-{sequence}");
                transaction
                    .execute(
                        "INSERT INTO room_events(
                             event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                             kind, content, conversation_root_event_id, group_enabled,
                             conversation_mode, idempotency_key, execution_working_directory,
                             created_at
                         ) VALUES (
                             ?1, 'room-1', ?2, 'service', 'service', '系统', 'page_filler',
                             ?1, ?1, 0, 'chat', ?1, ?3, ?4
                         )",
                        params![
                            event_id,
                            sequence,
                            room.working_directory,
                            Utc::now().to_rfc3339()
                        ],
                    )
                    .unwrap();
            }
            transaction
                .execute(
                    "INSERT INTO room_events(
                         event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                         kind, content, parent_event_id, conversation_root_event_id,
                         group_enabled, conversation_mode, idempotency_key,
                         execution_working_directory, created_at
                     ) VALUES (
                         'page-child', 'room-1', 11, 'user', 'user', '用户',
                         'user_message', 'historical child', 'page-parent', 'page-parent',
                         1, 'chat', 'page-child', ?1, ?2
                     )",
                    params![room.working_directory, Utc::now().to_rfc3339()],
                )
                .unwrap();
            transaction
                .execute(
                    "UPDATE collaboration_rooms SET latest_event_seq = 11 WHERE room_id = 'room-1'",
                    [],
                )
                .unwrap();
            transaction.commit().unwrap();

            let page = repository.events_before("room-1", 12, 1).unwrap();
            assert_eq!(page.events.len(), 1);
            assert_eq!(page.events[0].event_id, "page-child");
            let reference = page.events[0].reply_reference.as_ref().unwrap();
            assert_eq!(reference.event_id, "page-parent");
            assert_eq!(reference.content, "historical parent");
            assert_eq!(
                reference.content_hash,
                history_content_hash("historical parent")
            );
            assert!(page.has_more);
        }

        #[test]
        fn events_before_snapshot_reports_earlier_and_missing_room_is_explicit() {
            let (_runtime, repository) = paged_repository();
            repository.ensure_room("room-1", "Room", &[]).unwrap();
            seed_service_events(&repository, "room-1", 305, &[]);

            let snapshot = repository.snapshot("room-1").unwrap();
            assert_eq!(snapshot.events.len(), 300);
            assert_eq!(snapshot.events[0].sequence, 6);
            assert!(snapshot.has_earlier_events);
            let mut serialized = serde_json::to_value(&snapshot).unwrap();
            serialized
                .as_object_mut()
                .unwrap()
                .remove("has_earlier_events");
            let old_snapshot: RoomSnapshot = serde_json::from_value(serialized).unwrap();
            assert!(!old_snapshot.has_earlier_events);

            let error = repository
                .events_before("missing-room", 10, 10)
                .unwrap_err();
            assert!(matches!(
                error,
                CollaborationError::RoomNotFound(room_id) if room_id == "missing-room"
            ));
        }
    }

    #[test]
    fn invalid_room_working_directory_errors_leave_path_and_version_unchanged() {
        let runtime = tempfile::tempdir().unwrap();
        let startup = tempfile::tempdir().unwrap();
        let absolute_workspace = tempfile::tempdir().unwrap();
        let relative_workspace = startup.path().join("relative-workspace");
        fs::create_dir(&relative_workspace).unwrap();
        let file_path = startup.path().join("not-a-directory.txt");
        fs::write(&file_path, "file").unwrap();
        let missing_path = startup.path().join("missing-workspace");
        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            startup.path(),
        )
        .unwrap();
        let created = repository.ensure_room("room-1", "Room", &[]).unwrap();
        let original_path = created.room.working_directory.clone();
        let original_version = created.room.version;

        for invalid_path in [
            "",
            "   ",
            missing_path.to_str().unwrap(),
            file_path.to_str().unwrap(),
        ] {
            assert!(repository
                .update_room_working_directory("room-1", invalid_path, original_version)
                .is_err());
            let unchanged = repository.snapshot("room-1").unwrap().room;
            assert_eq!(unchanged.working_directory, original_path);
            assert_eq!(unchanged.version, original_version);
        }

        let relative = repository
            .update_room_working_directory("room-1", "  relative-workspace  ", original_version)
            .unwrap();
        assert_eq!(
            Path::new(&relative.working_directory),
            relative_workspace.canonicalize().unwrap()
        );
        assert_eq!(relative.version, original_version + 1);

        let stale_error = repository
            .update_room_working_directory(
                "room-1",
                &absolute_workspace.path().to_string_lossy(),
                original_version,
            )
            .unwrap_err();
        assert!(matches!(
            stale_error,
            CollaborationError::VersionConflict {
                entity: "room",
                expected,
                actual,
                ..
            } if expected == original_version && actual == relative.version
        ));
        let unchanged = repository.snapshot("room-1").unwrap().room;
        assert_eq!(unchanged.working_directory, relative.working_directory);
        assert_eq!(unchanged.version, relative.version);

        let absolute = repository
            .update_room_working_directory(
                "room-1",
                &absolute_workspace.path().to_string_lossy(),
                relative.version,
            )
            .unwrap();
        assert_eq!(
            Path::new(&absolute.working_directory),
            absolute_workspace.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn invalid_room_working_directory_is_hidden_from_actor_without_configure_room() {
        let runtime = tempfile::tempdir().unwrap();
        let startup = tempfile::tempdir().unwrap();
        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            startup.path(),
        )
        .unwrap();
        repository.ensure_room("room-1", "Room", &[]).unwrap();
        repository
            .upsert_membership(
                &CollaborationActor::local(),
                "room-1",
                "viewer-1",
                RoomRole::Viewer,
                &[RoomCapability::RoomRead],
                None,
            )
            .unwrap();
        let before = repository.snapshot("room-1").unwrap().room;
        let missing_path = startup.path().join("secret-does-not-exist");

        let error = repository
            .update_room_working_directory_as(
                &CollaborationActor::new("viewer-1"),
                "room-1",
                &missing_path.to_string_lossy(),
                before.version,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CollaborationError::CapabilityDenied {
                capability: RoomCapability::ConfigureRoom,
                ..
            }
        ));
        let after = repository.snapshot("room-1").unwrap().room;
        assert_eq!(after.working_directory, before.working_directory);
        assert_eq!(after.version, before.version);
    }

    #[test]
    fn verified_instance_policies_refresh_template_allowlist_without_migrating_members() {
        let directory = tempfile::tempdir().unwrap();
        let initial =
            CollaborationRepository::new(directory.path(), CollaborationConfig::default()).unwrap();
        let room = ensure(&initial);
        let existing = initial
            .create_member("room-1", "保留的智脑", None, None)
            .unwrap();
        drop(initial);

        let config = CollaborationConfig::default().with_available_model_policies([
            String::from("gemini-2-5-flash"),
            String::from("deepseek-v4-pro"),
            String::from("gemini-2-5-flash"),
        ]);
        assert_eq!(config.default_model_policy, "main");
        let repository = CollaborationRepository::new(directory.path(), config).unwrap();
        let snapshot = repository.snapshot("room-1").unwrap();

        assert_eq!(
            snapshot.model_policies,
            vec!["main", "gemini-2-5-flash", "deepseek-v4-pro"]
        );
        assert_eq!(
            snapshot
                .members
                .iter()
                .find(|member| member.member_id == existing.member_id)
                .unwrap()
                .model_policy,
            "main"
        );

        let configured = repository
            .create_member("room-1", "Gemini 智脑", Some("gemini-2-5-flash"), None)
            .unwrap();
        assert_eq!(configured.model_policy, "gemini-2-5-flash");
        assert!(matches!(
            repository.configure_member(
                "room-1",
                &configured.member_id,
                "Gemini 智脑",
                "not-configured",
                &configured.reasoning_depth,
                configured.version,
            ),
            Err(CollaborationError::ModelPolicyNotAllowed(policy)) if policy == "not-configured"
        ));

        assert_eq!(room.room.default_member_id, snapshot.room.default_member_id);
    }

    fn acknowledge_pending_outbox(repository: &CollaborationRepository) {
        for event in repository.pending_outbox_events(100).unwrap() {
            repository
                .mark_outbox_published(&event.outbox_event_id, event.version)
                .unwrap();
        }
    }

    #[test]
    fn ensure_room_creates_one_durable_default_member_without_work() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        assert_eq!(snapshot.members.len(), 1);
        assert_eq!(snapshot.members[0].availability, MemberAvailability::Active);
        assert_eq!(snapshot.members[0].activity, MemberActivity::Idle);
        assert!(snapshot.inbox.is_empty());
        assert!(snapshot.events.is_empty());
    }

    #[test]
    fn legacy_messages_import_once_and_restore_default_member_history() {
        let (_directory, repository) = repository();
        let messages = vec![
            LegacyMessageSeed {
                id: "u1".into(),
                role: "user".into(),
                content: "第一问".into(),
                timestamp: Utc::now(),
                hidden: false,
            },
            LegacyMessageSeed {
                id: "a1".into(),
                role: "assistant".into(),
                content: "第一答".into(),
                timestamp: Utc::now(),
                hidden: false,
            },
        ];
        let first = repository
            .ensure_room("room-1", "Legacy", &messages)
            .unwrap();
        let second = repository
            .ensure_room("room-1", "Legacy", &messages)
            .unwrap();
        assert_eq!(first.events.len(), 2);
        assert_eq!(second.events.len(), 2);
        assert_eq!(
            second.events[0].recipients,
            vec![first.room.default_member_id]
        );
    }

    #[test]
    fn post_message_is_ordered_atomic_and_idempotent() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        let first = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "处理任务",
                RoomInputMode::Task,
                "command-1",
            )
            .unwrap();
        let duplicate = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "不会重复",
                RoomInputMode::Task,
                "command-1",
            )
            .unwrap();
        assert!(!first.duplicate);
        assert!(duplicate.duplicate);
        assert_eq!(first.event.event_id, duplicate.event.event_id);
        assert_eq!(first.inbox_items.len(), 1);
        assert_eq!(
            first.inbox_items[0].task_run_id.as_deref(),
            Some(format!("task-{}", first.inbox_items[0].inbox_item_id).as_str())
        );
        assert_eq!(
            duplicate.inbox_items[0].task_run_id,
            first.inbox_items[0].task_run_id
        );
        assert_eq!(repository.snapshot("room-1").unwrap().inbox.len(), 1);
    }

    #[test]
    fn one_member_serializes_while_different_members_can_claim() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&a),
                "A1",
                RoomInputMode::Chat,
                "a1",
            )
            .unwrap();
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&a),
                "A2",
                RoomInputMode::Chat,
                "a2",
            )
            .unwrap();
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&b),
                "B1",
                RoomInputMode::Chat,
                "b1",
            )
            .unwrap();

        let first = repository.claim_next().unwrap().unwrap();
        let second = repository.claim_next().unwrap().unwrap();
        assert_ne!(first.member_id, second.member_id);
        assert!(repository.claim_next().unwrap().is_none());
    }

    #[test]
    fn leased_item_is_queued_until_worker_admission_activates_it() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "等待统一准入",
                RoomInputMode::Task,
                "lease-command",
            )
            .unwrap();

        let lease = repository.lease_next().unwrap().unwrap();
        let queued = repository.snapshot("room-1").unwrap();
        assert_eq!(queued.inbox[0].state, InboxState::Leased);
        assert_eq!(queued.members[0].activity, MemberActivity::Queued);
        assert_eq!(queued.members[0].pending_count, 1);
        assert!(queued.members[0].active_run_id.is_none());
        assert!(repository.lease_next().unwrap().is_none());

        let active = repository.activate_lease(&lease).unwrap();
        assert_eq!(active.version, lease.version + 1);
        let running = repository.snapshot("room-1").unwrap();
        assert_eq!(running.inbox[0].state, InboxState::Running);
        assert_eq!(running.members[0].activity, MemberActivity::Running);
        assert_eq!(
            running.members[0].active_run_id.as_deref(),
            Some(active.run_id.as_str())
        );
    }

    #[test]
    fn stale_lease_release_does_not_emit_room_change() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        acknowledge_pending_outbox(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "等待准入",
                RoomInputMode::Task,
                "stale-release-command",
            )
            .unwrap();
        let lease = repository.lease_next().unwrap().unwrap();
        let active = repository.activate_lease(&lease).unwrap();
        acknowledge_pending_outbox(&repository);

        repository.release_lease(&lease).unwrap();

        assert!(repository.pending_outbox_events(100).unwrap().is_empty());
        let item = &repository.snapshot("room-1").unwrap().inbox[0];
        assert_eq!(item.state, InboxState::Running);
        assert_eq!(item.version, active.version);
    }

    #[test]
    fn active_retry_release_tolerates_interrupt_version_bump() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "中断与失败结算并发",
                RoomInputMode::Task,
                "interrupt-before-retry-release",
            )
            .unwrap();
        let leased = repository.lease_next().unwrap().unwrap();
        let active = repository.activate_lease(&leased).unwrap();
        repository
            .request_interrupt("room-1", &member_id, &active.run_id)
            .unwrap();

        repository.release_active_for_retry(&active).unwrap();

        let item = repository
            .snapshot("room-1")
            .unwrap()
            .inbox
            .into_iter()
            .find(|item| item.inbox_item_id == active.inbox_item_id)
            .unwrap();
        assert_eq!(item.state, InboxState::Pending);
        assert!(item.run_id.is_none());
    }

    #[test]
    fn durable_task_result_reconciliation_appends_reply_once() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        let posted = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "需要可恢复的回复",
                RoomInputMode::Task,
                "reconcile-command",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();

        let event = repository
            .reconcile_completed_item(
                &posted.inbox_items[0].inbox_item_id,
                &claim.run_id,
                "已经持久化的回复",
            )
            .unwrap()
            .unwrap();
        assert_eq!(event.content, "已经持久化的回复");
        assert!(repository
            .reconcile_completed_item(
                &posted.inbox_items[0].inbox_item_id,
                &claim.run_id,
                "已经持久化的回复",
            )
            .unwrap()
            .is_none());
        let snapshot = repository.snapshot("room-1").unwrap();
        assert_eq!(snapshot.inbox[0].state, InboxState::Completed);
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| event.kind == "member_message")
                .count(),
            1
        );
    }

    #[test]
    fn durable_task_result_reconciliation_preserves_reply_metadata() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        let posted = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "需要恢复完整回复上下文的任务",
                RoomInputMode::Task,
                "reconcile-metadata-command",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();
        assert_eq!(claim.response_to_event_id, posted.event.event_id);
        assert_eq!(claim.mode, RoomInputMode::Task);
        assert!(claim.group_enabled);

        let event = repository
            .reconcile_completed_item(
                &posted.inbox_items[0].inbox_item_id,
                &claim.run_id,
                "恢复后仍属于原任务线程的回复",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            event.parent_event_id.as_deref(),
            Some(claim.response_to_event_id.as_str())
        );
        assert_eq!(
            event.conversation_root_event_id,
            posted.event.conversation_root_event_id
        );
        assert_eq!(event.debate_depth, posted.event.debate_depth + 1);
        assert!(event.group_enabled);
        assert_eq!(event.conversation_mode, RoomInputMode::Task);
        assert_eq!(
            event.reply_reference,
            Some(RoomEventReferenceView {
                event_id: posted.event.event_id.clone(),
                sequence: posted.event.sequence,
                sender_kind: posted.event.sender_kind.clone(),
                sender_id: posted.event.sender_id.clone(),
                sender_name: posted.event.sender_name.clone(),
                kind: posted.event.kind.clone(),
                content: posted.event.content.clone(),
                content_hash: history_content_hash(&posted.event.content),
                created_at: posted.event.created_at,
            })
        );
        let persisted_directory: String = repository
            .connect()
            .unwrap()
            .query_row(
                "SELECT execution_working_directory FROM room_events WHERE event_id = ?1",
                [event.event_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            PathBuf::from(persisted_directory),
            claim.execution_working_directory
        );

        assert!(repository
            .reconcile_completed_item(
                &posted.inbox_items[0].inbox_item_id,
                &claim.run_id,
                "重复恢复不应新增回复",
            )
            .unwrap()
            .is_none());
        assert_eq!(
            repository
                .snapshot("room-1")
                .unwrap()
                .events
                .iter()
                .filter(|event| event.kind == "member_message")
                .count(),
            1
        );
    }

    #[test]
    fn retry_last_user_event_keeps_user_and_invalidates_later_results() {
        let (directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        let posted = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "请重新回答",
                RoomInputMode::Chat,
                "retry-source",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();
        repository
            .reconcile_completed_item(
                &posted.inbox_items[0].inbox_item_id,
                &claim.run_id,
                "旧回答",
            )
            .unwrap();

        let retried = repository
            .retry_last_user_event("room-1", &posted.event.event_id)
            .unwrap();

        assert_eq!(
            retried
                .events
                .iter()
                .filter(|event| event.sender_kind == "user")
                .count(),
            1
        );
        assert_eq!(retried.events[0].event_id, posted.event.event_id);
        assert!(retried.events.iter().all(|event| event.content != "旧回答"));
        assert_eq!(
            retried
                .inbox
                .iter()
                .filter(|item| item.source_event_id == posted.event.event_id)
                .filter(|item| item.state == InboxState::Pending)
                .count(),
            1
        );

        drop(repository);
        let reopened =
            CollaborationRepository::new(directory.path(), CollaborationConfig::default()).unwrap();
        assert!(reopened
            .snapshot("room-1")
            .unwrap()
            .events
            .iter()
            .all(|event| event.content != "旧回答"));
    }

    #[test]
    fn retry_rejects_a_user_event_that_is_not_last() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        let first = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "第一条",
                RoomInputMode::Chat,
                "retry-first",
            )
            .unwrap();
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "第二条",
                RoomInputMode::Chat,
                "retry-second",
            )
            .unwrap();
        let before = repository.snapshot("room-1").unwrap();

        let error = repository
            .retry_last_user_event("room-1", &first.event.event_id)
            .unwrap_err();

        assert!(error.to_string().contains("最后一条用户消息"));
        assert_eq!(
            repository.snapshot("room-1").unwrap().events.len(),
            before.events.len()
        );
    }

    #[test]
    fn retry_rejects_a_late_result_from_the_superseded_run() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let posted = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "重新执行",
                RoomInputMode::Chat,
                "retry-late-result",
            )
            .unwrap();
        let old_claim = repository.claim_next().unwrap().unwrap();

        repository
            .retry_last_user_event("room-1", &posted.event.event_id)
            .unwrap();
        let error = repository
            .reconcile_completed_item(
                &old_claim.inbox_item_id,
                &old_claim.run_id,
                "不应写回的旧结果",
            )
            .unwrap_err();

        assert!(matches!(error, CollaborationError::RunNotActive(_)));
        assert!(repository
            .snapshot("room-1")
            .unwrap()
            .events
            .iter()
            .all(|event| event.content != "不应写回的旧结果"));
    }

    #[test]
    fn cancelled_result_reconciliation_emits_one_room_change() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "稍后取消",
                RoomInputMode::Task,
                "cancelled-reconcile-command",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();
        repository
            .request_interrupt("room-1", &claim.member_id, &claim.run_id)
            .unwrap();
        acknowledge_pending_outbox(&repository);
        let interrupted_version = repository.snapshot("room-1").unwrap().inbox[0].version;

        assert!(repository
            .reconcile_completed_item(&claim.inbox_item_id, &claim.run_id, "晚到的完成产物")
            .unwrap()
            .is_none());
        let cancelled = repository.snapshot("room-1").unwrap().inbox[0].clone();
        assert_eq!(cancelled.state, InboxState::Cancelled);
        assert_eq!(cancelled.version, interrupted_version + 1);
        assert_eq!(repository.pending_outbox_events(100).unwrap().len(), 1);

        acknowledge_pending_outbox(&repository);
        assert!(repository
            .reconcile_completed_item(&claim.inbox_item_id, &claim.run_id, "晚到的完成产物")
            .unwrap()
            .is_none());
        assert_eq!(
            repository.snapshot("room-1").unwrap().inbox[0].version,
            cancelled.version
        );
        assert!(repository.pending_outbox_events(100).unwrap().is_empty());
    }

    #[test]
    fn sleep_after_current_stops_new_claims_and_settles_after_completion() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "正在执行",
                RoomInputMode::Chat,
                "run",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();
        let sleeping = repository.sleep_member("room-1", &member_id).unwrap();
        assert_eq!(sleeping.availability, MemberAvailability::SleepAfterCurrent);
        let event = repository.complete_item(&claim, "完成").unwrap().unwrap();
        assert_eq!(event.sender_id, member_id);
        assert_eq!(
            repository
                .member("room-1", &member_id)
                .unwrap()
                .availability,
            MemberAvailability::Sleeping
        );
    }

    #[test]
    fn blank_member_reply_cannot_complete_inbox_item() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "需要有效回复",
                RoomInputMode::Chat,
                "blank-reply",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();

        let error = repository.complete_item(&claim, " \n\t").unwrap_err();

        assert!(matches!(error, CollaborationError::EmptyMemberReply));
        let persisted = repository.snapshot("room-1").unwrap();
        assert_eq!(persisted.inbox[0].state, InboxState::Running);
        assert!(persisted
            .events
            .iter()
            .all(|event| event.kind != "member_message"));
    }

    #[test]
    fn recovery_requeues_running_item_without_restoring_runtime() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "崩溃前任务",
                RoomInputMode::Task,
                "crash",
            )
            .unwrap();
        let first = repository.claim_next().unwrap().unwrap();
        assert_eq!(repository.recover_inflight().unwrap(), 1);
        let recovered = repository.claim_next().unwrap().unwrap();
        assert_eq!(first.inbox_item_id, recovered.inbox_item_id);
        assert_ne!(first.run_id, recovered.run_id);
    }

    #[test]
    fn interrupt_is_persisted_before_the_runtime_cancels() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "停止前先落库",
                RoomInputMode::Task,
                "interrupt",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();
        assert!(!repository.interrupt_requested(&claim).unwrap());

        repository
            .request_interrupt("room-1", &claim.member_id, &claim.run_id)
            .unwrap();
        assert!(repository.interrupt_requested(&claim).unwrap());
        repository.fail_item(&claim, "cancelled").unwrap();

        let snapshot = repository.snapshot("room-1").unwrap();
        let item = snapshot
            .inbox
            .iter()
            .find(|item| item.inbox_item_id == claim.inbox_item_id)
            .unwrap();
        assert_eq!(item.state, InboxState::Cancelled);
    }

    #[test]
    fn member_history_is_private_to_the_addressed_member() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&a),
                "A 的秘密",
                RoomInputMode::Chat,
                "a-secret",
            )
            .unwrap();
        let claim_a = repository.claim_next().unwrap().unwrap();
        repository.complete_item(&claim_a, "A 的回复").unwrap();
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&b),
                "B 的问题",
                RoomInputMode::Chat,
                "b-question",
            )
            .unwrap();
        let claim_b = repository.claim_next().unwrap().unwrap();
        let history = repository.member_history(&claim_b).unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn group_member_history_keeps_three_latest_user_messages_and_own_replies() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;

        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "用户消息 1",
                RoomInputMode::Chat,
                "history-1",
            )
            .unwrap();
        let first = repository.claim_next().unwrap().unwrap();
        repository
            .complete_item(&first, "A 对消息 1 的回复")
            .unwrap();

        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&b),
                "用户消息 2",
                RoomInputMode::Chat,
                "history-2",
            )
            .unwrap();
        let second = repository.claim_next().unwrap().unwrap();
        repository
            .complete_item(&second, "B 对消息 2 的回复")
            .unwrap();

        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "用户消息 3",
                RoomInputMode::Chat,
                "history-3",
            )
            .unwrap();
        let third = repository.claim_next().unwrap().unwrap();
        repository
            .complete_item(&third, "A 对消息 3 的回复")
            .unwrap();

        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&b),
                "用户消息 4",
                RoomInputMode::Chat,
                "history-4",
            )
            .unwrap();
        let fourth = repository.claim_next().unwrap().unwrap();
        repository
            .complete_item(&fourth, "B 对消息 4 的回复")
            .unwrap();

        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "用户消息 5",
                RoomInputMode::Chat,
                "history-5",
            )
            .unwrap();
        let fifth = repository.claim_next().unwrap().unwrap();
        assert_eq!(fifth.member_id, a);
        assert_eq!(fifth.input, "用户消息 5");

        let history = repository.member_history(&fifth).unwrap();
        assert_eq!(history.len(), 3);
        assert!(history
            .iter()
            .any(|message| message.content.contains("用户消息 3")));
        assert!(history
            .iter()
            .any(|message| message.content.contains("A 对消息 3 的回复")));
        assert!(history
            .iter()
            .any(|message| message.content.contains("用户消息 4")));
        for unexpected in [
            "用户消息 1",
            "A 对消息 1 的回复",
            "用户消息 2",
            "B 对消息 2 的回复",
            "B 对消息 4 的回复",
        ] {
            assert!(!history
                .iter()
                .any(|message| message.content.contains(unexpected)));
        }
    }

    #[test]
    fn events_through_stops_at_the_claim_context_boundary() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let first = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "边界内消息",
                RoomInputMode::Chat,
                "tool-boundary-1",
            )
            .unwrap();
        let second = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "边界外消息",
                RoomInputMode::Chat,
                "tool-boundary-2",
            )
            .unwrap();

        let events = repository
            .events_through("room-1", first.event.sequence, 10)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, first.event.event_id);
        assert!(!events
            .iter()
            .any(|event| event.event_id == second.event.event_id));
    }

    #[test]
    fn group_message_only_queues_explicit_recipients() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        let c = repository
            .create_member("room-1", "智脑 C", None, None)
            .unwrap()
            .member_id;

        let posted = repository
            .post_group_message(
                "room-1",
                &[a.clone(), b.clone()],
                "@A @B 只唤醒指定实例",
                RoomInputMode::Chat,
                "addressed-only",
            )
            .unwrap();

        let mut recipients = posted.event.recipients.clone();
        recipients.sort();
        let mut expected_recipients = vec![a.clone(), b.clone()];
        expected_recipients.sort();
        assert_eq!(recipients, expected_recipients);
        assert!(posted.event.audience.is_empty());
        assert_eq!(posted.inbox_items.len(), 2);
        assert!(posted
            .inbox_items
            .iter()
            .all(|item| item.purpose == InboxPurpose::Direct));
        assert!(posted.inbox_items.iter().all(|item| item.member_id != c));
        assert!(repository.claim_next().unwrap().is_some());
        assert!(repository.claim_next().unwrap().is_some());
        assert!(repository.claim_next().unwrap().is_none());
    }

    #[test]
    fn active_member_display_names_are_unique_within_a_room() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap();

        assert!(matches!(
            repository.create_member("room-1", "智脑 B", None, None),
            Err(CollaborationError::DuplicateMemberName(name)) if name == "智脑 B"
        ));
        assert!(matches!(
            repository.configure_member(
                "room-1",
                &b.member_id,
                &snapshot
                    .members
                    .iter()
                    .find(|member| member.member_id == a)
                    .unwrap()
                    .display_name,
                &b.model_policy,
                &b.reasoning_depth,
                b.version,
            ),
            Err(CollaborationError::DuplicateMemberName(_))
        ));
        repository.archive_member("room-1", &b.member_id).unwrap();
        repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap();
        assert!(matches!(
            repository.restore_member("room-1", &b.member_id),
            Err(CollaborationError::DuplicateMemberName(name)) if name == "智脑 B"
        ));
    }

    #[test]
    fn version_five_migration_normalizes_duplicate_active_member_names() {
        let directory = tempfile::tempdir().unwrap();
        let repository =
            CollaborationRepository::new(directory.path(), CollaborationConfig::default()).unwrap();
        ensure(&repository);
        let first = repository
            .create_member("room-1", "重复名称", None, None)
            .unwrap();
        let second = repository
            .create_member("room-1", "待替换名称", None, None)
            .unwrap();
        let connection = repository.connect().unwrap();
        connection
            .execute("DROP INDEX brain_members_active_display_name_idx", [])
            .unwrap();
        connection
            .execute(
                "UPDATE brain_members SET display_name = '重复名称' WHERE member_id = ?1",
                [&second.member_id],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE collaboration_schema SET version = 5 WHERE singleton = 1",
                [],
            )
            .unwrap();
        drop(connection);

        let migrated =
            CollaborationRepository::new(directory.path(), CollaborationConfig::default())
                .unwrap()
                .snapshot("room-1")
                .unwrap();
        assert!(
            migrated
                .members
                .iter()
                .any(|member| member.member_id == first.member_id
                    && member.display_name == "重复名称")
        );
        assert!(migrated.members.iter().any(|member| {
            member.member_id == second.member_id && member.display_name == "重复名称 (2)"
        }));
    }

    #[test]
    fn direct_member_reply_is_appended_without_waking_other_members() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "只问 A",
                RoomInputMode::Chat,
                "only-a",
            )
            .unwrap();

        let claim = repository.claim_next().unwrap().unwrap();
        let reply = repository
            .complete_item(&claim, "A 的答复")
            .unwrap()
            .unwrap();
        assert!(reply.audience.is_empty());
        assert!(reply.recipients.is_empty());
        assert!(repository.claim_next().unwrap().is_none());
        assert!(!repository
            .snapshot("room-1")
            .unwrap()
            .inbox
            .iter()
            .any(|item| item.member_id == b));
    }

    #[test]
    fn group_message_queues_only_explicit_recipients() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        let c = repository
            .create_member("room-1", "智脑 C", None, None)
            .unwrap()
            .member_id;

        let posted = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "大家怎么看这个方案？",
                RoomInputMode::Chat,
                "group-message",
            )
            .unwrap();

        assert_eq!(posted.event.recipients, vec![a.clone()]);
        assert!(posted.event.audience.is_empty());
        assert_eq!(posted.inbox_items.len(), 1);
        assert_eq!(
            posted
                .inbox_items
                .iter()
                .filter(|item| item.purpose == InboxPurpose::Direct)
                .count(),
            1
        );
        let room = repository.snapshot("room-1").unwrap();
        let deliveries = room
            .deliveries
            .iter()
            .filter(|delivery| delivery.event_id == posted.event.event_id)
            .collect::<Vec<_>>();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].member_id, a);
        assert!(!room.inbox.iter().any(|item| item.member_id == b));
        assert!(!room.inbox.iter().any(|item| item.member_id == c));
    }

    #[test]
    fn unmentioned_member_does_not_receive_a_group_message() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        let posted = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "只唤醒被 @ 的实例",
                RoomInputMode::Chat,
                "only-mentioned",
            )
            .unwrap();

        let direct = repository.claim_next().unwrap().unwrap();
        assert_eq!(direct.member_id, a);
        assert_eq!(direct.purpose, InboxPurpose::Direct);
        let persisted = repository.snapshot("room-1").unwrap();
        assert!(!persisted.inbox.iter().any(|item| item.member_id == b));
        assert!(!persisted.deliveries.iter().any(|delivery| {
            delivery.event_id == posted.event.event_id && delivery.member_id == b
        }));
        assert!(repository.claim_next().unwrap().is_none());
    }

    #[test]
    fn member_reply_is_public_but_does_not_wake_other_members() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        let c = repository
            .create_member("room-1", "智脑 C", None, None)
            .unwrap()
            .member_id;
        let posted = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "评估发布方案",
                RoomInputMode::Chat,
                "group-rebuttal",
            )
            .unwrap();

        let claim_a = repository.claim_next().unwrap().unwrap();
        assert_eq!(claim_a.member_id, a);
        let reply = repository
            .complete_item(&claim_a, "我建议先灰度发布")
            .unwrap()
            .unwrap();
        assert!(reply.recipients.is_empty());
        assert!(reply.audience.is_empty());
        assert_eq!(reply.conversation_root_event_id, posted.event.event_id);
        assert_eq!(
            reply.parent_event_id.as_deref(),
            Some(posted.event.event_id.as_str())
        );
        assert_eq!(reply.debate_depth, 1);

        let persisted = repository.snapshot("room-1").unwrap();
        assert!(persisted
            .events
            .iter()
            .any(|event| event.event_id == reply.event_id));
        assert!(!persisted.inbox.iter().any(|item| item.member_id == b));
        assert!(!persisted.inbox.iter().any(|item| item.member_id == c));
        assert!(repository.claim_next().unwrap().is_none());
    }

    #[test]
    fn later_direct_group_work_excludes_other_members_replies() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&b),
                "第一轮",
                RoomInputMode::Chat,
                "group-history-1",
            )
            .unwrap();
        let claim_b = repository.claim_next().unwrap().unwrap();
        assert_eq!(claim_b.member_id, b);
        repository.complete_item(&claim_b, "B 补充了风险").unwrap();
        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "第二轮",
                RoomInputMode::Chat,
                "group-history-2",
            )
            .unwrap();

        let later_direct = repository.claim_next().unwrap().unwrap();
        assert_eq!(later_direct.member_id, a);
        assert_eq!(later_direct.purpose, InboxPurpose::Direct);
        assert!(later_direct.group_enabled);
        let history = repository.member_history(&later_direct).unwrap();
        assert!(!history
            .iter()
            .any(|message| message.content.contains("B 补充了风险")));
    }

    #[test]
    fn busy_unmentioned_member_is_not_queued_for_group_work() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&b),
                "B 正在执行的任务",
                RoomInputMode::Task,
                "busy-b",
            )
            .unwrap();
        let busy = repository.claim_next().unwrap().unwrap();
        assert_eq!(busy.member_id, b);

        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "新的群聊观点",
                RoomInputMode::Chat,
                "while-busy",
            )
            .unwrap();
        let claim_a = repository.claim_next().unwrap().unwrap();
        assert_eq!(claim_a.member_id, a);
        assert!(repository.claim_next().unwrap().is_none());

        repository.complete_item(&busy, "B 原任务完成").unwrap();
        assert!(repository.claim_next().unwrap().is_none());
    }

    #[test]
    fn recovered_group_result_keeps_durable_run_identity_and_delivery() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&a),
                "恢复后继续群聊",
                RoomInputMode::Chat,
                "group-recovery",
            )
            .unwrap();
        let leased = repository.lease_next().unwrap().unwrap();

        let completion = repository
            .reconcile_claim_result(&leased, "durable-instance-run", Some("恢复的观点"))
            .unwrap();
        let event = completion.event.unwrap();
        assert_eq!(event.run_id.as_deref(), Some("durable-instance-run"));
        assert!(event.audience.is_empty());
        let persisted = repository.snapshot("room-1").unwrap();
        let item = persisted
            .inbox
            .iter()
            .find(|item| item.inbox_item_id == leased.inbox_item_id)
            .unwrap();
        assert_eq!(item.run_id.as_deref(), Some("durable-instance-run"));
        assert_eq!(item.state, InboxState::Completed);
        assert!(!persisted.inbox.iter().any(|item| item.member_id == b));
    }

    #[test]
    fn failed_durable_projection_releases_the_running_transition_for_retry() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "恢复一个无效的持久结果",
                RoomInputMode::Task,
                "invalid-durable-projection",
            )
            .unwrap();
        let leased = repository.lease_next().unwrap().unwrap();

        let error = repository
            .reconcile_claim_result(&leased, "invalid-durable-run", Some("   "))
            .unwrap_err();

        assert!(matches!(error, CollaborationError::EmptyMemberReply));
        let persisted = repository.snapshot("room-1").unwrap();
        let item = persisted
            .inbox
            .iter()
            .find(|item| item.inbox_item_id == leased.inbox_item_id)
            .unwrap();
        assert_eq!(item.state, InboxState::Pending);
        assert!(item.run_id.is_none());
        let retried = repository.lease_next().unwrap().unwrap();
        assert_eq!(retried.inbox_item_id, leased.inbox_item_id);
        assert_ne!(retried.run_id, "invalid-durable-run");
    }

    #[test]
    fn failed_inbox_accepts_valid_authoritative_durable_result() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "恢复历史失败任务的权威结果",
                RoomInputMode::Task,
                "failed-authoritative-result",
            )
            .unwrap();
        let leased = repository.lease_next().unwrap().unwrap();
        let active = repository.activate_lease(&leased).unwrap();
        repository.fail_item(&active, "历史执行失败").unwrap();
        let durable_run_id = "durable-failed-authoritative-run";
        let recovered = repository
            .claim_for_reconciliation(&active.inbox_item_id, &active.task_run_id, durable_run_id)
            .unwrap()
            .unwrap();

        let completion = repository
            .reconcile_claim_result(&recovered, durable_run_id, Some("权威持久回复"))
            .unwrap();

        assert_eq!(completion.disposition, ParticipationDisposition::Replied);
        assert_eq!(completion.event.unwrap().content, "权威持久回复");
        let item = repository
            .snapshot("room-1")
            .unwrap()
            .inbox
            .into_iter()
            .find(|item| item.inbox_item_id == active.inbox_item_id)
            .unwrap();
        assert_eq!(item.state, InboxState::Completed);
        assert_eq!(item.run_id.as_deref(), Some(durable_run_id));
        assert!(item.error.is_none());
    }

    #[test]
    fn failed_inbox_projection_error_restores_failed_state_and_run() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "无效权威结果不得覆盖历史失败态",
                RoomInputMode::Task,
                "failed-invalid-authoritative-result",
            )
            .unwrap();
        let leased = repository.lease_next().unwrap().unwrap();
        let active = repository.activate_lease(&leased).unwrap();
        repository.fail_item(&active, "需要保留的历史错误").unwrap();
        let durable_run_id = "durable-failed-invalid-run";
        let recovered = repository
            .claim_for_reconciliation(&active.inbox_item_id, &active.task_run_id, durable_run_id)
            .unwrap()
            .unwrap();

        let error = repository
            .reconcile_claim_result(&recovered, durable_run_id, Some("   "))
            .unwrap_err();

        assert!(matches!(error, CollaborationError::EmptyMemberReply));
        let snapshot = repository.snapshot("room-1").unwrap();
        let item = snapshot
            .inbox
            .iter()
            .find(|item| item.inbox_item_id == active.inbox_item_id)
            .unwrap();
        assert_eq!(item.state, InboxState::Failed);
        assert_eq!(item.run_id.as_deref(), Some(active.run_id.as_str()));
        assert_eq!(item.error.as_deref(), Some("需要保留的历史错误"));
        assert!(snapshot
            .events
            .iter()
            .all(|event| event.kind != "member_message"));
    }

    #[test]
    fn failed_reconciliation_rollback_never_overwrites_a_newer_transition() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "旧回滚不得覆盖更新转换",
                RoomInputMode::Task,
                "failed-rollback-cas",
            )
            .unwrap();
        let leased = repository.lease_next().unwrap().unwrap();
        let active = repository.activate_lease(&leased).unwrap();
        repository.fail_item(&active, "需要保留的原始失败").unwrap();
        let durable_run_id = "durable-rollback-cas";
        let recovered = repository
            .claim_for_reconciliation(&active.inbox_item_id, &active.task_run_id, durable_run_id)
            .unwrap()
            .unwrap();
        let connection = Connection::open(repository.database_path()).unwrap();
        connection
            .execute(
                "UPDATE member_inbox_items
                 SET state = 'running', run_id = ?1, version = version + 1
                 WHERE inbox_item_id = ?2 AND state = 'failed' AND version = ?3",
                params![durable_run_id, active.inbox_item_id, recovered.version],
            )
            .unwrap();
        let mut durable_claim = recovered;
        durable_claim.version += 1;
        connection
            .execute(
                "UPDATE member_inbox_items SET version = version + 1
                 WHERE inbox_item_id = ?1 AND state = 'running' AND run_id = ?2",
                params![active.inbox_item_id, durable_run_id],
            )
            .unwrap();

        let error = repository
            .restore_failed_reconciliation(&durable_claim, Some(&active.run_id))
            .unwrap_err();

        assert!(matches!(error, CollaborationError::VersionConflict { .. }));
        let (state, run_id, version): (String, String, u64) = connection
            .query_row(
                "SELECT state, run_id, version FROM member_inbox_items
                 WHERE inbox_item_id = ?1",
                [&active.inbox_item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, "running");
        assert_eq!(run_id, durable_run_id);
        assert_eq!(version, durable_claim.version + 1);
    }

    #[test]
    fn durable_reconciliation_never_replaces_a_different_active_run() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "活动运行不得被旧持久结果覆盖",
                RoomInputMode::Task,
                "different-active-run",
            )
            .unwrap();
        let leased = repository.lease_next().unwrap().unwrap();
        let active = repository.activate_lease(&leased).unwrap();
        let stale_durable_run = "stale-durable-run";
        let recovered = repository
            .claim_for_reconciliation(
                &active.inbox_item_id,
                &active.task_run_id,
                stale_durable_run,
            )
            .unwrap()
            .unwrap();

        let error = repository
            .reconcile_claim_result(&recovered, stale_durable_run, Some("迟到回复"))
            .unwrap_err();

        assert!(matches!(error, CollaborationError::RunNotActive(_)));
        let snapshot = repository.snapshot("room-1").unwrap();
        let item = snapshot
            .inbox
            .iter()
            .find(|item| item.inbox_item_id == active.inbox_item_id)
            .unwrap();
        assert_eq!(item.state, InboxState::Running);
        assert_eq!(item.run_id.as_deref(), Some(active.run_id.as_str()));
        assert!(snapshot
            .events
            .iter()
            .all(|event| event.kind != "member_message"));
    }

    #[test]
    fn explicitly_mentioned_busy_member_receives_one_pending_direct_item() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let a = snapshot.room.default_member_id;
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&b),
                "B 正在执行的任务",
                RoomInputMode::Task,
                "busy-b",
            )
            .unwrap();
        let busy = repository.claim_next().unwrap().unwrap();
        assert_eq!(busy.member_id, b);

        let posted = repository
            .post_group_message(
                "room-1",
                &[a.clone(), b.clone()],
                "同时 @ A 和 B",
                RoomInputMode::Chat,
                "direct-while-busy",
            )
            .unwrap();
        assert_eq!(posted.inbox_items.len(), 2);

        let claim_a = repository.claim_next().unwrap().unwrap();
        assert_eq!(claim_a.member_id, a);
        assert!(repository.claim_next().unwrap().is_none());

        repository.complete_item(&busy, "B 原任务完成").unwrap();
        let persisted = repository.snapshot("room-1").unwrap();
        let b_items = persisted
            .inbox
            .iter()
            .filter(|item| item.member_id == b && item.source_event_id == posted.event.event_id)
            .collect::<Vec<_>>();
        assert_eq!(b_items.len(), 1);
        assert_eq!(b_items[0].purpose, InboxPurpose::Direct);
        assert_eq!(b_items[0].state, InboxState::Pending);
    }

    #[test]
    fn direct_group_messages_ignore_legacy_debate_limit() {
        let directory = tempfile::tempdir().unwrap();
        let repository = CollaborationRepository::new(
            directory.path(),
            CollaborationConfig {
                max_members_per_room: 3,
                max_pending_items_per_member: 4,
                max_pending_items_per_room: 8,
                max_recipients_per_message: 3,
                max_group_replies_per_member: 1,
                ..CollaborationConfig::default()
            },
        )
        .unwrap();
        ensure(&repository);
        let b = repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap()
            .member_id;
        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&b),
                "第一条定向消息",
                RoomInputMode::Chat,
                "group-limit",
            )
            .unwrap();
        let first = repository.claim_next().unwrap().unwrap();
        assert_eq!(first.member_id, b);
        repository.complete_item(&first, "B 的第一条回复").unwrap();

        let second_post = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&b),
                "第二条定向消息",
                RoomInputMode::Chat,
                "group-limit-2",
            )
            .unwrap();
        let second = repository.claim_next().unwrap().unwrap();
        assert_eq!(second.member_id, b);
        assert_eq!(second.purpose, InboxPurpose::Direct);
        repository.complete_item(&second, "B 的第二条回复").unwrap();

        let persisted = repository.snapshot("room-1").unwrap();
        assert_eq!(
            persisted
                .inbox
                .iter()
                .filter(|item| item.source_event_id == second_post.event.event_id)
                .count(),
            1
        );
        assert!(persisted
            .deliveries
            .iter()
            .all(|delivery| delivery.kind == DeliveryKind::Direct));
    }

    fn create_schema_six_working_directory_fixture(database_path: &Path) {
        let connection = Connection::open(database_path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE collaboration_schema (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     version INTEGER NOT NULL
                 );
                 INSERT INTO collaboration_schema(singleton, version) VALUES (1, 6);
                 CREATE TABLE collaboration_rooms (
                     room_id TEXT PRIMARY KEY,
                     workspace_id TEXT NOT NULL DEFAULT 'local',
                     title TEXT NOT NULL,
                     default_member_id TEXT NOT NULL,
                     latest_event_seq INTEGER NOT NULL DEFAULT 0,
                     room_summary_ref TEXT,
                     room_summary_through_seq INTEGER NOT NULL DEFAULT 0,
                     version INTEGER NOT NULL DEFAULT 1,
                     created_at TEXT NOT NULL
                 );
                 CREATE TABLE room_events (
                     event_id TEXT PRIMARY KEY,
                     room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id),
                     sequence INTEGER NOT NULL,
                     sender_kind TEXT NOT NULL,
                     sender_id TEXT NOT NULL,
                     sender_name TEXT NOT NULL,
                     visibility TEXT NOT NULL DEFAULT 'room',
                     kind TEXT NOT NULL,
                     content TEXT NOT NULL,
                     run_id TEXT,
                     parent_event_id TEXT,
                     conversation_root_event_id TEXT,
                     debate_depth INTEGER NOT NULL DEFAULT 0,
                     group_enabled INTEGER NOT NULL DEFAULT 0,
                     conversation_mode TEXT NOT NULL DEFAULT 'chat',
                     idempotency_key TEXT NOT NULL,
                     created_at TEXT NOT NULL,
                     invalidated_at TEXT,
                     UNIQUE(room_id, sequence),
                     UNIQUE(room_id, idempotency_key)
                 );
                 CREATE TABLE room_principal_memberships (
                     room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id) ON DELETE CASCADE,
                     principal_id TEXT NOT NULL,
                     role TEXT NOT NULL,
                     capabilities_json TEXT NOT NULL,
                     capability_version INTEGER NOT NULL DEFAULT 1,
                     version INTEGER NOT NULL DEFAULT 1,
                     created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL,
                     PRIMARY KEY(room_id, principal_id)
                 );",
            )
            .unwrap();
        let legacy_time = "2026-08-08T00:00:00Z";
        connection
            .execute(
                "INSERT INTO collaboration_rooms(
                     room_id, workspace_id, title, default_member_id, latest_event_seq,
                     room_summary_ref, room_summary_through_seq, version, created_at
                 ) VALUES ('legacy-room', 'local', '旧房间', 'legacy-member', 2, NULL, 0, 11, ?1)",
                [legacy_time],
            )
            .unwrap();
        for (event_id, sequence, sender_kind, sender_id, kind, content) in [
            (
                "legacy-user",
                1_u64,
                "user",
                "user",
                "user_message",
                "旧用户消息",
            ),
            (
                "legacy-member-event",
                2_u64,
                "member",
                "legacy-member",
                "member_message",
                "旧成员消息",
            ),
        ] {
            connection
                .execute(
                    "INSERT INTO room_events(
                         event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                         visibility, kind, content, run_id, parent_event_id,
                         conversation_root_event_id, debate_depth, group_enabled,
                         conversation_mode, idempotency_key, created_at, invalidated_at
                     ) VALUES (
                         ?1, 'legacy-room', ?2, ?3, ?4, ?4, 'room', ?5, ?6, NULL,
                         NULL, ?1, 0, 0, 'chat', ?1, ?7, NULL
                     )",
                    params![
                        event_id,
                        sequence,
                        sender_kind,
                        sender_id,
                        kind,
                        content,
                        legacy_time,
                    ],
                )
                .unwrap();
        }
        let legacy_capabilities = serialize_capabilities(&[
            RoomCapability::RoomRead,
            RoomCapability::RoomPost,
            RoomCapability::MentionMember,
            RoomCapability::SubmitTask,
            RoomCapability::CreateMember,
            RoomCapability::ConfigureMember,
            RoomCapability::WakeSleepMember,
            RoomCapability::ArchiveRestoreMember,
            RoomCapability::InterruptOwnRun,
            RoomCapability::InterruptAnyRun,
            RoomCapability::OverrideMemberModel,
            RoomCapability::OverrideMemberReasoning,
            RoomCapability::ManageMembership,
        ])
        .unwrap();
        connection
            .execute(
                "INSERT INTO room_principal_memberships(
                     room_id, principal_id, role, capabilities_json, capability_version,
                     version, created_at, updated_at
                 ) VALUES ('legacy-room', ?1, 'owner', ?2, 1, 4, ?3, ?3)",
                params![LOCAL_PRINCIPAL_ID, legacy_capabilities, legacy_time],
            )
            .unwrap();
    }

    #[test]
    fn migrates_working_directory_and_owner_capability_from_schema_six() {
        let runtime = tempfile::tempdir().unwrap();
        let startup = tempfile::tempdir().unwrap();
        let canonical_startup = startup.path().canonicalize().unwrap().display().to_string();
        create_schema_six_working_directory_fixture(&runtime.path().join("runtime.db"));

        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            startup.path(),
        )
        .unwrap();
        let room = repository.snapshot("legacy-room").unwrap().room;
        assert_eq!(room.working_directory, canonical_startup);
        assert_eq!(room.version, 11);

        let connection = repository.connect().unwrap();
        let schema_version: u32 = connection
            .query_row(
                "SELECT version FROM collaboration_schema WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(schema_version, 7);
        let mut statement = connection
            .prepare(
                "SELECT event_id, execution_working_directory
                 FROM room_events ORDER BY sequence",
            )
            .unwrap();
        let event_directories = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            event_directories,
            vec![
                ("legacy-user".into(), canonical_startup.clone()),
                ("legacy-member-event".into(), canonical_startup.clone()),
            ]
        );
        let owner =
            membership_from_connection(&connection, "legacy-room", LOCAL_PRINCIPAL_ID).unwrap();
        assert!(owner.capabilities.contains(&RoomCapability::ConfigureRoom));
        assert_eq!(owner.capability_version, 2);
        assert_eq!(owner.version, 5);
        let updated_at: String = connection
            .query_row(
                "SELECT updated_at FROM room_principal_memberships
                 WHERE room_id = 'legacy-room' AND principal_id = ?1",
                [LOCAL_PRINCIPAL_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(updated_at, "2026-08-08T00:00:00Z");
    }

    #[test]
    fn migrates_working_directory_atomically_when_owner_update_fails() {
        let runtime = tempfile::tempdir().unwrap();
        let startup = tempfile::tempdir().unwrap();
        let database_path = runtime.path().join("runtime.db");
        create_schema_six_working_directory_fixture(&database_path);
        let connection = Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_owner_capability_migration
                 BEFORE UPDATE OF capabilities_json ON room_principal_memberships
                 BEGIN
                     SELECT RAISE(ABORT, 'forced owner migration failure');
                 END;",
            )
            .unwrap();
        drop(connection);

        assert!(CollaborationRepository::new_with_startup_working_directory(
            runtime.path(),
            CollaborationConfig::default(),
            startup.path(),
        )
        .is_err());

        let connection = Connection::open(&database_path).unwrap();
        let schema_version: u32 = connection
            .query_row(
                "SELECT version FROM collaboration_schema WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(schema_version, 6);
        assert!(
            !table_has_column(&connection, "collaboration_rooms", "working_directory").unwrap()
        );
        assert!(
            !table_has_column(&connection, "room_events", "execution_working_directory").unwrap()
        );
        let owner_versions: (u64, u64) = connection
            .query_row(
                "SELECT capability_version, version FROM room_principal_memberships
                 WHERE room_id = 'legacy-room' AND principal_id = ?1",
                [LOCAL_PRINCIPAL_ID],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(owner_versions, (1, 4));
    }

    #[test]
    fn retry_invalidation_schema_extends_phase_five_without_replacing_it() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let connection = repository.connect().unwrap();
        let version: u32 = connection
            .query_row(
                "SELECT version FROM collaboration_schema WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 7);
        assert!(table_has_column(&connection, "room_events", "invalidated_at").unwrap());
        assert!(table_has_column(&connection, "collaboration_rooms", "working_directory").unwrap());
        assert!(
            table_has_column(&connection, "room_events", "execution_working_directory").unwrap()
        );
        let owner = membership_from_connection(&connection, "room-1", LOCAL_PRINCIPAL_ID).unwrap();
        assert!(owner.capabilities.contains(&RoomCapability::ConfigureRoom));
        assert_eq!(owner.capability_version, 2);
        let member_name_index: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type = 'index' AND name = 'brain_members_active_display_name_idx'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(member_name_index);

        for table in [
            "room_principal_memberships",
            "member_templates",
            "runtime_outbox_events",
            "room_summary_snapshots",
            "member_summary_snapshots",
            "member_cursors",
            "room_event_deliveries",
        ] {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "missing Phase 4 table {table}");
        }
        assert_eq!(snapshot.member_templates.len(), 1);
        assert_eq!(snapshot.members[0].template_id, DEFAULT_MEMBER_TEMPLATE_ID);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn version_two_database_migrates_in_place_without_losing_room_history() {
        let directory = tempfile::tempdir().unwrap();
        let startup = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("runtime.db");
        let connection = Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE collaboration_schema (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     version INTEGER NOT NULL
                 );
                 INSERT INTO collaboration_schema(singleton, version) VALUES (1, 2);
                 CREATE TABLE collaboration_rooms (
                     room_id TEXT PRIMARY KEY, title TEXT NOT NULL,
                     default_member_id TEXT NOT NULL, latest_event_seq INTEGER NOT NULL DEFAULT 0,
                     version INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL
                 );
                 CREATE TABLE brain_members (
                     member_id TEXT PRIMARY KEY,
                     room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id),
                     display_name TEXT NOT NULL, profile_id TEXT NOT NULL,
                     model_policy TEXT NOT NULL, reasoning_depth TEXT NOT NULL,
                     availability TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1,
                     created_at TEXT NOT NULL, last_woken_at TEXT
                 );
                 CREATE TABLE room_events (
                     event_id TEXT PRIMARY KEY,
                     room_id TEXT NOT NULL REFERENCES collaboration_rooms(room_id),
                     sequence INTEGER NOT NULL, sender_kind TEXT NOT NULL,
                     sender_id TEXT NOT NULL, sender_name TEXT NOT NULL,
                     kind TEXT NOT NULL, content TEXT NOT NULL, run_id TEXT,
                     idempotency_key TEXT NOT NULL, created_at TEXT NOT NULL,
                     UNIQUE(room_id, sequence), UNIQUE(room_id, idempotency_key)
                 );
                 CREATE TABLE room_event_recipients (
                     event_id TEXT NOT NULL REFERENCES room_events(event_id) ON DELETE CASCADE,
                     member_id TEXT NOT NULL REFERENCES brain_members(member_id),
                     PRIMARY KEY(event_id, member_id)
                 );
                 CREATE TABLE member_inbox_items (
                     inbox_item_id TEXT PRIMARY KEY,
                     member_id TEXT NOT NULL REFERENCES brain_members(member_id),
                     source_event_id TEXT NOT NULL REFERENCES room_events(event_id),
                     mode TEXT NOT NULL, state TEXT NOT NULL, run_id TEXT,
                     cancel_requested INTEGER NOT NULL DEFAULT 0,
                     reply_event_id TEXT REFERENCES room_events(event_id), error TEXT,
                     version INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
                     started_at TEXT, completed_at TEXT, lease_expires_at TEXT,
                     UNIQUE(member_id, source_event_id)
                 );
                 CREATE TABLE task_runs (
                     task_run_id TEXT PRIMARY KEY,
                     origin_kind TEXT NOT NULL,
                     origin_id TEXT NOT NULL
                 );",
            )
            .unwrap();
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "INSERT INTO collaboration_rooms(
                     room_id, title, default_member_id, latest_event_seq, version, created_at
                 ) VALUES ('legacy-room', '旧房间', 'legacy-member', 1, 7, ?1)",
                [&now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO brain_members(
                     member_id, room_id, display_name, profile_id, model_policy,
                     reasoning_depth, availability, version, created_at, last_woken_at
                 ) VALUES ('legacy-member', 'legacy-room', '旧智脑', 'general_member',
                           'main', 'medium', 'sleeping', 4, ?1, ?1)",
                [&now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO room_events(
                     event_id, room_id, sequence, sender_kind, sender_id, sender_name,
                     kind, content, run_id, idempotency_key, created_at
                 ) VALUES ('legacy-event', 'legacy-room', 1, 'user', 'user', '用户',
                           'user_message', '保留的旧消息', NULL, 'legacy-command', ?1)",
                [&now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO room_event_recipients(event_id, member_id)
                 VALUES ('legacy-event', 'legacy-member')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO member_inbox_items(
                     inbox_item_id, member_id, source_event_id, mode, state,
                     cancel_requested, version, created_at
                 ) VALUES ('legacy-inbox', 'legacy-member', 'legacy-event', 'chat',
                           'pending', 0, 1, ?1)",
                [&now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO task_runs(task_run_id, origin_kind, origin_id)
                 VALUES ('legacy-task', 'member_inbox', 'legacy-inbox')",
                [],
            )
            .unwrap();
        drop(connection);

        let repository = CollaborationRepository::new_with_startup_working_directory(
            directory.path(),
            CollaborationConfig::default(),
            startup.path(),
        )
        .unwrap();
        let snapshot = repository.snapshot("legacy-room").unwrap();
        assert_eq!(snapshot.room.version, 7);
        let canonical_startup = startup.path().canonicalize().unwrap().display().to_string();
        assert_eq!(snapshot.room.working_directory, canonical_startup);
        assert_eq!(snapshot.members[0].display_name, "旧智脑");
        assert_eq!(
            snapshot.members[0].availability,
            MemberAvailability::Sleeping
        );
        assert_eq!(snapshot.members[0].template_id, DEFAULT_MEMBER_TEMPLATE_ID);
        assert_eq!(snapshot.events[0].content, "保留的旧消息");
        assert_eq!(snapshot.events[0].recipients, vec!["legacy-member"]);
        assert_eq!(
            snapshot.inbox[0].task_run_id.as_deref(),
            Some("legacy-task")
        );
        let connection = repository.connect().unwrap();
        let schema_version: u32 = connection
            .query_row(
                "SELECT version FROM collaboration_schema WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(schema_version, 7);
        assert!(table_has_column(&connection, "collaboration_rooms", "working_directory").unwrap());
        assert!(
            table_has_column(&connection, "room_events", "execution_working_directory").unwrap()
        );
        let execution_working_directory: String = connection
            .query_row(
                "SELECT execution_working_directory FROM room_events
                 WHERE event_id = 'legacy-event'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(execution_working_directory, canonical_startup);
        let owner =
            membership_from_connection(&connection, "legacy-room", LOCAL_PRINCIPAL_ID).unwrap();
        assert!(owner.capabilities.contains(&RoomCapability::ConfigureRoom));
        assert_eq!(owner.capability_version, 2);
        let migrated_idempotency_key: String = connection
            .query_row(
                "SELECT idempotency_key FROM member_inbox_items WHERE inbox_item_id = 'legacy-inbox'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated_idempotency_key, "legacy-command:legacy-member");
    }

    #[test]
    fn room_capabilities_are_checked_against_current_membership() {
        let (_directory, repository) = repository();
        ensure(&repository);
        let owner = CollaborationActor::local();
        repository
            .upsert_membership(
                &owner,
                "room-1",
                "viewer-1",
                RoomRole::Viewer,
                &[RoomCapability::RoomRead],
                None,
            )
            .unwrap();
        let viewer = CollaborationActor::new("viewer-1");

        repository.snapshot_as(&viewer, "room-1").unwrap();
        let error = repository
            .create_member_as(
                &viewer,
                "room-1",
                "不应创建",
                DEFAULT_MEMBER_TEMPLATE_ID,
                None,
                None,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CollaborationError::CapabilityDenied {
                capability: RoomCapability::CreateMember,
                ..
            }
        ));
    }

    #[test]
    fn checked_commands_reject_stale_room_and_member_versions_atomically() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let actor = CollaborationActor::local();
        let member = snapshot.members[0].clone();
        let targets = vec![MemberAddress {
            member_id: member.member_id.clone(),
            expected_version: member.version,
        }];
        repository
            .post_message_checked(
                &actor,
                "room-1",
                &targets,
                "第一次",
                RoomInputMode::Chat,
                DEFAULT_THREAD_KEY,
                snapshot.room.version,
                "checked-1",
            )
            .unwrap();

        let error = repository
            .post_message_checked(
                &actor,
                "room-1",
                &targets,
                "过期命令",
                RoomInputMode::Chat,
                DEFAULT_THREAD_KEY,
                snapshot.room.version,
                "checked-2",
            )
            .unwrap_err();
        assert!(matches!(error, CollaborationError::VersionConflict { .. }));
        assert_eq!(repository.snapshot("room-1").unwrap().events.len(), 1);

        let current = repository.member("room-1", &member.member_id).unwrap();
        repository
            .sleep_member_checked(&actor, "room-1", &member.member_id, current.version)
            .unwrap();
        let error = repository
            .wake_member_checked(&actor, "room-1", &member.member_id, current.version)
            .unwrap_err();
        assert!(matches!(error, CollaborationError::VersionConflict { .. }));
    }

    #[test]
    fn room_mutations_commit_replayable_outbox_events_with_cas_ack() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        for event in repository.pending_outbox_events(100).unwrap() {
            repository
                .mark_outbox_published(&event.outbox_event_id, event.version)
                .unwrap();
        }
        assert!(repository.pending_outbox_events(100).unwrap().is_empty());

        repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "需要可靠通知",
                RoomInputMode::Task,
                "outbox-command",
            )
            .unwrap();
        let pending = repository.pending_outbox_events(100).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].room_id, "room-1");
        assert_eq!(pending[0].event_kind, "room_changed");
        repository
            .mark_outbox_published(&pending[0].outbox_event_id, pending[0].version)
            .unwrap();
        let stale = repository
            .mark_outbox_published(&pending[0].outbox_event_id, pending[0].version)
            .unwrap_err();
        assert!(matches!(stale, CollaborationError::VersionConflict { .. }));
    }

    #[test]
    fn sequence_replay_and_member_cursors_are_purpose_specific() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        let first = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "第一条",
                RoomInputMode::Chat,
                "replay-1",
            )
            .unwrap();
        let second = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&member_id),
                "第二条",
                RoomInputMode::Chat,
                "replay-2",
            )
            .unwrap();
        let replay = repository
            .events_after("room-1", first.event.sequence, 100)
            .unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].event_id, second.event.event_id);

        let summary = repository
            .advance_member_cursor(&member_id, MemberCursorKind::Summary, 0, 1)
            .unwrap();
        let notification = repository
            .advance_member_cursor(&member_id, MemberCursorKind::Notification, 0, 2)
            .unwrap();
        assert_eq!(summary.event_sequence, 1);
        assert_eq!(notification.event_sequence, 2);
        let cursors = repository.member_cursors(&member_id).unwrap();
        assert_eq!(cursors.len(), 2);
    }

    #[test]
    fn same_member_same_thread_serializes_but_independent_threads_can_lease() {
        let (_directory, repository) = repository();
        let snapshot = ensure(&repository);
        let member_id = snapshot.room.default_member_id;
        repository
            .post_message_in_thread(
                "room-1",
                std::slice::from_ref(&member_id),
                "主线一",
                RoomInputMode::Chat,
                "thread-main",
                "thread-1",
            )
            .unwrap();
        repository
            .post_message_in_thread(
                "room-1",
                std::slice::from_ref(&member_id),
                "主线二",
                RoomInputMode::Chat,
                "thread-main",
                "thread-2",
            )
            .unwrap();
        repository
            .post_message_in_thread(
                "room-1",
                std::slice::from_ref(&member_id),
                "独立支线",
                RoomInputMode::Chat,
                "thread-independent",
                "thread-3",
            )
            .unwrap();

        let first = repository.lease_next().unwrap().unwrap();
        let second = repository.lease_next().unwrap().unwrap();
        assert_eq!(first.member_id, second.member_id);
        assert_ne!(first.thread_key, second.thread_key);
        assert!(repository.lease_next().unwrap().is_none());
    }
}
