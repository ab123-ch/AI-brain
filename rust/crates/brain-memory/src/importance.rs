//! importance 衰减与强化管理
//!
//! 职责：
//! - 召回时强化：importance += 0.05（上限 1.0），更新 last_accessed
//! - 四步分析时衰减：超过 7 天未访问的 importance *= 0.9
//! - 超过 30 天未访问且 importance < 0.3 → 标记 superseded

use std::path::Path;

use chrono::Utc;

use crate::error::Result;
use crate::memory_iteration::MemoryStoreType;
use crate::storage::Storage;
use crate::subconscious::SubconsciousStore;
use crate::summary::SessionSummaryStore;

/// 衰减报告
#[derive(Debug, Clone)]
pub struct DecayReport {
    /// 被衰减的条目数
    pub decayed_count: usize,
    /// 被淘汰（标记 superseded）的条目数
    pub pruned_count: usize,
}

/// 强化增量（可配置）
const REINFORCE_DELTA: f64 = 0.05;
/// importance 上限
const IMPORTANCE_CAP: f64 = 1.0;
/// importance 下限
const IMPORTANCE_FLOOR: f64 = 0.1;
/// 衰减系数
const DECAY_FACTOR: f64 = 0.9;
/// 衰减阈值：超过此天数未访问则衰减
const DECAY_DAYS: i64 = 7;
/// 淘汰阈值：超过此天数未访问 + importance 低于阈值则淘汰
const PRUNE_DAYS: i64 = 30;
/// 淘汰 importance 阈值
const PRUNE_IMPORTANCE_THRESHOLD: f64 = 0.3;

/// importance 管理器
pub struct ImportanceManager;

impl ImportanceManager {
    /// 召回时强化：对被选中的条目 importance += REINFORCE_DELTA（上限 1.0）
    ///
    /// 目前只处理 SubconsciousStore（有 importance 字段）。
    /// 其他 Store 如需 importance 可后续扩展。
    pub fn reinforce(
        storage: &Storage,
        entry_ids: &[String],
        store_type: MemoryStoreType,
    ) -> Result<()> {
        match store_type {
            MemoryStoreType::Subconscious => {
                let store = SubconsciousStore::new(storage.clone());
                for id in entry_ids {
                    let all = store.load_all()?;
                    for mut entry in all {
                        if entry.id == *id {
                            entry.importance =
                                (entry.importance + REINFORCE_DELTA).min(IMPORTANCE_CAP);
                            entry.last_accessed = Utc::now();
                            entry.updated_at = Utc::now();
                            store.store(&entry)?;
                            break;
                        }
                    }
                }
            }
            // Summary/Pitfall/Evolution 暂无 importance 字段，跳过
            MemoryStoreType::Summary | MemoryStoreType::Pitfall | MemoryStoreType::Evolution => {}
        }
        Ok(())
    }

    /// 四步分析时衰减 + 淘汰
    ///
    /// 规则：
    /// - 超过 7 天未访问：importance = max(0.1, importance * 0.9)
    /// - 超过 30 天未访问 + importance < 0.3：标记 superseded
    pub fn decay_all(base_dir: &Path) -> Result<DecayReport> {
        let storage = Storage::new_lazy(base_dir.to_path_buf());
        let now = Utc::now();
        let mut decayed_count = 0usize;
        let mut pruned_count = 0usize;

        // Subconscious 衰减
        let sub_store = SubconsciousStore::new(storage.clone());
        let entries = sub_store.load_all()?;
        for mut entry in entries {
            if entry.superseded {
                continue;
            }
            let days_since_access = (now - entry.last_accessed).num_days();
            if days_since_access > PRUNE_DAYS && entry.importance < PRUNE_IMPORTANCE_THRESHOLD {
                entry.superseded = true;
                entry.updated_at = now;
                sub_store.store(&entry)?;
                pruned_count += 1;
            } else if days_since_access > DECAY_DAYS {
                entry.importance = (entry.importance * DECAY_FACTOR).max(IMPORTANCE_FLOOR);
                entry.updated_at = now;
                sub_store.store(&entry)?;
                decayed_count += 1;
            }
        }

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

        Ok(DecayReport {
            decayed_count,
            pruned_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subconscious::{NewSubconsciousEntry, SubconsciousStore};
    use tempfile::TempDir;

    #[test]
    fn reinforce_increases_importance() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let store = SubconsciousStore::new(storage.clone());

        // 创建条目
        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "test".into(),
                trigger_keywords: vec!["test".into()],
                impression: "test impression".into(),
                pitfall_hint: String::new(),
                reference_hint: "".into(),
                importance: 0.5,
            }])
            .unwrap();

