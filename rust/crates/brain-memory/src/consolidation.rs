use chrono::Utc;

use brain_core::types::{ConsolidationReport, MemoryEntry};

use crate::error::Result;
use crate::event_index::{EventEntry, EventIndexLayer};
use crate::raw_layer::RawLayer;
use crate::short_term::ShortTermLayer;
use crate::task_summary::{TaskSummary, TaskSummaryLayer};

/// 巩固引擎（空闲时执行）
///
/// 职责：
/// - 从 L3 原始记忆中提取任务级总结 → 写入 L0
/// - 从 L2 短期记忆中提炼事件索引 → 写入 L1
/// - 物理删除：仅在磁盘空间不足时，清理 importance < 0.1 的记忆
pub struct ConsolidationEngine {
    #[allow(dead_code)] // L3 巩固在后续 Phase 启用
    raw: RawLayer,
    short_term: ShortTermLayer,
    event_index: EventIndexLayer,
    task_summary: TaskSummaryLayer,
}

impl ConsolidationEngine {
    pub fn new(
        raw: RawLayer,
        short_term: ShortTermLayer,
        event_index: EventIndexLayer,
        task_summary: TaskSummaryLayer,
    ) -> Self {
        Self {
            raw,
            short_term,
            event_index,
            task_summary,
        }
    }

    /// 执行一轮巩固
    ///
    /// 1. 将 L2 未巩固的条目提炼为 L1 事件
    /// 2. 尝试从 L3 原始记忆中识别完整任务 → 生成 L0 总结
    pub fn run(&mut self) -> Result<ConsolidationReport> {
        let start = std::time::Instant::now();
        let mut report = ConsolidationReport {
            task_summaries_created: 0,
            event_indexes_created: 0,
            memories_consolidated: 0,
            duration_ms: 0,
        };

        // Step 1: L2 → L1 事件索引
        // 先 clone 数据以避免借用冲突
        let unconsolidated: Vec<MemoryEntry> = self
            .short_term
            .unconsolidated()
            .into_iter()
            .cloned()
            .collect();

        for entry in &unconsolidated {
            if let Err(e) = self.consolidate_to_event(entry) {
                tracing::warn!("巩固到 L1 失败: {e}");
                continue;
            }
            report.event_indexes_created += 1;
            report.memories_consolidated += 1;
        }

        // Step 2: 尝试生成 L0 任务总结（简化版：基于 L2 的高重要性条目）
        let high_importance: Vec<&MemoryEntry> = unconsolidated
            .iter()
            .filter(|e| e.importance >= 0.8)
            .collect();

        for entry in high_importance {
            if let Err(e) = self.consolidate_to_task_summary(entry) {
                tracing::warn!("巩固到 L0 失败: {e}");
                continue;
            }
            report.task_summaries_created += 1;
            report.memories_consolidated += 1;
        }

        // 持久化 L2
        self.short_term.persist()?;

        report.duration_ms = start.elapsed().as_millis() as u64;
        Ok(report)
    }

    /// 将一条 L2 记忆巩固为 L1 事件
    fn consolidate_to_event(&mut self, entry: &MemoryEntry) -> Result<()> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let event = EventEntry {
            id: format!("ev-{}", entry.id),
            date: today,
            summary: entry.content.clone(),
            key_actions: extract_key_actions(&entry.content),
            tags: entry.tags.clone(),
            importance: entry.importance,
            created_at: Utc::now(),
        };
        self.event_index.store(&event)?;

        // 标记 L2 已巩固
        let entry_id = entry.id.clone();
        self.short_term.mark_consolidated(&entry_id);

        Ok(())
    }

    /// 将一条高重要性 L2 记忆巩固为 L0 任务总结
    fn consolidate_to_task_summary(&self, entry: &MemoryEntry) -> Result<()> {
        let summary = TaskSummary {
            id: format!("task-{}", entry.id),
            task_description: entry.content.clone(),
            trigger_pattern: entry.tags.join(","),
            reasoning_path: vec![entry.content.clone()],
            mistakes: Vec::new(),
            files_modified: Vec::new(),
            tools_used: Vec::new(),
            success_rate: entry.confidence,
            shortcuts: Vec::new(),
            created_at: Utc::now(),
        };
        self.task_summary.store(&summary)?;
        Ok(())
    }
}

/// 从内容中提取关键动作（简化版，基于规则）
fn extract_key_actions(content: &str) -> Vec<String> {
    let keywords = [
        "完成了",
        "修复了",
        "创建了",
        "删除了",
        "实现了",
        "优化了",
        "重写了",
    ];
    content
        .split(&['。', '，', '；', '！', '.'][..])
        .filter(|s| keywords.iter().any(|kw| s.contains(kw)))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::{KnowledgeSource, MemoryLayer};
    use tempfile::TempDir;

    use crate::short_term::ShortTermConfig;
    use crate::storage::Storage;

    fn make_engine() -> (TempDir, ConsolidationEngine) {
        let tmp = TempDir::new().unwrap();
        let s1 = Storage::new(tmp.path().to_path_buf()).unwrap();
        let s2 = Storage::new(tmp.path().to_path_buf()).unwrap();
        let s3 = Storage::new(tmp.path().to_path_buf()).unwrap();
        let s4 = Storage::new(tmp.path().to_path_buf()).unwrap();

        let engine = ConsolidationEngine::new(
            RawLayer::new(s1),
            ShortTermLayer::new(s2, ShortTermConfig::default()),
            EventIndexLayer::new(s3),
            TaskSummaryLayer::new(s4),
        );
        (tmp, engine)
    }

    fn make_entry(id: &str, content: &str, importance: f64) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            content: content.into(),
            tags: vec!["test".into()],
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
    fn consolidation_creates_events() {
        let (_tmp, mut engine) = make_engine();
        engine
            .short_term
            .store(make_entry("e1", "完成了记忆脑开发", 0.7));

        let report = engine.run().unwrap();
        assert_eq!(report.event_indexes_created, 1);
        assert_eq!(report.memories_consolidated, 1);
    }

    #[test]
    fn consolidation_creates_task_summary_for_high_importance() {
        let (_tmp, mut engine) = make_engine();
        engine
            .short_term
            .store(make_entry("e1", "实现了完整的记忆系统", 0.9));

        let report = engine.run().unwrap();
        assert_eq!(report.event_indexes_created, 1);
        assert_eq!(report.task_summaries_created, 1);
        assert_eq!(report.memories_consolidated, 2); // L1 + L0
    }

    #[test]
    fn extract_key_actions_from_content() {
        let actions = extract_key_actions("完成了Phase 3开发。修复了bug。这是普通文本");
        assert_eq!(actions.len(), 2);
        assert!(actions[0].contains("完成了"));
        assert!(actions[1].contains("修复了"));
    }
}
