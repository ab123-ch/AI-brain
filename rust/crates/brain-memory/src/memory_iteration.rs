//! Step0 记忆迭代核心类型
//!
//! 职责：
//! - 定义记忆之间的关系分类（OVERRIDE/COMPLEMENT/REFINE/UNRELATED）
//! - 定义迭代分析的输入/输出结构
//! - 提供确定性冲突消解逻辑

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 记忆关系类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MemoryRelation {
    /// 新事实覆盖旧事实（互斥，时间戳新的赢）
    Override,
    /// 互补关系（同一话题不同方面，都保留）
    Complement,
    /// 细化关系（新的是旧的深化版本，旧标记 superseded）
    Refine,
    /// 完全无关
    Unrelated,
}

/// 记忆存储类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryStoreType {
    Subconscious,
    Summary,
    Pitfall,
    Evolution,
}

/// 已有记忆条目（供 LLM 分类）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExistingMemory {
    pub id: String,
    pub store_type: MemoryStoreType,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

/// 迭代分析结果（LLM 返回的每条分类）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationResult {
    pub id: String,
    pub relation: MemoryRelation,
}

/// 冲突消解后的动作
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IterationAction {
    /// 标记旧条目 superseded
    MarkSuperseded,
    /// 两边都保留
    KeepBoth,
    /// 替换为新条目（同 MarkSuperseded 效果）
    ReplaceWithNew,
    /// 跳过
    Skip,
}

/// 确定性冲突消解
///
/// 规则：
/// - OVERRIDE → MarkSuperseded（旧条目被取代）
/// - COMPLEMENT → KeepBoth（互补，两边保留）
/// - REFINE → MarkSuperseded（旧条目被细化版取代）
/// - UNRELATED → Skip
pub fn resolve_action(relation: &MemoryRelation) -> IterationAction {
    match relation {
        MemoryRelation::Override => IterationAction::MarkSuperseded,
        MemoryRelation::Complement => IterationAction::KeepBoth,
        MemoryRelation::Refine => IterationAction::MarkSuperseded,
        MemoryRelation::Unrelated => IterationAction::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_override_marks_superseded() {
        assert_eq!(
            resolve_action(&MemoryRelation::Override),
            IterationAction::MarkSuperseded
        );
    }

    #[test]
    fn resolve_complement_keeps_both() {
        assert_eq!(
            resolve_action(&MemoryRelation::Complement),
            IterationAction::KeepBoth
        );
    }

    #[test]
    fn resolve_refine_marks_superseded() {
        assert_eq!(
            resolve_action(&MemoryRelation::Refine),
            IterationAction::MarkSuperseded
        );
    }

    #[test]
    fn resolve_unrelated_skips() {
        assert_eq!(
            resolve_action(&MemoryRelation::Unrelated),
            IterationAction::Skip
        );
    }
}
