//! Generic, single-instance agent execution contracts.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use runtime::{
    ApiClient, ContentBlock, ConversationRuntime, PermissionPolicy, Session, TokenUsage, ToolError,
    ToolExecutor,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use brain_llm::config::ResolvedModelPolicy;
pub use knowledge_core::{ContextBlock, ContextSnapshot};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum AgentRuntimeError {
    #[error("invalid agent runtime specification: {0}")]
    InvalidSpec(String),
    #[error("agent run cancelled")]
    Cancelled,
    #[error("agent worker pool is closed")]
    WorkerPoolClosed,
    #[error("agent runtime failed: {0}")]
    Runtime(String),
    #[error("artifact sink failed: {0}")]
    ArtifactSink(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolGrant {
    tools: BTreeSet<String>,
}

impl ToolGrant {
    #[must_use]
    pub fn new<I, S>(tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            tools: tools.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, tool_name: &str) -> bool {
        self.tools.contains(tool_name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(String::as_str)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputContract {
    Text,
    Json { schema_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfileSnapshot {
    pub profile_id: String,
    pub version: u64,
    pub role: String,
    pub system_prompt: Vec<String>,
    pub tool_grant: ToolGrant,
    pub output_contract: OutputContract,
    pub max_iterations: usize,
}

impl AgentProfileSnapshot {
    pub fn new(
        profile_id: impl Into<String>,
        version: u64,
        role: impl Into<String>,
        system_prompt: Vec<String>,
        tool_grant: ToolGrant,
        output_contract: OutputContract,
        max_iterations: usize,
    ) -> Result<Self, AgentRuntimeError> {
        let snapshot = Self {
            profile_id: profile_id.into(),
            version,
            role: role.into(),
            system_prompt,
            tool_grant,
            output_contract,
            max_iterations,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn validate(&self) -> Result<(), AgentRuntimeError> {
        require_non_empty("profile_id", &self.profile_id)?;
        require_non_empty("profile role", &self.role)?;
        if self.system_prompt.is_empty()
            || self
                .system_prompt
                .iter()
                .all(|prompt| prompt.trim().is_empty())
        {
            return Err(AgentRuntimeError::InvalidSpec(
                "profile system_prompt must not be empty".into(),
            ));
        }
        if self.max_iterations == 0 {
            return Err(AgentRuntimeError::InvalidSpec(
                "profile max_iterations must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningDepth {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReasoningPolicy {
    pub depth: ReasoningDepth,
    pub native_effort: Option<String>,
}

impl ReasoningPolicy {
    #[must_use]
    pub fn medium() -> Self {
        Self {
            depth: ReasoningDepth::Medium,
            native_effort: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetReservation {
    pub reservation_id: String,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRunSpec {
    pub agent_instance_id: String,
    pub instance_run_id: String,
    pub member_id: Option<String>,
    pub inbox_item_id: Option<String>,
    pub task_run_id: String,
    pub node_id: String,
    pub profile: AgentProfileSnapshot,
    pub context_snapshot: ContextSnapshot,
    pub input_artifacts: Vec<String>,
    pub model: ResolvedModelPolicy,
    pub reasoning: ReasoningPolicy,
    pub budget_reservation: BudgetReservation,
    pub deadline_unix_ms: Option<u64>,
}

impl AgentRunSpec {
    fn validate(&self) -> Result<(), AgentRuntimeError> {
        require_non_empty("agent_instance_id", &self.agent_instance_id)?;
        require_non_empty("instance_run_id", &self.instance_run_id)?;
        require_non_empty("task_run_id", &self.task_run_id)?;
        require_non_empty("node_id", &self.node_id)?;
        require_non_empty("model policy_id", &self.model.policy_id)?;
        require_non_empty("model provider", &self.model.provider)?;
        require_non_empty("model", &self.model.model)?;
        require_non_empty(
            "budget reservation_id",
            &self.budget_reservation.reservation_id,
        )?;
        self.profile.validate()?;
        self.context_snapshot
            .validate()
            .map_err(|error| AgentRuntimeError::InvalidSpec(error.to_string()))?;
        if self.model.max_output_tokens == 0
            || self.budget_reservation.max_input_tokens == 0
            || self.budget_reservation.max_output_tokens == 0
        {
            return Err(AgentRuntimeError::InvalidSpec(
                "model and budget token limits must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactEnvelope {
    pub artifact_id: String,
    pub content: String,
    pub content_hash: String,
    pub output_contract: OutputContract,
    pub context_snapshot_id: String,
    pub producer_instance_id: String,
    pub producer_instance_run_id: String,
    pub producer_member_id: Option<String>,
    pub task_run_id: String,
    pub node_id: String,
    pub profile_id: String,
    pub profile_version: u64,
    pub model: ResolvedModelPolicy,
    pub reasoning: ReasoningPolicy,
    pub usage: TokenUsage,
    pub iterations: usize,
    pub created_at_unix_ms: u64,
}

pub trait ArtifactSink: Send + Sync + 'static {
    fn store(&self, artifact: &ArtifactEnvelope) -> Result<(), AgentRuntimeError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRunStatus {
    Completed,
    Failed,
    Cancelled,
    DeadlineExceeded,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRunOutcome {
    pub instance_run_id: String,
    pub status: AgentRunStatus,
    pub artifact: Option<ArtifactEnvelope>,
    pub error: Option<String>,
    pub usage: TokenUsage,
    pub iterations: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct AgentWorkerPool {
    semaphore: Arc<Semaphore>,
}

impl AgentWorkerPool {
    pub fn new(max_workers: usize) -> Result<Self, AgentRuntimeError> {
        if max_workers == 0 {
            return Err(AgentRuntimeError::InvalidSpec(
                "max_workers must be greater than zero".into(),
            ));
        }
        Ok(Self {
            semaphore: Arc::new(Semaphore::new(max_workers)),
        })
    }

    pub async fn acquire(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<AgentWorkerLease, AgentRuntimeError> {
        if cancellation.is_cancelled() {
            return Err(AgentRuntimeError::Cancelled);
        }
        let permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(AgentRuntimeError::Cancelled),
            permit = Arc::clone(&self.semaphore).acquire_owned() => {
                permit.map_err(|_| AgentRuntimeError::WorkerPoolClosed)?
            }
        };
        Ok(AgentWorkerLease { _permit: permit })
    }

    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

pub struct AgentWorkerLease {
    _permit: OwnedSemaphorePermit,
}

pub struct AgentRuntime<C, T, S> {
    api_client: C,
    tool_executor: T,
    permission_policy: PermissionPolicy,
    artifact_sink: S,
}

impl<C, T, S> AgentRuntime<C, T, S>
where
    C: ApiClient + Send + 'static,
    T: ToolExecutor + Send + 'static,
    S: ArtifactSink,
{
    #[must_use]
    pub fn new(
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        artifact_sink: S,
    ) -> Self {
        Self {
            api_client,
            tool_executor,
            permission_policy,
            artifact_sink,
        }
    }

    pub fn spawn(
        self,
        spec: AgentRunSpec,
        lease: AgentWorkerLease,
        cancellation: CancellationToken,
    ) -> AgentRunHandle {
        let instance_run_id = spec.instance_run_id.clone();
        let deadline_expired = Arc::new(AtomicBool::new(false));
        schedule_deadline(
            spec.deadline_unix_ms,
            cancellation.clone(),
            Arc::clone(&deadline_expired),
        );
        let worker_cancellation = cancellation.clone();
        let join = tokio::task::spawn_blocking(move || {
            let _lease = lease;
            self.run(spec, &worker_cancellation, deadline_expired.as_ref())
        });
        AgentRunHandle {
            instance_run_id,
            cancellation,
            join,
        }
    }

    fn run(
        self,
        spec: AgentRunSpec,
        cancellation: &CancellationToken,
        deadline_expired: &AtomicBool,
    ) -> AgentRunOutcome {
        let started = Instant::now();
        if let Err(error) = spec.validate() {
            return failed_outcome(
                spec.instance_run_id,
                AgentRunStatus::Failed,
                error.to_string(),
                TokenUsage::default(),
                0,
                started.elapsed(),
            );
        }
        if cancellation.is_cancelled() {
            return cancelled_outcome(
                spec.instance_run_id,
                deadline_expired.load(Ordering::SeqCst),
                TokenUsage::default(),
                0,
                started.elapsed(),
            );
        }

        let Self {
            api_client,
            tool_executor,
            permission_policy,
            artifact_sink,
        } = self;
        let profile = spec.profile.clone();
        let context = spec.context_snapshot.render();
        let mut conversation = ConversationRuntime::new(
            Session::new(),
            api_client,
            GrantedToolExecutor::new(tool_executor, profile.tool_grant.clone()),
            permission_policy,
            profile.system_prompt.clone(),
        )
        .with_max_iterations(profile.max_iterations)
        .with_cancellation_token(cancellation.clone());

        let summary = match conversation.run_turn(context, None) {
            Ok(summary) => summary,
            Err(error) => {
                let usage = conversation.usage().cumulative_usage();
                if error.is_cancelled() || cancellation.is_cancelled() {
                    return cancelled_outcome(
                        spec.instance_run_id,
                        deadline_expired.load(Ordering::SeqCst),
                        usage,
                        0,
                        started.elapsed(),
                    );
                }
                return failed_outcome(
                    spec.instance_run_id,
                    AgentRunStatus::Failed,
                    error.to_string(),
                    usage,
                    0,
                    started.elapsed(),
                );
            }
        };

        finish_successful_run(
            &artifact_sink,
            spec,
            profile,
            &summary,
            cancellation,
            deadline_expired,
            started,
        )
    }
}

fn finish_successful_run<S: ArtifactSink>(
    artifact_sink: &S,
    spec: AgentRunSpec,
    profile: AgentProfileSnapshot,
    summary: &runtime::TurnSummary,
    cancellation: &CancellationToken,
    deadline_expired: &AtomicBool,
    started: Instant,
) -> AgentRunOutcome {
    if cancellation.is_cancelled() {
        return cancelled_outcome(
            spec.instance_run_id,
            deadline_expired.load(Ordering::SeqCst),
            summary.usage,
            summary.iterations,
            started.elapsed(),
        );
    }
    if summary.usage.input_tokens > spec.budget_reservation.max_input_tokens
        || summary.usage.output_tokens > spec.budget_reservation.max_output_tokens
    {
        return failed_outcome(
            spec.instance_run_id,
            AgentRunStatus::Failed,
            "agent run exceeded its token budget reservation".into(),
            summary.usage,
            summary.iterations,
            started.elapsed(),
        );
    }

    let output = final_assistant_text(summary);
    if let Err(error) = validate_output(&profile.output_contract, &output) {
        return failed_outcome(
            spec.instance_run_id,
            AgentRunStatus::Failed,
            error.to_string(),
            summary.usage,
            summary.iterations,
            started.elapsed(),
        );
    }
    let artifact = build_artifact(&spec, profile, summary, output);
    if cancellation.is_cancelled() {
        return cancelled_outcome(
            spec.instance_run_id,
            deadline_expired.load(Ordering::SeqCst),
            summary.usage,
            summary.iterations,
            started.elapsed(),
        );
    }
    if let Err(error) = artifact_sink.store(&artifact) {
        return failed_outcome(
            spec.instance_run_id,
            AgentRunStatus::Failed,
            error.to_string(),
            summary.usage,
            summary.iterations,
            started.elapsed(),
        );
    }

    AgentRunOutcome {
        instance_run_id: spec.instance_run_id,
        status: AgentRunStatus::Completed,
        artifact: Some(artifact),
        error: None,
        usage: summary.usage,
        iterations: summary.iterations,
        duration_ms: elapsed_ms(started.elapsed()),
    }
}

fn build_artifact(
    spec: &AgentRunSpec,
    profile: AgentProfileSnapshot,
    summary: &runtime::TurnSummary,
    output: String,
) -> ArtifactEnvelope {
    let content_hash = sha256_hex(output.as_bytes());
    ArtifactEnvelope {
        artifact_id: format!("artifact-{}-{}", spec.instance_run_id, &content_hash[..12]),
        content: output,
        content_hash,
        output_contract: profile.output_contract,
        context_snapshot_id: spec.context_snapshot.context_snapshot_id.clone(),
        producer_instance_id: spec.agent_instance_id.clone(),
        producer_instance_run_id: spec.instance_run_id.clone(),
        producer_member_id: spec.member_id.clone(),
        task_run_id: spec.task_run_id.clone(),
        node_id: spec.node_id.clone(),
        profile_id: profile.profile_id,
        profile_version: profile.version,
        model: spec.model.clone(),
        reasoning: spec.reasoning.clone(),
        usage: summary.usage,
        iterations: summary.iterations,
        created_at_unix_ms: unix_time_ms(),
    }
}

pub struct AgentRunHandle {
    instance_run_id: String,
    cancellation: CancellationToken,
    join: JoinHandle<AgentRunOutcome>,
}

impl AgentRunHandle {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub async fn wait(self) -> AgentRunOutcome {
        match self.join.await {
            Ok(outcome) => outcome,
            Err(error) => failed_outcome(
                self.instance_run_id,
                AgentRunStatus::Failed,
                format!("agent worker task failed: {error}"),
                TokenUsage::default(),
                0,
                Duration::ZERO,
            ),
        }
    }
}

struct GrantedToolExecutor<T> {
    inner: T,
    grant: ToolGrant,
}

impl<T> GrantedToolExecutor<T> {
    fn new(inner: T, grant: ToolGrant) -> Self {
        Self { inner, grant }
    }
}

impl<T> ToolExecutor for GrantedToolExecutor<T>
where
    T: ToolExecutor,
{
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if !self.grant.contains(tool_name) {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not granted by the agent profile"
            )));
        }
        self.inner.execute(tool_name, input)
    }
}

fn validate_output(
    output_contract: &OutputContract,
    output: &str,
) -> Result<(), AgentRuntimeError> {
    require_non_empty("agent output", output)?;
    if let OutputContract::Json { schema_id } = output_contract {
        require_non_empty("output schema_id", schema_id)?;
        serde_json::from_str::<serde_json::Value>(output).map_err(|error| {
            AgentRuntimeError::Runtime(format!(
                "agent output does not satisfy JSON contract `{schema_id}`: {error}"
            ))
        })?;
    }
    Ok(())
}

fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    summary
        .assistant_messages
        .iter()
        .rev()
        .find_map(|message| {
            let text = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            (!text.trim().is_empty()).then(|| text.trim().to_string())
        })
        .unwrap_or_default()
}

fn cancelled_outcome(
    instance_run_id: String,
    deadline_expired: bool,
    usage: TokenUsage,
    iterations: usize,
    elapsed: Duration,
) -> AgentRunOutcome {
    let (status, error) = if deadline_expired {
        (
            AgentRunStatus::DeadlineExceeded,
            "agent run deadline exceeded",
        )
    } else {
        (AgentRunStatus::Cancelled, "agent run cancelled")
    };
    failed_outcome(
        instance_run_id,
        status,
        error.into(),
        usage,
        iterations,
        elapsed,
    )
}

fn failed_outcome(
    instance_run_id: String,
    status: AgentRunStatus,
    error: String,
    usage: TokenUsage,
    iterations: usize,
    elapsed: Duration,
) -> AgentRunOutcome {
    AgentRunOutcome {
        instance_run_id,
        status,
        artifact: None,
        error: Some(error),
        usage,
        iterations,
        duration_ms: elapsed_ms(elapsed),
    }
}

fn schedule_deadline(
    deadline_unix_ms: Option<u64>,
    cancellation: CancellationToken,
    deadline_expired: Arc<AtomicBool>,
) {
    let Some(deadline_unix_ms) = deadline_unix_ms else {
        return;
    };
    tokio::spawn(async move {
        let remaining = deadline_unix_ms.saturating_sub(unix_time_ms());
        tokio::time::sleep(Duration::from_millis(remaining)).await;
        deadline_expired.store(true, Ordering::SeqCst);
        cancellation.cancel();
    });
}

fn require_non_empty(field: &str, value: &str) -> Result<(), AgentRuntimeError> {
    if value.trim().is_empty() {
        return Err(AgentRuntimeError::InvalidSpec(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn elapsed_ms(duration: Duration) -> u64 {
    duration.as_millis() as u64
}
