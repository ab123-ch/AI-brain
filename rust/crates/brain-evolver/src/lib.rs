pub mod backlog;
pub mod capability_tree;
pub mod coordinator;
pub mod cycle_runner;
pub mod error;
pub mod evo_log;
pub mod evo_orchestrator;
pub mod evo_prompt;
pub mod evolution_engine;
pub mod evolver_brain;
pub mod guard;
pub mod idle_scanner;
pub mod sandbox;
pub mod target;
pub mod tdd_runner;
pub mod trigger;

pub use coordinator::{EvoConfig, EvoTargetCandidate, EvolutionCoordinator, NightSessionResult};
pub use cycle_runner::{
    CycleConfig, CycleResult, CycleRunner, PerceiveResult, ResearchResult, LearnResult,
    SynthesizeResult, RegisterResult, VerificationResult, SkillDraft, VerificationSpec,
};
pub use evo_orchestrator::{EvoOrchestrator, NightSessionOutput, SharedResources};
pub use evo_prompt::EvoPromptContext;
pub use error::{EvolverError, Result};
pub use evolution_engine::{EvolutionEngine, EvolutionResult, EvolutionStatus};
pub use evolver_brain::EvolverBrain;
pub use guard::Guard;
pub use idle_scanner::{EvolutionSuggestion, Finding, IdleScanner};
pub use sandbox::Sandbox;
pub use tdd_runner::EvolutionGoal;
pub use tdd_runner::{TddPhase, TddRunner};
