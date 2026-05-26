//! L4 潜意识层（金字塔版）
//!
//! 覆盖式更新：触发词上限 50 个，叙事文本 500 字以内。
//! 路径: `personas/{persona_id}/pyramid/l4-subconscious.json`

use crate::error::Result;
use crate::pyramid_storage::PyramidStorage;
use crate::pyramid_types::{SubconsciousData, SubconsciousTrigger};

/// 叙事文本最大长度（字）
const MAX_NARRATIVE_CHARS: usize = 500;
/// 触发词上限
const MAX_TRIGGERS: usize = 50;

/// L4 潜意识层
pub struct SubconsciousPool {
    storage: PyramidStorage,
}

impl SubconsciousPool {
    /// 创建 SubconsciousPool
    pub fn new(storage: PyramidStorage) -> Self {
        Self { storage }
    }

    /// 加载潜意识数据
    pub fn load(&self) -> Result<Option<SubconsciousData>> {
        let path = self.storage.l4_path();
        self.storage.read_json_optional(&path)
    }

    /// 全量覆盖重写潜意识数据
    pub fn regenerate(&self, data: &SubconsciousData) -> Result<()> {
        let mut data = data.clone();

        // 截断保护
        if data.narrative.chars().count() > MAX_NARRATIVE_CHARS {
            data.narrative = data.narrative.chars().take(MAX_NARRATIVE_CHARS).collect();
        }
        if data.triggers.len() > MAX_TRIGGERS {
            data.triggers.truncate(MAX_TRIGGERS);
        }

        self.storage.write_json(&self.storage.l4_path(), &data)
    }

    /// 触发词匹配 — 返回命中的触发词列表
    pub fn match_triggers(&self, query: &str) -> Result<Vec<SubconsciousTrigger>> {
        let Some(data) = self.load()? else {
            return Ok(Vec::new());
        };

        let query_lower = query.to_lowercase();
        let hits: Vec<SubconsciousTrigger> = data
            .triggers
            .into_iter()
            .filter(|t| query_lower.contains(&t.keyword.to_lowercase()))
            .collect();

        Ok(hits)
    }

    /// 获取叙事文本
    pub fn narrative(&self) -> Result<String> {
        let data = self.load()?;
        Ok(data.map(|d| d.narrative).unwrap_or_default())
    }

    /// 生成注入上下文的文本（用于 system prompt）
    pub fn inject_text(&self) -> Result<String> {
        let Some(data) = self.load()? else {
            return Ok(String::new());
        };

        if data.narrative.is_empty() && data.triggers.is_empty() {
            return Ok(String::new());
        }

        let mut parts = Vec::new();
        if !data.narrative.is_empty() {
            parts.push(format!("[潜意识叙事] {}", data.narrative));
        }
        if !data.triggers.is_empty() {
            let keywords: Vec<&str> = data.triggers.iter().map(|t| t.keyword.as_str()).collect();
            parts.push(format!("[记忆触发词] {}", keywords.join(", ")));
        }

        Ok(parts.join("\n"))
    }

    /// 获取底层 PyramidStorage 引用
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::pyramid_types::TaskType;

