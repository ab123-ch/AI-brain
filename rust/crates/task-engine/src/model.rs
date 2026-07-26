use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Result<T> = std::result::Result<T, TaskEngineError>;

#[derive(Debug, thiserror::Error)]
pub enum TaskEngineError {
    #[error("task engine I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("task engine database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid task engine input: {0}")]
    Invalid(String),
    #[error("{entity} `{id}` was not found")]
    NotFound { entity: &'static str, id: String },
    #[error(
        "compare-and-swap conflict for {entity} `{id}`: expected version {expected}, actual version {actual}"
    )]
    CasConflict {
        entity: &'static str,
        id: String,
        expected: u64,
        actual: u64,
    },
    #[error("invalid {entity} transition for `{id}` from `{from}` to `{to}`")]
    InvalidTransition {
        entity: &'static str,
        id: String,
        from: String,
        to: String,
    },
    #[error(
        "budget `{account_id}` exceeded for {resource}: requested {requested}, available {available}"
    )]
    BudgetExceeded {
        account_id: String,
        resource: &'static str,
        requested: u64,
        available: u64,
    },
    #[error(
        "usage exceeds reservation `{reservation_id}`: reserved {reserved_input}/{reserved_output}, actual {actual_input}/{actual_output}"
    )]
    UsageExceedsReservation {
        reservation_id: String,
        reserved_input: u64,
        reserved_output: u64,
        actual_input: u64,
        actual_output: u64,
    },
    #[error("scheduler admission was cancelled")]
    Cancelled,
    #[error("scheduler is closed")]
    SchedulerClosed,
    #[error("task coordinator worker failed: {0}")]
    CoordinatorWorker(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunState {
    Queued,
    Running,
    PausedBudget,
    NeedsInput,
    Completed,
    Failed,
    Cancelled,
}

impl TaskRunState {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::PausedBudget => "paused_budget",
            Self::NeedsInput => "needs_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "paused_budget" => Ok(Self::PausedBudget),
            "needs_input" => Ok(Self::NeedsInput),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(TaskEngineError::Invalid(format!(
                "unknown task state `{other}`"
            ))),
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    WaitingDependency,
    Ready,
    Running,
    NeedsInput,
    Completed,
    Failed,
    Cancelled,
}

impl NodeState {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::WaitingDependency => "waiting_dependency",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::NeedsInput => "needs_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "waiting_dependency" => Ok(Self::WaitingDependency),
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "needs_input" => Ok(Self::NeedsInput),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(TaskEngineError::Invalid(format!(
                "unknown node state `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Deterministic,
    Model,
    Reviewer,
    Synthesizer,
}

impl NodeKind {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Model => "model",
            Self::Reviewer => "reviewer",
            Self::Synthesizer => "synthesizer",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "deterministic" => Ok(Self::Deterministic),
            "model" => Ok(Self::Model),
            "reviewer" => Ok(Self::Reviewer),
            "synthesizer" => Ok(Self::Synthesizer),
            other => Err(TaskEngineError::Invalid(format!(
                "unknown node kind `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceRunState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Quarantined,
}

impl InstanceRunState {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Quarantined => "quarantined",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(TaskEngineError::Invalid(format!(
                "unknown instance state `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BudgetReservationState {
    Active,
    Settled,
    Released,
}

impl BudgetReservationState {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Settled => "settled",
            Self::Released => "released",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "settled" => Ok(Self::Settled),
            "released" => Ok(Self::Released),
            other => Err(TaskEngineError::Invalid(format!(
                "unknown budget reservation state `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    TaskCreated,
    TaskRunning,
    TaskPausedBudget,
    NodeStarted,
    NodeCompleted,
    NodeFailed,
    NodeCancelled,
    NodeReady,
    NodeInterrupted,
    TaskCompleted,
    TaskFailed,
    TaskNeedsInput,
    TaskCancelled,
}

impl TaskEventKind {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::TaskCreated => "task_created",
            Self::TaskRunning => "task_running",
            Self::TaskPausedBudget => "task_paused_budget",
            Self::NodeStarted => "node_started",
            Self::NodeCompleted => "node_completed",
            Self::NodeFailed => "node_failed",
            Self::NodeCancelled => "node_cancelled",
            Self::NodeReady => "node_ready",
            Self::NodeInterrupted => "node_interrupted",
            Self::TaskCompleted => "task_completed",
            Self::TaskFailed => "task_failed",
            Self::TaskNeedsInput => "task_needs_input",
            Self::TaskCancelled => "task_cancelled",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "task_created" => Ok(Self::TaskCreated),
            "task_running" => Ok(Self::TaskRunning),
            "task_paused_budget" => Ok(Self::TaskPausedBudget),
            "node_started" => Ok(Self::NodeStarted),
            "node_completed" => Ok(Self::NodeCompleted),
            "node_failed" => Ok(Self::NodeFailed),
            "node_cancelled" => Ok(Self::NodeCancelled),
            "node_ready" => Ok(Self::NodeReady),
            "node_interrupted" => Ok(Self::NodeInterrupted),
            "task_completed" => Ok(Self::TaskCompleted),
            "task_failed" => Ok(Self::TaskFailed),
            "task_needs_input" => Ok(Self::TaskNeedsInput),
            "task_cancelled" => Ok(Self::TaskCancelled),
            other => Err(TaskEngineError::Invalid(format!(
                "unknown task event kind `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetLimits {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetRequest {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActualUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewBudgetAccount {
    pub budget_account_id: String,
    pub parent_budget_account_id: Option<String>,
    pub owner_kind: String,
    pub owner_id: String,
    pub limits: BudgetLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetAccount {
    pub budget_account_id: String,
    pub parent_budget_account_id: Option<String>,
    pub owner_kind: String,
    pub owner_id: String,
    pub limits: BudgetLimits,
    pub reserved_input_tokens: u64,
    pub reserved_output_tokens: u64,
    pub consumed_input_tokens: u64,
    pub consumed_output_tokens: u64,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetReservation {
    pub reservation_id: String,
    pub budget_account_id: String,
    pub idempotency_key: String,
    pub reserved: BudgetRequest,
    pub actual: Option<ActualUsage>,
    pub state: BudgetReservationState,
    pub version: u64,
    pub created_at: String,
    pub settled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewTaskNode {
    pub node_id: String,
    pub kind: NodeKind,
    pub dependencies: Vec<String>,
    pub provider: String,
    pub model: String,
    pub profile: String,
    pub room_id: Option<String>,
    pub member_id: Option<String>,
    pub reservation: BudgetRequest,
    pub retryable: bool,
    pub side_effecting: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewTaskRun {
    pub task_run_id: String,
    pub workflow: String,
    pub objective: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub room_id: Option<String>,
    pub config_version: String,
    pub resolved_config: Value,
    pub parent_budget_account_id: Option<String>,
    pub budget: BudgetLimits,
    pub nodes: Vec<NewTaskNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRun {
    pub task_run_id: String,
    pub workflow: String,
    pub objective: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub room_id: Option<String>,
    pub state: TaskRunState,
    pub version: u64,
    pub config_snapshot_id: String,
    pub config_version: String,
    pub config_content_hash: String,
    pub resolved_config: Value,
    pub budget_account_id: String,
    pub latest_event_seq: u64,
    pub cancel_requested: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskNode {
    pub node_id: String,
    pub task_run_id: String,
    pub kind: NodeKind,
    pub dependencies: Vec<String>,
    pub state: NodeState,
    pub provider: String,
    pub model: String,
    pub profile: String,
    pub room_id: Option<String>,
    pub member_id: Option<String>,
    pub reservation: BudgetRequest,
    pub retryable: bool,
    pub side_effecting: bool,
    pub current_instance_run_id: Option<String>,
    pub output_artifact_id: Option<String>,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceRun {
    pub instance_run_id: String,
    pub task_run_id: String,
    pub node_id: String,
    pub reservation_id: String,
    pub state: InstanceRunState,
    pub artifact_id: Option<String>,
    pub error: Option<String>,
    pub usage: Option<ActualUsage>,
    pub version: u64,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartedNode {
    pub task: TaskRun,
    pub node: TaskNode,
    pub instance: InstanceRun,
    pub reservation: BudgetReservation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeCompletion {
    pub task: TaskRun,
    pub node: TaskNode,
    pub instance: InstanceRun,
    pub reservation: BudgetReservation,
    pub newly_ready_node_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEvent {
    pub event_id: String,
    pub task_run_id: String,
    pub sequence: u64,
    pub kind: TaskEventKind,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskArtifact {
    pub artifact_id: String,
    pub task_run_id: String,
    pub node_id: String,
    pub instance_run_id: String,
    pub content: String,
    pub content_hash: String,
    pub media_type: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurableTaskResult {
    pub task_run_id: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub instance_run_id: String,
    pub artifact: TaskArtifact,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryReport {
    pub interrupted: usize,
    pub requeued: usize,
    pub needs_input: usize,
    pub cancelled: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdmissionRequest {
    pub request_id: String,
    pub task_run_id: String,
    pub room_id: Option<String>,
    pub member_id: Option<String>,
    pub provider: String,
    pub profile: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerLimits {
    pub max_workers: usize,
    pub max_global: usize,
    pub max_per_room: usize,
    pub max_per_member: usize,
    pub max_per_provider: usize,
    pub max_per_profile: usize,
    pub max_per_task: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub active_workers: usize,
    pub active_global: usize,
    pub waiting: usize,
}
