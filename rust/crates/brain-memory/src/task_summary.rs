use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use brain_core::types::{KnowledgeSource, MemoryEntry, MemoryLayer};

use crate::error::Result;
use crate::storage::Storage;

/// L0 任务总结条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub task_description: String,
    pub trigger_pattern: String,
    pub reasoning_path: Vec<String>,
    pub mistakes: Vec<MistakeEntry>,
    pub files_modified: Vec<String>,
    pub tools_used: Vec<String>,
    pub success_rate: f64,
    pub shortcuts: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// 犯错记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistakeEntry {
    pub what: String,
    pub why: String,
    pub how_to_avoid: String,
}

/// L0 任务总结层（深度巩固产出）
///
/// 职责：
/// - 一个完整任务的端到端总结
/// - 空闲时由巩固引擎从 L3 原始记忆中提炼
/// - 经验写入推理脑的经验库
/// - 持久化到 `memory/long-term/tasks/{task-id}.json`
pub struct TaskSummaryLayer {
    storage: Storage,
}

impl TaskSummaryLayer {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 存入一个任务总结
    pub fn store(&self, summary: &TaskSummary) -> Result<()> {
        let path = self
            .storage
            .tasks_dir()
            .join(format!("{}.json", summary.id));
        self.storage.write_json(&path, &summary)
    }

    /// 获取一个任务总结
    pub fn get(&self, task_id: &str) -> Result<TaskSummary> {
        let path = self.storage.tasks_dir().join(format!("{task_id}.json"));
        if !path.exists() {
            return Err(crate::error::MemoryError::EntryNotFound(task_id.into()));
        }
        self.storage.read_json(&path)
    }

    /// 按关键词/trigger_pattern 搜索任务总结
    pub fn search(&self, keywords: &[String], limit: usize) -> Result<Vec<MemoryEntry>> {
        let files = self.storage.list_json_files(&self.storage.tasks_dir())?;
        let mut results = Vec::new();

        for file in &files {
            if results.len() >= limit {
                break;
            }
            let summary: TaskSummary = match self.storage.read_json(file) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let matched = keywords.iter().any(|kw| {
                summary
                    .task_description
                    .to_lowercase()
                    .contains(&kw.to_lowercase())
                    || summary
                        .trigger_pattern
                        .to_lowercase()
                        .contains(&kw.to_lowercase())
                    || summary
                        .shortcuts
                        .iter()
                        .any(|s| s.to_lowercase().contains(&kw.to_lowercase()))
            });
            if matched {
                results.push(MemoryEntry {
                    id: summary.id.clone(),
                    content: format!(
                        "任务: {}\n路径: {}\n经验: {}",
                        summary.task_description,
                        summary.reasoning_path.join(" → "),
                        summary.shortcuts.join("; ")
                    ),
                    tags: vec![summary.trigger_pattern.clone()],
                    layer: MemoryLayer::TaskSummary,
                    importance: 0.9, // L0 总结总是高重要性
                    source: KnowledgeSource::Memory {
                        memory_id: summary.id.clone(),
                        layer: MemoryLayer::TaskSummary,
                    },
                    confidence: summary.success_rate,
                    reference_count: 0,
                    created_at: summary.created_at,
                    last_accessed: Utc::now(),
                    consolidated: true,
                });
            }
        }
        Ok(results)
    }

    /// 列出所有任务总结 ID
    pub fn list(&self) -> Result<Vec<String>> {
        let files = self.storage.list_json_files(&self.storage.tasks_dir())?;
        Ok(files
            .iter()
            .filter_map(|f| f.file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect())
    }

    /// 统计任务总结数
    pub fn count(&self) -> Result<u32> {
        Ok(self.list()?.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_layer() -> TaskSummaryLayer {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        std::mem::forget(tmp);
        TaskSummaryLayer::new(storage)
    }

    fn make_summary(id: &str, desc: &str, pattern: &str) -> TaskSummary {
        TaskSummary {
            id: id.into(),
            task_description: desc.into(),
            trigger_pattern: pattern.into(),
            reasoning_path: vec!["分析需求".into(), "设计方案".into(), "实现代码".into()],
            mistakes: vec![MistakeEntry {
                what: "遗漏了边界检查".into(),
                why: "急于完成".into(),
                how_to_avoid: "先写测试再实现".into(),
            }],
            files_modified: vec!["src/main.rs".into()],
            tools_used: vec!["cargo".into()],
            success_rate: 0.85,
            shortcuts: vec!["下次直接复用模板".into()],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn store_and_get() {
        let layer = make_layer();
        let summary = make_summary("task-001", "实现记忆脑", "记忆,存储");
        layer.store(&summary).unwrap();

        let got = layer.get("task-001").unwrap();
        assert_eq!(got.task_description, "实现记忆脑");
        assert_eq!(got.mistakes.len(), 1);
    }

    #[test]
    fn search_by_keyword() {
        let layer = make_layer();
        layer
            .store(&make_summary("task-001", "记忆脑开发", "记忆"))
            .unwrap();
        layer
            .store(&make_summary("task-002", "感知脑开发", "感知"))
            .unwrap();

        let results = layer.search(&["记忆".into()], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].layer, MemoryLayer::TaskSummary);
        assert!(results[0].importance > 0.8);
    }

    #[test]
    fn get_not_found() {
        let layer = make_layer();
        assert!(layer.get("nonexistent").is_err());
    }
}