    fn make_pool(persona_id: &str) -> (SubconsciousPool, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), persona_id);
        storage.ensure_dirs().unwrap();
        (SubconsciousPool::new(storage), tmp)
    }

    fn make_data(triggers: Vec<(&str, &str)>, narrative: &str) -> SubconsciousData {
        SubconsciousData {
            triggers: triggers
                .into_iter()
                .map(|(kw, task)| SubconsciousTrigger {
                    keyword: kw.to_string(),
                    l3_type: TaskType::Coding,
                    l2_task: task.to_string(),
                })
                .collect(),
            narrative: narrative.to_string(),
            version: 1,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn save_and_load() {
        let (pool, _tmp) = make_pool("test");
        let data = make_data(
            vec![("红冲逻辑", "task-033")],
            "用户是Rust开发者",
        );
        pool.regenerate(&data).unwrap();

        let loaded = pool.load().unwrap().unwrap();
        assert_eq!(loaded.narrative, "用户是Rust开发者");
        assert_eq!(loaded.triggers.len(), 1);
        assert_eq!(loaded.triggers[0].keyword, "红冲逻辑");
    }

    #[test]
    fn regenerate_overwrites() {
        let (pool, _tmp) = make_pool("test");

        let data1 = make_data(vec![("旧触发词", "task-001")], "旧叙事");
        pool.regenerate(&data1).unwrap();

        let data2 = make_data(vec![("新触发词", "task-002")], "新叙事");
        pool.regenerate(&data2).unwrap();

        let loaded = pool.load().unwrap().unwrap();
        assert_eq!(loaded.narrative, "新叙事");
        assert_eq!(loaded.triggers.len(), 1);
        assert_eq!(loaded.triggers[0].keyword, "新触发词");
    }

    #[test]
    fn match_triggers_hit() {
        let (pool, _tmp) = make_pool("test");
        let data = make_data(
            vec![
                ("红冲逻辑", "task-033"),
                ("TUI鼠标", "task-001"),
                ("消消乐", "task-021"),
            ],
            "用户是Rust开发者",
        );
        pool.regenerate(&data).unwrap();

        let hits = pool.match_triggers("我要改红冲逻辑的代码").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].keyword, "红冲逻辑");
        assert_eq!(hits[0].l2_task, "task-033");
    }

    #[test]
    fn match_triggers_multiple_hits() {
        let (pool, _tmp) = make_pool("test");
        let data = make_data(
            vec![("Rust", "task-001"), ("crossterm", "task-002")],
            "叙事",
        );
        pool.regenerate(&data).unwrap();

        let hits = pool
            .match_triggers("用Rust和crossterm做TUI")
            .unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn match_triggers_miss() {
        let (pool, _tmp) = make_pool("test");
        let data = make_data(vec![("红冲逻辑", "task-033")], "叙事");
        pool.regenerate(&data).unwrap();

        let hits = pool.match_triggers("今天天气不错").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn match_triggers_empty_data() {
        let (pool, _tmp) = make_pool("test");
        let hits = pool.match_triggers("测试").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn narrative_truncation() {
        let (pool, _tmp) = make_pool("test");
        let long_narrative: String = "很".repeat(600);
        let data = SubconsciousData {
            triggers: vec![],
            narrative: long_narrative,
            version: 1,
            updated_at: Utc::now(),
        };
        pool.regenerate(&data).unwrap();

        let loaded = pool.load().unwrap().unwrap();
        assert_eq!(loaded.narrative.chars().count(), MAX_NARRATIVE_CHARS);
    }

    #[test]
    fn triggers_truncation() {
        let (pool, _tmp) = make_pool("test");
        let triggers: Vec<SubconsciousTrigger> = (0..60)
            .map(|i| SubconsciousTrigger {
                keyword: format!("触发词{i}"),
                l3_type: TaskType::Coding,
                l2_task: format!("task-{i}"),
            })
            .collect();
        let data = SubconsciousData {
            triggers,
            narrative: "叙事".into(),
            version: 1,
            updated_at: Utc::now(),
        };
        pool.regenerate(&data).unwrap();

        let loaded = pool.load().unwrap().unwrap();
        assert_eq!(loaded.triggers.len(), MAX_TRIGGERS);
    }

    #[test]
    fn inject_text_format() {
        let (pool, _tmp) = make_pool("test");
        let data = make_data(
            vec![("红冲", "task-033"), ("TUI", "task-001")],
            "Rust开发者",
        );
        pool.regenerate(&data).unwrap();

        let inject = pool.inject_text().unwrap();
        assert!(inject.contains("[潜意识叙事]"));
        assert!(inject.contains("Rust开发者"));
        assert!(inject.contains("[记忆触发词]"));
        assert!(inject.contains("红冲"));
        assert!(inject.contains("TUI"));
    }

    #[test]
    fn inject_text_empty() {
        let (pool, _tmp) = make_pool("test");
        let inject = pool.inject_text().unwrap();
        assert!(inject.is_empty());
    }

    #[test]
    fn persona_isolation() {
        let tmp = tempfile::tempdir().unwrap();

        let storage_a = PyramidStorage::new(tmp.path().to_path_buf(), "persona-a");
        storage_a.ensure_dirs().unwrap();
        let pool_a = SubconsciousPool::new(storage_a);

        let storage_b = PyramidStorage::new(tmp.path().to_path_buf(), "persona-b");
        storage_b.ensure_dirs().unwrap();
        let pool_b = SubconsciousPool::new(storage_b);

        let data_a = make_data(vec![], "A的叙事");
        pool_a.regenerate(&data_a).unwrap();

        let data_b = make_data(vec![], "B的叙事");
        pool_b.regenerate(&data_b).unwrap();

        assert_eq!(pool_a.narrative().unwrap(), "A的叙事");
        assert_eq!(pool_b.narrative().unwrap(), "B的叙事");
    }
}
