//! 自进化规则存储与更新（v2 四步分析第四步产出）
//!
//! 职责：
//! - 持久化自进化规则到 `memory/evolution/` 目录
//! - 按优先级排序查询规则
//! - 合并新规则（内容去重）
//! - 提供活跃规则快照

use chrono::Utc;

use brain_core::types::EvolutionRule;

use crate::error::Result;
use crate::storage::Storage;

/// 自进化规则存储管理器
pub struct EvolutionStore {
    storage: Storage,
}

impl EvolutionStore {
    /// 创建自进化规则存储管理器
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 获取存储根目录
    pub fn base_dir(&self) -> &std::path::Path {
        self.storage.base_dir()
    }

    /// 获取规则存储目录
    fn evolution_dir(&self) -> std::path::PathBuf {
        self.storage.base_dir().join("memory").join("evolution")
    }

    /// 存储一条进化规则（原子写入）
    pub fn store(&self, rule: &EvolutionRule) -> Result<()> {
        let dir = self.evolution_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", rule.id));
        self.storage.write_json_atomic(&path, &rule)
    }

    /// 加载所有进化规则
    pub fn load_all(&self) -> Result<Vec<EvolutionRule>> {
        let dir = self.evolution_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let files = self.storage.list_json_files(&dir)?;
        let mut rules = Vec::new();
        for file in &files {
            if let Ok(rule) = self.storage.read_json::<EvolutionRule>(file) {
                rules.push(rule);
            }
        }
        Ok(rules)
    }

    /// 按优先级降序加载规则
    pub fn load_sorted_by_priority(&self) -> Result<Vec<EvolutionRule>> {
        let mut rules = self.load_all()?;
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(rules)
    }

    /// 加载高优先级规则（priority >= threshold）
    pub fn load_high_priority(&self, min_priority: u8) -> Result<Vec<EvolutionRule>> {
        let all = self.load_all()?;
        Ok(all
            .into_iter()
            .filter(|r| r.priority >= min_priority)
            .collect())
    }

    /// 合并 LLM 分析产出的新进化规则
    ///
    /// 策略：按 rule 文本内容精确匹配去重
    /// - 匹配到：保留优先级更高的那个
    /// - 未匹配：新建规则
    pub fn merge_analysis(&self, new_rules: &[NewEvolutionRule]) -> Result<Vec<EvolutionRule>> {
        let mut existing = self.load_all()?;
        let mut result = Vec::new();

        for new_rule in new_rules {
            let normalized_rule = new_rule.rule.trim().to_lowercase();

            if let Some(existing_rule) = existing
                .iter_mut()
                .find(|r| r.rule.trim().to_lowercase() == normalized_rule)
            {
                // 保留更高优先级
                if new_rule.priority > existing_rule.priority {
                    existing_rule.priority = new_rule.priority;
                }
                // 合并来源踩坑ID
                for pid in &new_rule.source_pitfall_ids {
                    if !existing_rule.source_pitfall_ids.contains(pid) {
                        existing_rule.source_pitfall_ids.push(pid.clone());
                    }
                }
                result.push(existing_rule.clone());
            } else {
                let rule = EvolutionRule {
                    id: format!("evo-{}", Utc::now().timestamp_millis()),
                    rule: new_rule.rule.clone(),
                    source_pitfall_ids: new_rule.source_pitfall_ids.clone(),
                    priority: new_rule.priority,
                    created_at: Utc::now(),
                    superseded: false,
                };
                let cloned = rule.clone();
                self.store(&rule)?;
                existing.push(rule);
                result.push(cloned);
            }
        }

        // 保存所有更新过的规则
        for rule in &existing {
            self.store(rule)?;
        }

        Ok(result)
    }

    /// 统计总规则数
    pub fn count(&self) -> Result<u32> {
        Ok(self.load_all()?.len() as u32)
    }

    /// 标记为已取代
    pub fn mark_superseded(&self, id: &str) -> Result<()> {
        let all = self.load_all()?;
        for mut rule in all {
            if rule.id == id {
                rule.superseded = true;
                self.store(&rule)?;
                return Ok(());
            }
        }
        Ok(())
    }

    /// 加载未取代的规则（按优先级排序）
    pub fn load_active(&self) -> Result<Vec<EvolutionRule>> {
        let mut rules = self.load_all()?;
        rules.retain(|r| !r.superseded);
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(rules)
    }
}

/// 新进化规则输入（LLM 分析产出）
#[derive(Debug, Clone)]
pub struct NewEvolutionRule {
    pub rule: String,
    pub source_pitfall_ids: Vec<String>,
    pub priority: u8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, EvolutionStore) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        (tmp, EvolutionStore::new(storage))
    }

    fn make_rule(id: &str, rule: &str, priority: u8) -> EvolutionRule {
        EvolutionRule {
            id: id.into(),
            rule: rule.into(),
            source_pitfall_ids: vec!["pit-1".into()],
            priority,
            created_at: Utc::now(),
            superseded: false,
        }
    }

    #[test]
    fn store_and_load_all() {
        let (_tmp, store) = make_store();
        store.store(&make_rule("e1", "总是先读取文件", 3)).unwrap();
        store
            .store(&make_rule("e2", "使用 cargo clippy", 4))
            .unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn load_sorted_by_priority() {
        let (_tmp, store) = make_store();
        store.store(&make_rule("e1", "低优先级", 1)).unwrap();
        store.store(&make_rule("e2", "高优先级", 5)).unwrap();
        store.store(&make_rule("e3", "中优先级", 3)).unwrap();

        let sorted = store.load_sorted_by_priority().unwrap();
        assert_eq!(sorted[0].priority, 5);
        assert_eq!(sorted[1].priority, 3);
        assert_eq!(sorted[2].priority, 1);
    }

    #[test]
    fn load_high_priority_filters() {
        let (_tmp, store) = make_store();
        store.store(&make_rule("e1", "低", 1)).unwrap();
        store.store(&make_rule("e2", "高", 5)).unwrap();

        let high = store.load_high_priority(3).unwrap();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].priority, 5);
    }

    #[test]
    fn merge_analysis_creates_new() {
        let (_tmp, store) = make_store();
        let result = store
            .merge_analysis(&[NewEvolutionRule {
                rule: "失败后检查环境变量".into(),
                source_pitfall_ids: vec!["pit-1".into()],
                priority: 4,
            }])
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].priority, 4);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn merge_analysis_deduplicates_by_content() {
        let (_tmp, store) = make_store();

        // 第一次
        store
            .merge_analysis(&[NewEvolutionRule {
                rule: "总是先运行测试".into(),
                source_pitfall_ids: vec!["pit-1".into()],
                priority: 3,
            }])
            .unwrap();

        // 第二次：相同规则，更高优先级
        let result = store
            .merge_analysis(&[NewEvolutionRule {
                rule: "总是先运行测试".into(),
                source_pitfall_ids: vec!["pit-2".into()],
                priority: 5,
            }])
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].priority, 5);
        // 合并了来源
        assert_eq!(result[0].source_pitfall_ids.len(), 2);
        // 只有一条规则
        assert_eq!(store.count().unwrap(), 1);
    }
}
