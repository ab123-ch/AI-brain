//! Memory injection types for the proactive recall system.
//!
//! Defines how the memory brain injects context into the reasoning brain,
//! including weight-based prioritization and multi-tier content delivery.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Injection Context (assembled by memory brain for each LLM call)
// ---------------------------------------------------------------------------

/// The full context package injected by the memory brain before each
/// reasoning-brain LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectedContext {
    /// Confirmed design decisions (always fully injected).
    pub confirmed_facts: Vec<ConfirmedFact>,
    /// Weight-ranked memories (full / summary / mention).
    pub memories: Vec<MemoryInjection>,
    /// Recent conversation episodes (sliding window).
    pub recent_episodes: Vec<RoundRecord>,
    /// Compressed summaries of older rounds.
    pub older_summaries: Vec<String>,
}

/// A single memory injection, with weight determining delivery tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryInjection {
    /// Composite weight score [0.0, 1.0].
    pub weight: f64,
    /// The content to inject (tier depends on weight).
    pub content: InjectionContent,
}

/// How a memory is delivered to the reasoning brain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tier", rename_all = "snake_case")]
pub enum InjectionContent {
    /// Full original content (weight >= 0.8).
    Full { content: String, ref_id: String },
    /// Compressed summary (weight 0.5..0.8).
    Summary { summary: String, ref_id: String },
    /// One-line mention (weight 0.3..0.5).
    Mention { hint: String, ref_id: String },
}

// ---------------------------------------------------------------------------
// Confirmed Facts (semantic memory)
// ---------------------------------------------------------------------------

/// A design decision confirmed by the user during the clarification phase.
/// These are immutable once confirmed (unless explicitly overridden).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmedFact {
    /// Short topic name (e.g. "language", "error_handling").
    pub topic: String,
    /// The confirmed decision (e.g. "Rust", "Result<T,E>").
    pub content: String,
    /// Which clarification round this was confirmed in.
    pub round: u32,
    /// When the fact was confirmed.
    pub confirmed_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Episode Records (episodic memory)
// ---------------------------------------------------------------------------

/// Record of a single round in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    /// Which phase this round belongs to.
    pub phase: Phase,
    /// Round number within the phase.
    pub round_number: u32,
    /// The reasoning brain's output (if any).
    pub reasoning_output: Option<String>,
    /// Step ID (only during execution phase).
    pub step_id: Option<u32>,
    /// Step execution result (only during execution phase).
    pub step_result: Option<super::plan::StepResult>,
    /// Questions asked to the user and their answers.
    pub user_qa: Vec<(String, String)>,
    /// When this round started.
    pub started_at: DateTime<Utc>,
}

/// Which phase of execution a round belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Clarification loop (Phase 2).
    Clarification,
    /// Step execution (Phase 3).
    Execution,
}

// ---------------------------------------------------------------------------
// Step Summary (compressed episodic memory)
// ---------------------------------------------------------------------------

/// Compressed summary of a completed step's tool calls and outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSummary {
    /// The step this summary covers.
    pub step_id: String,
    /// High-value content preserved verbatim.
    pub preserved: Vec<String>,
    /// Compressed summary of low-value content.
    pub summary: String,
    /// Reference ID for full original content in long-term storage.
    pub storage_id: String,
}

// ---------------------------------------------------------------------------
// Memory Scoring
// ---------------------------------------------------------------------------

/// Scoring dimensions for memory relevance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryScore {
    /// Semantic similarity to current task (LLM-judged).
    pub relevance: f64,
    /// Time decay — more recent is higher.
    pub recency: f64,
    /// Intrinsic importance: confirmed=1.0, code=0.8, exploration=0.3.
    pub importance: f64,
    /// How often this memory has been recalled.
    pub frequency: f64,
}

impl MemoryScore {
    /// Default weights for the composite score.
    pub const W_RELEVANCE: f64 = 0.4;
    pub const W_RECENCY: f64 = 0.2;
    pub const W_IMPORTANCE: f64 = 0.3;
    pub const W_FREQUENCY: f64 = 0.1;

    /// Compute the weighted composite score.
    pub fn weight(&self) -> f64 {
        Self::W_RELEVANCE * self.relevance
            + Self::W_RECENCY * self.recency
            + Self::W_IMPORTANCE * self.importance
            + Self::W_FREQUENCY * self.frequency
    }
}

/// Thresholds for injection tiers.
pub mod injection_tier {
    /// Weight >= this → full content injection.
    pub const FULL: f64 = 0.8;
    /// Weight >= this → summary injection.
    pub const SUMMARY: f64 = 0.5;
    /// Weight >= this → mention injection.
    pub const MENTION: f64 = 0.3;
}
