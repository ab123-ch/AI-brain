pub mod backlog;
pub mod capability_tree;
pub mod coordinator;
pub mod cycle_runner;
pub mod error;
pub mod evo_log;
pub mod evo_prompt;
pub mod evolution_engine;
pub mod guard;
pub mod memory_access;
pub mod sandbox;
pub mod target;
pub mod tdd_runner;
pub mod trigger;
pub mod verification;
pub mod web_search;

pub use coordinator::{
    extract_cycle_metadata, target_id_from_candidate, EvoConfig, EvoTargetCandidate,
    EvolutionCoordinator, NightSessionOutput, NightSessionResult, SharedResources,
};
pub use cycle_runner::{
    describe_target, CycleConfig, CycleResult, CycleRunner, LearnResult, PerceiveResult,
    RegisterResult, ResearchResult, SkillDraft, SynthesizeResult, VerificationResult,
    VerificationSpec,
};
pub use error::{EvolverError, Result};
pub use evo_prompt::EvoPromptContext;
pub use evolution_engine::{EvolutionEngine, EvolutionStatus};
pub use guard::Guard;
pub use memory_access::{
    MemoryAccess, MemoryLayer, MemoryWriteRequest, MockMemoryAccess, PyramidMemoryAccess,
    RecallResult, StubMemoryAccess,
};
pub use sandbox::Sandbox;
pub use tdd_runner::EvolutionGoal;
pub use tdd_runner::{TddPhase, TddRunner};
pub use trigger::{EvolutionTrigger, TriggerConfig, TriggerDecision};
pub use verification::VerificationAgent;
pub use web_search::{MockWebSearch, PageContent, SearchResult, StubWebSearch, WebSearch};
