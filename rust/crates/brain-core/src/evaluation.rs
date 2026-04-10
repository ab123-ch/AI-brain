//! Evaluation types for the clarification and step-evaluation phases.
//!
//! Defines how ambiguity is reported and how step completion is judged.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Clarification (Phase 2)
// ---------------------------------------------------------------------------

/// Report from the reasoning brain after analyzing a plan for ambiguities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbiguityReport {
    /// Whether the plan is clear enough to proceed.
    pub clear: bool,
    /// List of identified ambiguities that need user input.
    pub ambiguities: Vec<Ambiguity>,
    /// Optional suggestions from the reasoning brain.
    pub suggestions: Vec<String>,
}

/// A single ambiguity found in the plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ambiguity {
    /// Short topic name (e.g. "error_handling").
    pub topic: String,
    /// Specific question to ask the user.
    pub question: String,
    /// Concrete options the user can choose from.
    pub options: Vec<String>,
    /// Why resolving this matters for implementation.
    pub impact: String,
}

// ---------------------------------------------------------------------------
// Step Evaluation (Phase 3)
// ---------------------------------------------------------------------------

/// Evaluation result for a completed step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepEvaluation {
    /// Overall quality score [0.0, 1.0].
    pub score: f64,
    /// Whether the step goal was fully achieved.
    pub goal_achieved: bool,
    /// Brief quality notes.
    pub notes: String,
    /// Specific issues found (empty if all is well).
    pub issues: Vec<String>,
    /// Whether the step passes (score >= 0.7 and goal achieved).
    pub pass: bool,
}

impl StepEvaluation {
    /// Minimum score to pass a step.
    pub const PASS_THRESHOLD: f64 = 0.7;

    /// Create a passing evaluation.
    pub fn passed(score: f64, notes: impl Into<String>) -> Self {
        Self {
            score,
            goal_achieved: true,
            notes: notes.into(),
            issues: vec![],
            pass: true,
        }
    }

    /// Create a failing evaluation.
    pub fn failed(score: f64, notes: impl Into<String>, issues: Vec<String>) -> Self {
        Self {
            score,
            goal_achieved: false,
            notes: notes.into(),
            issues,
            pass: false,
        }
    }
}
