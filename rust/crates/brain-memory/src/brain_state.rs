//! 记忆脑上下文快照生成（v2 供主脑上下文重建使用）
//!
//! 职责：
//! - 汇总事实总结、用户画像、踩坑记录、进化规则
//! - 生成 `BrainState` 快照供主脑读取
//! - 将快照持久化到 `memory/snapshots/` 用于调试和审计

use chrono::Utc;

use brain_core::types::{BrainState, SourceRef, SourceRefKind};

use crate::error::Result;
use crate::evolution::EvolutionStore;
use crate::index_layer::IndexLayer;
use crate::pitfall::PitfallStore;
use crate::storage::Storage;
use crate::user_profile::UserProfileStore;

/// 上下文快照生成器
pub struct BrainStateGenerator {
    storage: Storage,
    profile_store: UserProfileStore,
    pitfall_store: PitfallStore,
    evolution_store: EvolutionStore,
    index: IndexLayer,
}

impl BrainStateGenerator {
    /// 创建快照生成器
    pub fn new(
        storage: Storage,
        profile_store: UserProfileStore,
        pitfall_store: PitfallStore,
        evolution_store: EvolutionStore,
        index: IndexLayer,
    ) -> Self {
        Self {
            storage,
            profile_store,
            pitfall_store,
            evolution_store,
            index,
        }
    }

    /// 生成当前 BrainState 快照
    ///
    /// 从各子存储读取最新数据，组装为 BrainState。
    /// fact_summary 由最近一次四步分析产生。
    pub fn generate(&self, fact_summary: &str) -> Result<BrainState> {
        let user_profile = self.profile_store.load()?;
        let active_pitfalls = self.pitfall_store.load_active()?;
        let evolution_rules = self.evolution_store.load_sorted_by_priority()?;

        // 从 L2 索引构建 SourceRef 映射
        let index_entries = self.build_source_refs()?;

        Ok(BrainState {
            fact_summary: fact_summary.to_string(),
            user_profile,
            active_pitfalls,
            evolution_rules,
            index_entries,
            snapshot_at: Utc::now(),
        })
    }

    /// 将快照持久化到磁盘（原子写入，防止中断导致文件损坏）
    pub fn persist_snapshot(&self, state: &BrainState) -> Result<()> {
        let dir = self.storage.base_dir().join("memory").join("snapshots");
        std::fs::create_dir_all(&dir)?;
        let filename = format!("snapshot_{}.json", state.snapshot_at.timestamp());
        let path = dir.join(filename);
        self.storage.write_json_atomic(&path, state)
    }

    /// 从 L2 索引构建 SourceRef 列表
    fn build_source_refs(&self) -> Result<Vec<SourceRef>> {
        let categories = self.index.list_categories()?;
        let mut refs = Vec::new();

        for category in &categories {
            let entries = self.index.search_by_category(category, 20)?;
            for entry in entries {
                // 每个 IndexEntry 的 source_refs 转换为 BrainState 的 SourceRef
                for src_ref in &entry.source_refs {
                    for entry_id in &src_ref.entry_ids {
                        refs.push(SourceRef {
                            kind: SourceRefKind::Message,
                            reference: format!("{}:{}", src_ref.session_file, entry_id),
                            storage_id: entry_id.clone(),
                        });
                    }
                }
            }

            // 限制总数
            if refs.len() >= 50 {
                refs.truncate(50);
                break;
            }
        }

        Ok(refs)
    }

    /// 生成并持久化快照（一步完成）
    pub fn generate_and_persist(&self, fact_summary: &str) -> Result<BrainState> {
        let state = self.generate(fact_summary)?;
        self.persist_snapshot(&state)?;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_generator() -> (TempDir, BrainStateGenerator) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let profile_store = UserProfileStore::new(storage.clone());
        let pitfall_store = PitfallStore::new(storage.clone());
        let evolution_store = EvolutionStore::new(storage.clone());
        let index = IndexLayer::new(storage.clone());

        let generator = BrainStateGenerator::new(
            storage,
            profile_store,
            pitfall_store,
            evolution_store,
            index,
        );
        (tmp, generator)
    }

    #[test]
    fn generate_empty_state() {
        let (_tmp, gen) = make_generator();
        let state = gen.generate("初始状态").unwrap();

        assert_eq!(state.fact_summary, "初始状态");
        assert!(state.active_pitfalls.is_empty());
        assert!(state.evolution_rules.is_empty());
        assert!(state.user_profile.explicit_preferences.is_empty());
    }

    #[test]
    fn generate_and_persist() {
        let (_tmp, gen) = make_generator();
        let state = gen.generate_and_persist("测试快照").unwrap();

        assert_eq!(state.fact_summary, "测试快照");
        // 快照文件应该存在
        let snap_dir = gen.storage.base_dir().join("memory").join("snapshots");
        assert!(snap_dir.exists());
        let files = std::fs::read_dir(&snap_dir).unwrap().count();
        assert_eq!(files, 1);
    }

    #[test]
    fn generate_with_profile_data() {
        let (tmp, gen) = make_generator();

        // 预填充用户画像
        let profile_store = UserProfileStore::new(Storage::new(tmp.path().to_path_buf()).unwrap());
        profile_store
            .merge_analysis(&["偏好 Rust".into()], &[], &[], &[])
            .unwrap();

        let state = gen.generate("有画像数据").unwrap();
        assert_eq!(state.user_profile.explicit_preferences.len(), 1);
    }
}
