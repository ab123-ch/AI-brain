use std::collections::BTreeSet;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;

/// Unique identifier for an agent.
pub type AgentId = String;

/// The kind of sub-agent that can be spawned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentType {
    Explore,
    GeneralPurpose,
    Plan,
    Verification,
}

/// Status of an agent execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Running,
    Completed,
    Failed,
}

/// Result returned by an agent after execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub agent_id: AgentId,
    pub status: AgentStatus,
    pub output: String,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Events flowing through the dispatch bus.
#[derive(Debug)]
pub enum DispatchEvent {
    /// A synchronous agent request that expects a result via oneshot channel.
    SyncAgentRequest {
        agent_id: AgentId,
        subagent_type: SubagentType,
        prompt: String,
        reply_tx: oneshot::Sender<AgentResult>,
    },
    /// An asynchronous agent has completed.
    AsyncAgentCompleted(AgentResult),
    /// A brain-level task has completed.
    BrainTaskCompleted {
        brain_id: String,
        result: AgentResult,
    },
    /// User input that needs to be routed.
    UserInput { content: String },
}

/// Priority level for dispatch events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    Urgent = 0,
    Normal = 1,
    Background = 2,
}

/// An event wrapped with priority and enqueue timestamp for ordered processing.
#[derive(Debug)]
pub struct PrioritizedEvent {
    pub event: DispatchEvent,
    pub priority: Priority,
    pub enqueued_at: Instant,
}

impl PrioritizedEvent {
    /// Create a new prioritized event with the current timestamp.
    pub fn new(event: DispatchEvent, priority: Priority) -> Self {
        Self {
            event,
            priority,
            enqueued_at: Instant::now(),
        }
    }
}

/// Classification of agent type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentType {
    Main,
    SubAgent,
    Brain,
}

/// Handle representing a registered agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHandle {
    pub id: AgentId,
    pub brain_id: String,
    pub agent_type: AgentType,
    pub allowed_tools: BTreeSet<String>,
}

/// Messages sent to the main loop.
#[derive(Debug)]
pub enum MainLoopMessage {
    AgentNotification(AgentResult),
    BrainTaskNotification {
        brain_id: String,
        result: AgentResult,
    },
}

/// Errors that can occur during dispatch operations.
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("agent not found: {0}")]
    AgentNotFound(AgentId),
    #[error("channel closed")]
    ChannelClosed,
    #[error("operation timed out")]
    Timeout,
    #[error("agent failed: {0}")]
    AgentFailed(String),
    #[error("dispatch queue is full")]
    QueueFull,
    #[error("{0}")]
    Other(String),
}
