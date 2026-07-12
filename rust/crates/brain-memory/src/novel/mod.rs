//! 小说项目级记忆。
//!
//! 项目快照是权威数据源，Novel 图谱是低上下文召回索引。所有写入先经过
//! revision 与事实冲突检查，再原子提交快照并 best-effort 镜像到图谱。

mod consistency;
mod recall;
mod schema;
mod store;

pub use consistency::{check_consistency, ConsistencyIssue, ConsistencyReport, IssueSeverity};
pub use recall::build_recall_pack;
pub use schema::{
    CanonStatus, CommitReport, ConflictRecord, ExperienceCandidate, ForeshadowingUpdate, NovelFact,
    NovelFactKind, NovelMemoryDelta, NovelProject, NovelProjectProgress, NovelProjectStatus,
    NovelRecallPack, NovelTaskType, PlotUpdate, ProposedFact, StateChange,
};
pub use store::NovelMemoryStore;
