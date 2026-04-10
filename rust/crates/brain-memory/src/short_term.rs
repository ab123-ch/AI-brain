use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use brain_core::types::MemoryEntry;

use crate::error::Result;
use crate::storage::Storage;

/// L2 短期记忆配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortTermConfig {
    /// 最大条目数，默认 200
    pub max_entries: usize,
    /// 文件名
    pub file_name: String,
}

impl Default for ShortTermConfig {
    fn default() -> Self {
        Self {
            max_entries: 200,
            file_name: "recent.json".into(),
        }
    }
}

/// L2 短期记忆层
///
/// 职责：
/// - 存储最近活跃的记忆条目
/// - 维护关键词索引（tag → entry_ids）
/// - importance 控制召回优先级，不影响存储
/// - 持久化到 `memory/short-term/recent.json`
pub struct ShortTermLayer {
    entries: HashMap<String, MemoryEntry>,
    /// tag → entry_ids 索引
    tag_index: HashMap<String, HashSet<String>>,
    config: ShortTermConfig,
    storage: Storage,
}

impl ShortTermLayer {
    pub fn new(storage: Storage, config: ShortTermConfig) -> Self {
        let mut layer = Self {
            entries: HashMap::new(),
            tag_index: HashMap::new(),
            config,
            storage,
        };
        // 尝试从磁盘加载
        if let Ok(loaded) = layer.load_from_disk() {
            layer.entries = loaded;
            layer.rebuild_tag_index();
        }
        layer
    }

    /// 存入一条短期记忆
    pub fn store(&mut self, entry: MemoryEntry) {
        // 更新 tag 索引
        for tag in &entry.tags {
            self.tag_index
                .entry(tag.clone())
                .or_default()
                .insert(entry.id.clone());
        }
        self.entries.insert(entry.id.clone(), entry);
    }

    /// 获取一条记忆
    pub fn get(&self, id: &str) -> Option<&MemoryEntry> {
        self.entries.get(id)
    }

    /// 更新记忆的 importance（影响召回，不删数据）
    pub fn update_importance(&mut self, id: &str, importance: f64) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.importance = importance;
        }
    }

    /// 标记为已巩固
    pub fn mark_consolidated(&mut self, id: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.consolidated = true;
        }
    }

    /// 按关键词搜索
    pub fn search(
        &self,
        keywords: &[String],
        min_importance: f64,
        limit: usize,
    ) -> Vec<MemoryEntry> {
        let mut results: Vec<MemoryEntry> = Vec::new();

        // 先从 tag 索引精确匹配
        for kw in keywords {
            if let Some(ids) = self.tag_index.get(kw) {
                for id in ids {
                    if let Some(entry) = self.entries.get(id) {
                        if entry.importance >= min_importance {
                            results.push(entry.clone());
                        }
                    }
                }
            }
        }

        // 补充：内容模糊匹配
        for entry in self.entries.values() {
            if results.len() >= limit {
                break;
            }
            if entry.importance < min_importance {
                continue;
            }
            let already_found = results.iter().any(|r| r.id == entry.id);
            if already_found {
                continue;
            }
            let content_lower = entry.content.to_lowercase();
            let matched = keywords
                .iter()
                .any(|kw| content_lower.contains(&kw.to_lowercase()));
            if matched {
                results.push(entry.clone());
            }
        }

        // 按 importance 降序排序
        results.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    /// 获取所有未巩固的条目
    pub fn unconsolidated(&self) -> Vec<&MemoryEntry> {
        self.entries.values().filter(|e| !e.consolidated).collect()
    }

    /// 所有条目数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 持久化到磁盘
    pub fn persist(&self) -> Result<()> {
        let path = self.storage.short_term_dir().join(&self.config.file_name);
        let entries: Vec<&MemoryEntry> = self.entries.values().collect();
        self.storage.write_json(&path, &entries)?;
        Ok(())
    }

    fn load_from_disk(&self) -> Result<HashMap<String, MemoryEntry>> {
        let path = self.storage.short_term_dir().join(&self.config.file_name);
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let entries: Vec<MemoryEntry> = self.storage.read_json(&path)?;
        Ok(entries.into_iter().map(|e| (e.id.clone(), e)).collect())
    }

    fn rebuild_tag_index(&mut self) {
        self.tag_index.clear();
        for entry in self.entries.values() {
            for tag in &entry.tags {
                self.tag_index
                    .entry(tag.clone())
                    .or_default()
                    .insert(entry.id.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::{KnowledgeSource, MemoryLayer};
    use chrono::Utc;
    use tempfile::TempDir;

    fn make_layer() -> ShortTermLayer {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        std::mem::forget(tmp);
        ShortTermLayer::new(storage, ShortTermConfig::default())
    }

    fn make_entry(id: &str, content: &str, tags: &[&str], importance: f64) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            content: content.into(),
            tags: tags.iter().map(|t| (*t).into()).collect(),
            layer: MemoryLayer::ShortTerm,
            importance,
            source: KnowledgeSource::Memory {
                memory_id: id.into(),
                layer: MemoryLayer::ShortTerm,
            },
            confidence: 0.8,
            reference_count: 0,
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            consolidated: false,
        }
    }

    #[test]
    fn store_and_get() {
        let mut layer = make_layer();
        let entry = make_entry("e1", "清明节是4月5日", &["节假日", "清明节"], 0.8);
        layer.store(entry);

        let got = layer.get("e1").unwrap();
        assert_eq!(got.content, "清明节是4月5日");
        assert_eq!(got.tags.len(), 2);
    }

    #[test]
    fn search_by_tag() {
        let mut layer = make_layer();
        layer.store(make_entry("e1", "清明节是4月5日", &["节假日"], 0.8));
        layer.store(make_entry("e2", "劳动节是5月1日", &["节假日"], 0.7));
        layer.store(make_entry("e3", "Rust编程笔记", &["编程"], 0.9));

        let results = layer.search(&["节假日".into()], 0.0, 10);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_respects_importance() {
        let mut layer = make_layer();
        layer.store(make_entry("e1", "高重要性", &["test"], 0.9));
        layer.store(make_entry("e2", "低重要性", &["test"], 0.1));

        let results = layer.search(&["test".into()], 0.5, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "e1");
    }

    #[test]
    fn search_by_content_fuzzy() {
        let mut layer = make_layer();
        layer.store(make_entry("e1", "记忆系统的架构设计", &["架构"], 0.8));

        // 用 "记忆" 这个不在 tags 里但在 content 里的关键词搜索
        let results = layer.search(&["记忆".into()], 0.0, 10);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn update_importance_no_delete() {
        let mut layer = make_layer();
        layer.store(make_entry("e1", "测试", &["test"], 0.8));
        layer.update_importance("e1", 0.1);

        // 数据还在，只是 importance 降低了
        let entry = layer.get("e1").unwrap();
        assert!((entry.importance - 0.1).abs() < f64::EPSILON);
        assert_eq!(layer.len(), 1);
    }

    #[test]
    fn persist_and_reload() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();

        // 存入并持久化
        {
            let mut layer = ShortTermLayer::new(storage.clone(), ShortTermConfig::default());
            layer.store(make_entry("e1", "持久化测试", &["test"], 0.8));
            layer.persist().unwrap();
        }

        // 重新加载
        let layer2 = ShortTermLayer::new(storage, ShortTermConfig::default());
        assert_eq!(layer2.len(), 1);
        assert_eq!(layer2.get("e1").unwrap().content, "持久化测试");
    }
}
