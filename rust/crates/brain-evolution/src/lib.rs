pub mod dormancy;
pub mod error;
pub mod registry;
pub mod suggestion;
pub mod template;

pub use dormancy::{DormancyManager, DormantRecord};
pub use error::{EvolutionError, Result};
pub use registry::{
    ActiveBrainEntry, BrainRegistry, BrainRegistryStatus, BrainState, BrainStatusEntry,
};
pub use suggestion::{CreationSuggestion, SuggestionEngine, TaskPattern};
pub use template::{BrainTemplate, FastThinkRule};
