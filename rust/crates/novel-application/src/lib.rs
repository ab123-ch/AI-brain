//! Novel-owned application, persistence, migration, and knowledge projection boundary.

mod application;
mod error;
mod migration;
mod projection;
mod store;

pub use application::{
    NovelApplicationService, NovelApplicationStatus, NovelProjectStatusView, NovelResourcePort,
    StoreWorkflowEnvironment, TaskApplicationPort,
};
pub use error::{NovelApplicationError, Result};
pub use migration::{LegacyNovelImporter, MigrationReport};
pub use projection::{NovelProjectionReport, NovelProjectionWorker};
pub use store::{NovelDomainStore, NovelOutboxRecord};
