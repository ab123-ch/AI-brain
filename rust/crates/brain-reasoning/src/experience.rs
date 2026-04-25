use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// 单条经验路径
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceEntry {
    pub id: String,
    /// 触发模式（关键词组合，逗号分隔）
    pub trigger_pattern: String,
    /// 推理路径步骤
    pub reasoning_path: Vec<String>,
    /// 使用的工具列表
    pub tools_used: Vec<String>,
    /// 成功率 [0, 1]
    pub success_rate: f64,
    /// 使用次数
    pub usage_count: u32,
    /// 是否为反面案例
    pub is_negative: bool,
    /// 修改的文件（可选）
    pub files_modified: Vec<String>,
    /// 失败原因（仅反面案例）
    pub failure_reason: Option<String>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 最后使用时间
    pub last_used: DateTime<Utc>,
}

/// 经验路径库
///
/// 存储 `trigger_pattern → reasoning_path → success_rate` 的映射。
/// 快思考通过关键词匹配命中经验；命中且 success_rate > 阈值则复用路径。
pub struct ExperienceStore {
    /// trigger 关键词 → experience ids 的索引
    keyword_index: HashMap<String, Vec<String>>,
    /// id → entry
    entries: HashMap<String, ExperienceEntry>,
    /// 持久化目录
    storage_path: PathBuf,
    /// 最小成功率阈值
    min_success_rate: f64,
}

impl ExperienceStore {
    pub fn new(storage_path: PathBuf, min_success_rate: f64) -> Self {
        let mut store = Self {
            keyword_index: HashMap::new(),
            entries: HashMap::new(),
            storage_path,
            min_success_rate,
        };
        if let Err(e) = store.load_from_disk() {
            tracing::debug!("经验库加载失败（首次运行正常）: {e}");
        }
        store
    }

    /// 存入一条经验
    pub fn store(&mut self, entry: ExperienceEntry) {
        // 更新关键词索引
        let keywords = Self::extract_keywords(&entry.trigger_pattern);
        for kw in keywords {
            self.keyword_index
                .entry(kw)
                .or_default()
                .push(entry.id.clone());
        }
        self.entries.insert(entry.id.clone(), entry);
    }

    /// 获取一条经验
    pub fn get(&self, id: &str) -> Option<&ExperienceEntry> {
        self.entries.get(id)
    }

    /// 记录使用（增加 usage_count，更新 last_used）
    pub fn record_usage(&mut self, id: &str, success: bool) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.usage_count += 1;
            entry.last_used = Utc::now();

            // 更新成功率（指数移动平均）
            let alpha = 0.3;
            let outcome = if success { 1.0 } else { 0.0 };
            entry.success_rate = alpha * outcome + (1.0 - alpha) * entry.success_rate;

