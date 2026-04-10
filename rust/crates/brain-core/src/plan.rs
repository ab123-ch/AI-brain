//! Task plan types for the continuous execution architecture.
//!
//! Defines the data structures for task decomposition, plan lifecycle,
//! and step execution tracking.

use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Plan Lifecycle
// ---------------------------------------------------------------------------

/// Overall status of a task plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    /// Initial decomposition by sensory brain, not yet reviewed.
    Draft,
    /// Undergoing clarification loop (Phase 2).
    Clarifying,
    /// All ambiguities resolved, ready for execution (Phase 3).
    Confirmed,
}

impl fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Draft => write!(f, "draft"),
            Self::Clarifying => write!(f, "clarifying"),
            Self::Confirmed => write!(f, "confirmed"),
        }
    }
}

/// Status of a single plan step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Not yet started.
    Pending,
    /// Currently being executed by the reasoning brain.
    InProgress,
    /// Successfully completed.
    Done,
    /// Execution failed after retry.
    Failed,
    /// Skipped by user or master brain decision.
    Skipped,
}

impl fmt::Display for StepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

// ---------------------------------------------------------------------------
// Task Plan
// ---------------------------------------------------------------------------

/// A decomposed task plan produced by the sensory brain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    /// Unique plan identifier.
    pub id: String,
    /// One-sentence task description.
    pub description: String,
    /// Current lifecycle status.
    pub status: PlanStatus,
    /// Ordered list of steps to execute.
    pub steps: Vec<PlanStep>,
    /// Tools/skills/MCPs recommended by the sensory brain (top 5).
    pub recommended_tools: Vec<ToolRef>,
    /// Ambiguities already known at decomposition time.
    pub known_ambiguities: Vec<String>,
}

/// A single step within a task plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// Step number (1-based).
    pub id: u32,
    /// What this step should accomplish.
    pub goal: String,
    /// Detailed instructions (filled during clarification).
    pub detail: Option<String>,
    /// How to verify this step is complete.
    pub verification: Option<String>,
    /// Tools estimated to be useful for this step.
    pub estimated_tools: Vec<String>,
    /// Current execution status.
    pub status: StepStatus,
    /// Execution result (set after the step runs).
    pub result: Option<StepResult>,
}

// ---------------------------------------------------------------------------
// Tool Reference
// ---------------------------------------------------------------------------

/// Category of external tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolType {
    /// A SKILL.md-based skill.
    Skill,
    /// An MCP server tool.
    MCP,
    /// A plugin tool.
    Plugin,
}

impl fmt::Display for ToolType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skill => write!(f, "skill"),
            Self::MCP => write!(f, "mcp"),
            Self::Plugin => write!(f, "plugin"),
        }
    }
}

/// A reference to an external tool recommended for the task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRef {
    /// What kind of tool this is.
    pub tool_type: ToolType,
    /// Tool name (e.g. "tdd", "filesystem", "rust-analyzer").
    pub name: String,
    /// Relevance score [0.0, 1.0] for the current task.
    pub relevance: f64,
    /// Why this tool is recommended.
    pub description: String,
}

// ---------------------------------------------------------------------------
// Step Result
// ---------------------------------------------------------------------------

/// Outcome of executing a single plan step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Textual summary of what was done.
    pub output: String,
    /// All tool calls made during this step.
    pub tool_history: Vec<ToolCallRecord>,
    /// Files created or modified during this step.
    pub files_modified: Vec<String>,
    /// Whether the reasoning brain considers the step complete.
    pub success: bool,
    /// If not successful, why.
    pub failure_reason: Option<String>,
}

/// Record of a single tool call within a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Name of the tool called.
    pub tool_name: String,
    /// Input passed to the tool (JSON).
    pub input: serde_json::Value,
    /// Output returned by the tool.
    pub output: String,
    /// Whether the call resulted in an error.
    pub is_error: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}
