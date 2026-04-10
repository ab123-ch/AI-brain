use brain_core::types::{MemoryEntry, MemoryLayer, RecallQuery};

use crate::error::Result;
use crate::event_index::EventIndexLayer;
use crate::raw_layer::RawLayer;
use crate::short_term::ShortTermLayer;
use crate::task_summary::TaskSummaryLayer;

/// 多层召回策略
///
/// 读取路径：L0 经验匹配 → L1 事件匹配 → L2 关键词匹配 → L3 原始回溯
/// 快思考：纯关键词/模式匹配（不调 LLM，~10ms）
/// importance 控制召回行为（不删数据）：
///   - importance > 0.5 → 正常召回
///   - 0.2 < importance <= 0.5 → 需要强关键词线索才召回
///   - importance <= 0.2 → 不主动召回（但数据仍在 L3）
pub struct RecallEngine {
    task_summary: TaskSummaryLayer,
    event_index: EventIndexLayer,
    short_term: ShortTermLayer,
    raw: RawLayer,
}

impl RecallEngine {
    pub fn new(
        task_summary: TaskSummaryLayer,
        event_index: EventIndexLayer,
        short_term: ShortTermLayer,
        raw: RawLayer,
    ) -> Self {
        Self {
            task_summary,
            event_index,
            short_term,
            raw,
        }
    }

    /// 执行多层召回
    ///
    /// 按 L0 → L1 → L2 → L3 的优先级搜索，
    /// 每层结果追加到总结果中，直到达到 max_results。
    pub fn recall(&self, query: &RecallQuery) -> Result<Vec<MemoryEntry>> {
        let mut results = Vec::new();
        let limit = query.max_results;

        // L0: 任务总结
        if query.layers.contains(&MemoryLayer::TaskSummary) {
            let l0 = self.task_summary.search(&query.keywords, limit)?;
            results.extend(l0);
        }

        if results.len() >= limit {
            return Ok(truncate(results, limit));
        }

        // L1: 事件索引
        if query.layers.contains(&MemoryLayer::EventIndex) {
            let remaining = limit - results.len();
            let l1 = self.event_index.search(&query.keywords, remaining)?;
            // 去重
            append_dedup(&mut results, l1);
        }

        if results.len() >= limit {
            return Ok(truncate(results, limit));
        }

        // L2: 短期记忆
        if query.layers.contains(&MemoryLayer::ShortTerm) {
            let remaining = limit - results.len();
            let l2 = self
                .short_term
                .search(&query.keywords, query.min_importance, remaining);
            append_dedup(&mut results, l2);
        }

        if results.len() >= limit {
            return Ok(truncate(results, limit));
        }

        // L3: 原始记忆（仅在其他层未命中时）
        if query.layers.contains(&MemoryLayer::Raw) {
            let remaining = limit - results.len();
            let l3 = self.raw.search(&query.keywords, remaining)?;
            append_dedup(&mut results, l3);
        }

        Ok(results)
    }

    /// 快速关键词检查（用于 fast_think）
    ///
    /// 只搜 L0 和 L2 的 tag 索引，延迟 ~10ms
    pub fn quick_check(&self, keywords: &[String], min_importance: f64) -> Vec<MemoryEntry> {
        let mut results = Vec::new();

        // L2 tag 索引（最快）
        let l2 = self.short_term.search(keywords, min_importance, 5);
        results.extend(l2);

        // L0 任务总结（文件搜索，较慢但条目少）
        if let Ok(l0) = self.task_summary.search(keywords, 3) {
            append_dedup(&mut results, l0);
        }

        results
    }
}

fn truncate(mut v: Vec<MemoryEntry>, limit: usize) -> Vec<MemoryEntry> {
    v.truncate(limit);
    v
}

fn append_dedup(results: &mut Vec<MemoryEntry>, new: Vec<MemoryEntry>) {
    for entry in new {
        if !results.iter().any(|r| r.id == entry.id) {
            results.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::short_term::ShortTermConfig;
    use tempfile::TempDir;

    struct TestEnv {
        _tmp: TempDir,
        recall: RecallEngine,
    }

    impl TestEnv {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let base = tmp.path().to_path_buf();

            let recall = RecallEngine::new(
                TaskSummaryLayer::new(crate::storage::Storage::new_lazy(base.clone())),
                EventIndexLayer::new(crate::storage::Storage::new_lazy(base.clone())),
                ShortTermLayer::new(
                    crate::storage::Storage::new_lazy(base.clone()),
                    ShortTermConfig::default(),
                ),
                RawLayer::new(crate::storage::Storage::new_lazy(base)),
            );

            Self { _tmp: tmp, recall }
        }
    }

    #[test]
    fn recall_returns_empty_when_nothing_stored() {
        let env = TestEnv::new();
        let query = RecallQuery::default();
        let results = env.recall.recall(&query).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn quick_check_returns_empty_when_nothing_stored() {
        let env = TestEnv::new();
        let results = env.recall.quick_check(&["test".into()], 0.0);
        assert!(results.is_empty());
    }

    #[test]
    fn recall_respects_layer_filter() {
        let env = TestEnv::new();
        let query = RecallQuery {
            layers: vec![MemoryLayer::TaskSummary], // 只搜 L0
            ..RecallQuery::default()
        };
        let results = env.recall.recall(&query).unwrap();
        assert!(results.is_empty()); // 没存任何 L0 数据
    }
}
