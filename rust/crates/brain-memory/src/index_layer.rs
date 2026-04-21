//! L2 索引摘要层 — 分组归类 + 反向索引
//!
//! 职责：
//! - 存储对 L1 原始记忆的归纳摘要
//! - 维护反向索引（source_refs → L1 原始条目）
//! - 按分类和关键词搜索
//! - 追踪召回计数（GC 决策用）

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Storage;

/// L2 索引条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    /// 对 L1 的归纳摘要（2-3 句话）
    pub summary: String,
    /// 分类 (debugging/development/architecture/testing/deployment/general)
    pub category: String,
    /// 关键词索引
    pub tags: Vec<String>,
    /// 重要性评分 [0.0, 1.0]
    pub importance: f64,
    /// 反向索引 → L1 原始条目
    pub source_refs: Vec<SourceRef>,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    /// 召回计数（GC 决策用）
    pub recall_count: u32,
}

/// 反向索引 — 指向 L1 原始条目的来源引用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    /// L1 会话文件名（不含 .jsonl 后缀）
    pub session_file: String,
    /// JSONL 行号范围 [start, end]
    pub line_range: (usize, usize),
    /// 原始条目 ID 列表
    pub entry_ids: Vec<String>,
}

/// L2 索引摘要层
pub struct IndexLayer {
    storage: Storage,
}

impl IndexLayer {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 存入一条索引摘要（原子写入）
    pub fn store(&self, entry: &IndexEntry) -> Result<()> {
        let dir = self.storage.index_category_dir(&entry.category);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", entry.id));
        self.storage.write_json_atomic(&path, &entry)
    }

    /// 按 ID 获取索引条目（需指定分类）
    pub fn get(&self, id: &str, category: &str) -> Result<IndexEntry> {
        let path = self
            .storage
            .index_category_dir(category)
            .join(format!("{id}.json"));
        if !path.exists() {
            return Err(crate::error::MemoryError::EntryNotFound(id.into()));
        }
        self.storage.read_json(&path)
    }

