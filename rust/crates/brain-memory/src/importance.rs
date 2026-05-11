//! importance 衰减与强化管理
//!
//! 职责：
//! - 四步分析时衰减 Summary 条目（超过 30 天 → 标记 superseded）
//!
//! 注意：Subconscious 已迁移到叙事模型，不再有 importance 字段，衰减/强化逻辑已移除。

use std::path::Path;

use chrono::Utc;

use crate::error::Result;
use crate::memory_iteration::MemoryStoreType;
use crate::storage::Storage;
use crate::summary::SessionSummaryStore;

/// 衰减报告
#[derive(Debug, Clone)]
pub struct DecayReport {
    /// 被淘汰（标记 superseded）的条目数
    pub pruned_count: usize,
}

/// 淘汰阈值：超过此天数未访问则淘汰
const PRUNE_DAYS: i64 = 30;

/// importance 管理器
pub struct ImportanceManager;

impl ImportanceManager {
    /// 召回时强化（Subconscious 已迁移为叙事模型，不再有 importance 强化）
    ///
    /// 保留接口兼容性，对 Subconscious 类型无操作。
    pub fn reinforce(
        _storage: &Storage,
        _entry_ids: &[String],
        store_type: MemoryStoreType,
    ) -> Result<()> {
        // Subconscious: 叙事模型，无 importance
        // Summary/Pitfall/Evolution: 暂无 importance 字段
        let _ = store_type;
        Ok(())
    }

    /// 四步分析时衰减 + 淘汰
    ///
    /// 规则：
    /// - Summary: 超过 30 天未访问 → 标记 superseded
    pub fn decay_all(base_dir: &Path) -> Result<DecayReport> {
        let storage = Storage::new_lazy(base_dir.to_path_buf());
        let now = Utc::now();
        let mut pruned_count = 0usize;

        // Summary 衰减（没有 importance，只有 superseded 淘汰逻辑）
        let summary_store = SessionSummaryStore::new(storage.clone());
        let summaries = summary_store.list_all()?;
        for s in &summaries {
            if s.superseded || s.archived {
                continue;
            }
            let days_since = (now - s.created_at).num_days();
            if days_since > PRUNE_DAYS {
                summary_store.mark_superseded(&s.session_id)?;
                pruned_count += 1;
            }
        }

        Ok(DecayReport { pruned_count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn reinforce_is_noop_for_subconscious() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();

        // Subconscious 叙事模型不应有 importance 变化
        ImportanceManager::reinforce(&storage, &["any-id".into()], MemoryStoreType::Subconscious)
            .unwrap();
    }

    #[test]
    fn reinforce_is_noop_for_summary() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();

        ImportanceManager::reinforce(&storage, &["any-id".into()], MemoryStoreType::Summary)
            .unwrap();
    }

    #[test]
    fn decay_all_runs_without_subconscious() {
        let tmp = TempDir::new().unwrap();
        let report = ImportanceManager::decay_all(tmp.path()).unwrap();
        assert_eq!(report.pruned_count, 0);
    }

    #[test]
    fn decay_report_has_no_decayed_count() {
        let tmp = TempDir::new().unwrap();
        let _report = ImportanceManager::decay_all(tmp.path()).unwrap();
        // DecayReport 只剩 pruned_count，验证编译通过
    }
}
