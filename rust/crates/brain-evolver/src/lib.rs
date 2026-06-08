pub mod backlog;
pub mod error;
pub mod evolution_engine;
pub mod evolver_brain;
pub mod guard;
pub mod idle_scanner;
pub mod sandbox;
pub mod target;
pub mod tdd_runner;

pub use error::{EvolverError, Result};
pub use evolution_engine::{EvolutionEngine, EvolutionResult, EvolutionStatus};
pub use evolver_brain::EvolverBrain;
pub use guard::Guard;
pub use idle_scanner::{EvolutionSuggestion, Finding, IdleScanner};
pub use sandbox::Sandbox;
pub use tdd_runner::EvolutionGoal;
pub use tdd_runner::{TddPhase, TddRunner};