    /// 按关键词和分类搜索索引条目
    pub fn search(
        &self,
        keywords: &[String],
        category: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IndexEntry>> {
        let files = match category {
            Some(cat) => self
                .storage
                .list_json_files(&self.storage.index_category_dir(cat))?,
            None => self
                .storage
                .list_json_files_recursive(&self.storage.index_dir())?,
        };

        let mut results = Vec::new();
        for file in &files {
            if results.len() >= limit {
                break;
            }
            let entry: IndexEntry = match self.storage.read_json(file) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let matched = keywords.iter().any(|kw| {
                entry.summary.to_lowercase().contains(&kw.to_lowercase())
                    || entry.tags.iter().any(|t| t.eq_ignore_ascii_case(kw))
            });

            if matched {
                results.push(entry);
            }
        }
        Ok(results)
    }

    /// 按分类搜索
    pub fn search_by_category(&self, category: &str, limit: usize) -> Result<Vec<IndexEntry>> {
        let dir = self.storage.index_category_dir(category);
        let files = self.storage.list_json_files(&dir)?;
        let mut results = Vec::new();

        for file in &files {
            if results.len() >= limit {
                break;
            }
            if let Ok(entry) = self.storage.read_json::<IndexEntry>(file) {
                results.push(entry);
            }
        }
        Ok(results)
    }

    /// 递增召回计数
    pub fn increment_recall(&self, id: &str, category: &str) -> Result<()> {
        let mut entry = self.get(id, category)?;
        entry.recall_count += 1;
        entry.last_accessed = Utc::now();
        self.store(&entry)
    }

    /// 列出所有分类目录名
    pub fn list_categories(&self) -> Result<Vec<String>> {
        let index_dir = self.storage.index_dir();
        if !index_dir.exists() {
            return Ok(Vec::new());
        }
        let mut categories = Vec::new();
        for entry in std::fs::read_dir(&index_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    categories.push(name.to_string());
                }
            }
        }
        categories.sort();
        Ok(categories)
    }

    /// 统计总条目数
    pub fn count(&self) -> Result<u32> {
        let files = self
            .storage
            .list_json_files_recursive(&self.storage.index_dir())?;
        Ok(files.len() as u32)
    }

    /// 收集所有 source_refs（GC 用）
    pub fn collect_all_source_refs(&self) -> Result<Vec<SourceRef>> {
        let files = self
            .storage
            .list_json_files_recursive(&self.storage.index_dir())?;
        let mut refs = Vec::new();
        for file in &files {
            if let Ok(entry) = self.storage.read_json::<IndexEntry>(file) {
                refs.extend(entry.source_refs);
            }
        }
        Ok(refs)
    }

    /// 收集所有索引条目中引用的 L1 entry ID 集合（GC 用）
    pub fn collect_all_referenced_entry_ids(&self) -> Result<std::collections::HashSet<String>> {
        let refs = self.collect_all_source_refs()?;
        let mut ids = std::collections::HashSet::new();
        for r#ref in refs {
            for id in r#ref.entry_ids {
                ids.insert(id);
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_layer() -> IndexLayer {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        std::mem::forget(tmp);
        IndexLayer::new(storage)
    }

    fn make_entry(id: &str, category: &str, summary: &str, tags: &[&str]) -> IndexEntry {
        IndexEntry {
            id: id.into(),
            summary: summary.into(),
            category: category.into(),
            tags: tags.iter().map(|t| (*t).into()).collect(),
            importance: 0.7,
            source_refs: vec![SourceRef {
                session_file: "sess-001".into(),
                line_range: (1, 5),
                entry_ids: vec![format!("raw-{id}")],
            }],
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            recall_count: 0,
        }
    }

    #[test]
    fn store_and_get() {
        let layer = make_layer();
        let entry = make_entry(
            "idx-1",
            "development",
            "实现了记忆脑三层架构",
            &["记忆", "架构"],
        );
        layer.store(&entry).unwrap();

        let got = layer.get("idx-1", "development").unwrap();
        assert_eq!(got.summary, "实现了记忆脑三层架构");
        assert_eq!(got.category, "development");
    }

    #[test]
    fn search_by_keyword() {
        let layer = make_layer();
        layer
            .store(&make_entry(
                "idx-1",
                "development",
                "记忆脑架构设计",
                &["架构"],
            ))
            .unwrap();
        layer
            .store(&make_entry(
                "idx-2",
                "debugging",
                "修复了编译错误",
                &["编译"],
            ))
            .unwrap();

        let results = layer.search(&["记忆".into()], None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "idx-1");
    }

    #[test]
    fn search_by_category() {
        let layer = make_layer();
        layer
            .store(&make_entry("idx-1", "development", "开发条目1", &["开发"]))
            .unwrap();
        layer
            .store(&make_entry("idx-2", "debugging", "调试条目1", &["调试"]))
            .unwrap();
        layer
            .store(&make_entry("idx-3", "development", "开发条目2", &["开发"]))
            .unwrap();

        let results = layer.search_by_category("development", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn list_categories() {
        let layer = make_layer();
        layer
            .store(&make_entry("idx-1", "development", "开发", &["开发"]))
            .unwrap();
        layer
            .store(&make_entry("idx-2", "debugging", "调试", &["调试"]))
            .unwrap();

        let cats = layer.list_categories().unwrap();
        assert_eq!(cats, vec!["debugging", "development"]);
    }

    #[test]
    fn increment_recall() {
        let layer = make_layer();
        layer
            .store(&make_entry("idx-1", "development", "测试召回", &["测试"]))
            .unwrap();

        layer.increment_recall("idx-1", "development").unwrap();
        layer.increment_recall("idx-1", "development").unwrap();

        let got = layer.get("idx-1", "development").unwrap();
        assert_eq!(got.recall_count, 2);
    }

    #[test]
    fn collect_source_refs() {
        let layer = make_layer();
        layer
            .store(&make_entry("idx-1", "development", "条目1", &["测试"]))
            .unwrap();
        layer
            .store(&make_entry("idx-2", "debugging", "条目2", &["测试"]))
            .unwrap();

        let refs = layer.collect_all_source_refs().unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn count_entries() {
        let layer = make_layer();
        assert_eq!(layer.count().unwrap(), 0);
        layer
            .store(&make_entry("idx-1", "development", "条目1", &["测试"]))
            .unwrap();
        layer
            .store(&make_entry("idx-2", "debugging", "条目2", &["测试"]))
            .unwrap();
        assert_eq!(layer.count().unwrap(), 2);
    }
}