        let entries = store.load_all().unwrap();
        let id = entries[0].id.clone();
        assert_eq!(entries[0].importance, 0.5);

        // 强化
        ImportanceManager::reinforce(&storage, &[id.clone()], MemoryStoreType::Subconscious)
            .unwrap();

        let updated = store.load_all().unwrap();
        assert!((updated[0].importance - 0.55).abs() < 0.01);
    }

    #[test]
    fn reinforce_caps_at_1() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let store = SubconsciousStore::new(storage.clone());

        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "test".into(),
                trigger_keywords: vec!["test".into()],
                impression: "test impression".into(),
                pitfall_hint: String::new(),
                reference_hint: "".into(),
                importance: 0.98,
            }])
            .unwrap();

        let entries = store.load_all().unwrap();
        let id = entries[0].id.clone();

        ImportanceManager::reinforce(&storage, &[id], MemoryStoreType::Subconscious).unwrap();

        let updated = store.load_all().unwrap();
        assert!((updated[0].importance - 1.0).abs() < 0.01);
    }

    #[test]
    fn reinforce_ignores_non_subconscious() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();

        // 对 Summary 类型强化应该无操作
        ImportanceManager::reinforce(&storage, &["any-id".into()], MemoryStoreType::Summary)
            .unwrap();
    }

    #[test]
    fn decay_reduces_importance_for_old_entries() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let store = SubconsciousStore::new(storage.clone());

        // 创建一个条目并设置 last_accessed 为 10 天前
        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "old topic".into(),
                trigger_keywords: vec!["old".into()],
                impression: "old impression".into(),
                pitfall_hint: String::new(),
                reference_hint: "".into(),
                importance: 0.7,
            }])
            .unwrap();

        let mut entry = store.load_all().unwrap().pop().unwrap();
        entry.last_accessed = Utc::now() - chrono::Duration::days(10);
        store.store(&entry).unwrap();

        // 衰减
        let report = ImportanceManager::decay_all(tmp.path()).unwrap();
        assert_eq!(report.decayed_count, 1);
        assert_eq!(report.pruned_count, 0);

        let updated = store.load_all().unwrap();
        // importance *= 0.9 = 0.63
        assert!((updated[0].importance - 0.63).abs() < 0.01);
        assert!(!updated[0].superseded);
    }

    #[test]
    fn decay_prunes_very_old_low_importance() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let store = SubconsciousStore::new(storage.clone());

        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "very old".into(),
                trigger_keywords: vec!["old".into()],
                impression: "very old impression".into(),
                pitfall_hint: String::new(),
                reference_hint: "".into(),
                importance: 0.2,
            }])
            .unwrap();

        let mut entry = store.load_all().unwrap().pop().unwrap();
        entry.last_accessed = Utc::now() - chrono::Duration::days(35);
        store.store(&entry).unwrap();

        let report = ImportanceManager::decay_all(tmp.path()).unwrap();
        assert_eq!(report.pruned_count, 1);

        let updated = store.load_all().unwrap();
        assert!(updated[0].superseded);
    }

    #[test]
    fn decay_skips_recent_entries() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let store = SubconsciousStore::new(storage.clone());

        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "recent".into(),
                trigger_keywords: vec!["recent".into()],
                impression: "recent impression".into(),
                pitfall_hint: String::new(),
                reference_hint: "".into(),
                importance: 0.7,
            }])
            .unwrap();

        let report = ImportanceManager::decay_all(tmp.path()).unwrap();
        assert_eq!(report.decayed_count, 0);
        assert_eq!(report.pruned_count, 0);
    }
}