            if !success && entry.success_rate < 0.2 {
                entry.is_negative = true;
            }
        }
    }

    /// 标记为反面案例
    pub fn mark_negative(&mut self, id: &str, reason: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.is_negative = true;
            entry.failure_reason = Some(reason.into());
        }
    }

    /// 按关键词搜索匹配的经验
    ///
    /// 返回匹配的经验，按 success_rate 降序排列。
    /// 过滤掉 success_rate < min_success_rate 和 is_negative 的条目。
    pub fn search(&self, keywords: &[String], limit: usize) -> Vec<&ExperienceEntry> {
        let mut scored: Vec<(f64, &ExperienceEntry)> = Vec::new();

        for kw in keywords {
            let kw_lower = kw.to_lowercase();
            if let Some(ids) = self.keyword_index.get(&kw_lower) {
                for id in ids {
                    if let Some(entry) = self.entries.get(id) {
                        if entry.is_negative || entry.success_rate < self.min_success_rate {
                            continue;
                        }
                        // 避免重复
                        if scored.iter().any(|(_, e)| e.id == entry.id) {
                            continue;
                        }
                        // 匹配分数 = 关键词匹配数 * success_rate
                        let match_score = self.compute_match_score(entry, keywords);
                        scored.push((match_score, entry));
                    }
                }
            }
        }

        // 也做 trigger_pattern 模糊匹配（双向）
        for entry in self.entries.values() {
            if entry.is_negative || entry.success_rate < self.min_success_rate {
                continue;
            }
            if scored.iter().any(|(_, e)| e.id == entry.id) {
                continue;
            }
            let pattern_lower = entry.trigger_pattern.to_lowercase();

            // 方向1: trigger_pattern 包含关键词
            let pattern_contains_kw = keywords
                .iter()
                .any(|kw| pattern_lower.contains(&kw.to_lowercase()));

            // 方向2: 关键词包含 trigger_pattern 的成分
            let trigger_kws = Self::extract_keywords(&entry.trigger_pattern);
            let kw_contains_pattern = trigger_kws.iter().any(|trigger_kw| {
                keywords
                    .iter()
                    .any(|msg_kw| msg_kw.to_lowercase().contains(&trigger_kw.to_lowercase()))
            });

            if pattern_contains_kw || kw_contains_pattern {
                let score = self.compute_match_score(entry, keywords);
                scored.push((score, entry));
            }
        }

        // 按分数降序
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored.into_iter().map(|(_, e)| e).collect()
    }

    /// 快速检查是否有匹配经验（用于 fast_think 判断）
    pub fn has_match(&self, keywords: &[String]) -> bool {
        // 先检查精确索引匹配
        for kw in keywords {
            let kw_lower = kw.to_lowercase();
            if let Some(ids) = self.keyword_index.get(&kw_lower) {
                for id in ids {
                    if let Some(entry) = self.entries.get(id) {
                        if !entry.is_negative && entry.success_rate >= self.min_success_rate {
                            return true;
                        }
                    }
                }
            }
        }

        // 模糊匹配：双向包含检查
        for entry in self.entries.values() {
            if entry.is_negative || entry.success_rate < self.min_success_rate {
                continue;
            }
            let pattern_lower = entry.trigger_pattern.to_lowercase();

            // 方向1: trigger_pattern 包含关键词
            let pattern_contains_kw = keywords
                .iter()
                .any(|kw| pattern_lower.contains(&kw.to_lowercase()));

            // 方向2: 关键词包含 trigger_pattern 的成分
            let kw_contains_pattern =
                Self::extract_keywords(&entry.trigger_pattern)
                    .iter()
                    .any(|trigger_kw| {
                        keywords.iter().any(|msg_kw| {
                            msg_kw.to_lowercase().contains(&trigger_kw.to_lowercase())
                        })
                    });

            if pattern_contains_kw || kw_contains_pattern {
                return true;
            }
        }

        false
    }

    /// 获取反面案例（用于慢思考参考）
    pub fn get_negative_examples(
        &self,
        keywords: &[String],
        limit: usize,
    ) -> Vec<&ExperienceEntry> {
        let mut results: Vec<&ExperienceEntry> = self
            .entries
            .values()
            .filter(|e| e.is_negative)
            .filter(|e| {
                keywords.iter().any(|kw| {
                    e.trigger_pattern
                        .to_lowercase()
                        .contains(&kw.to_lowercase())
                })
            })
            .take(limit)
            .collect();
        results.sort_by(|a, b| b.usage_count.cmp(&a.usage_count));
        results
    }

    /// 所有经验数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 持久化到磁盘
    pub fn persist(&self) -> Result<()> {
        use std::fs;

        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.entries)?;
        fs::write(&self.storage_path, data)?;
        Ok(())
    }

    fn load_from_disk(&mut self) -> Result<()> {
        if !self.storage_path.exists() {
            return Ok(());
        }
        let data = std::fs::read_to_string(&self.storage_path)?;
        let entries: HashMap<String, ExperienceEntry> = serde_json::from_str(&data)?;
        for entry in entries.values() {
            let keywords = Self::extract_keywords(&entry.trigger_pattern);
            for kw in keywords {
                self.keyword_index
                    .entry(kw)
                    .or_default()
                    .push(entry.id.clone());
            }
        }
        self.entries = entries;
        Ok(())
    }

    fn extract_keywords(pattern: &str) -> Vec<String> {
        pattern
            .split(&[',', '，', ' ', '、'][..])
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[allow(clippy::cast_precision_loss)]
    fn compute_match_score(&self, entry: &ExperienceEntry, keywords: &[String]) -> f64 {
        let trigger_lower = entry.trigger_pattern.to_lowercase();
        let matched_count = keywords
            .iter()
            .filter(|kw| trigger_lower.contains(&kw.to_lowercase()))
            .count();
        let keyword_ratio = matched_count as f64 / keywords.len().max(1) as f64; // experience count fits in f64
        keyword_ratio * entry.success_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> ExperienceStore {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("experience.json");
        let store = ExperienceStore::new(path, 0.5);
        // TempDir 会被自动清理，经验库使用的是文件路径
        std::mem::forget(tmp);
        store
    }

    fn make_entry(id: &str, pattern: &str, success_rate: f64) -> ExperienceEntry {
        ExperienceEntry {
            id: id.into(),
            trigger_pattern: pattern.into(),
            reasoning_path: vec!["分析需求".into(), "查找历史".into(), "执行操作".into()],
            tools_used: vec!["Read".into(), "Edit".into()],
            success_rate,
            usage_count: 1,
            is_negative: false,
            files_modified: vec!["src/main.rs".into()],
            failure_reason: None,
            created_at: Utc::now(),
            last_used: Utc::now(),
        }
    }

    #[test]
    fn store_and_get() {
        let mut store = make_store();
        let entry = make_entry("exp-001", "代码,bug修复", 0.9);
        store.store(entry);

        let got = store.get("exp-001").unwrap();
        assert_eq!(got.trigger_pattern, "代码,bug修复");
        assert!((got.success_rate - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn search_by_keyword() {
        let mut store = make_store();
        store.store(make_entry("exp-001", "代码,bug修复", 0.9));
        store.store(make_entry("exp-002", "文档,编写", 0.8));
        store.store(make_entry("exp-003", "代码,重构", 0.85));

        let results = store.search(&["代码".into()], 10);
        assert_eq!(results.len(), 2);
        // bug修复的 success_rate 更高，排前面
        assert_eq!(results[0].id, "exp-001");
    }

    #[test]
    fn search_filters_low_success_rate() {
        let mut store = make_store();
        store.store(make_entry("exp-001", "测试", 0.3)); // 低于阈值 0.5
        store.store(make_entry("exp-002", "测试", 0.8));

        let results = store.search(&["测试".into()], 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "exp-002");
    }

    #[test]
    fn search_filters_negative() {
        let mut store = make_store();
        let mut entry = make_entry("exp-001", "测试", 0.7);
        entry.is_negative = true;
        store.store(entry);

        let results = store.search(&["测试".into()], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn has_match_works() {
        let mut store = make_store();
        assert!(!store.has_match(&["代码".into()]));

        store.store(make_entry("exp-001", "代码,开发", 0.9));
        assert!(store.has_match(&["代码".into()]));
    }

    #[test]
    fn record_usage_updates_success_rate() {
        let mut store = make_store();
        store.store(make_entry("exp-001", "测试", 0.9));

        // 记录失败
        store.record_usage("exp-001", false);
        let entry = store.get("exp-001").unwrap();
        assert_eq!(entry.usage_count, 2);
        assert!(entry.success_rate < 0.9); // 成功率下降了
    }

    #[test]
    fn mark_negative() {
        let mut store = make_store();
        store.store(make_entry("exp-001", "测试", 0.7));
        store.mark_negative("exp-001", "路径错误");

        let entry = store.get("exp-001").unwrap();
        assert!(entry.is_negative);
        assert_eq!(entry.failure_reason.as_deref(), Some("路径错误"));
    }

    #[test]
    fn get_negative_examples() {
        let mut store = make_store();
        store.store(make_entry("exp-001", "代码,bug修复", 0.9));

        let mut neg = make_entry("exp-002", "代码,重构", 0.7);
        neg.is_negative = true;
        neg.failure_reason = Some("重构范围过大".into());
        store.store(neg);

        let results = store.get_negative_examples(&["代码".into()], 5);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_negative);
    }

    #[test]
    fn persist_and_reload() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("experience.json");

        // 写入
        {
            let mut store = ExperienceStore::new(path.clone(), 0.5);
            store.store(make_entry("exp-001", "持久化测试", 0.8));
            store.persist().unwrap();
        }

        // 重新加载
        let store2 = ExperienceStore::new(path, 0.5);
        assert_eq!(store2.len(), 1);
        assert!(store2.get("exp-001").is_some());
    }
}
