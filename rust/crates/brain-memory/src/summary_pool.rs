//! L2 记忆摘要池（金字塔版）
//!
//! 按任务分类的摘要，不是按会话。全量重生成模式。
//! 路径: `personas/{persona_id}/pyramid/l2-summary/`

use chrono::Utc;
use crate::error::Result;
use crate::pyramid_storage::PyramidStorage;
use crate::pyramid_types::{SummaryIndex, SummaryIndexEntry, TaskSummary};

/// L2 记忆摘要池
pub struct SummaryPool {
    storage: PyramidStorage,
}

impl SummaryPool {
    /// 创建 SummaryPool
    pub fn new(storage: PyramidStorage) -> Self {
        Self { storage }
    }

    /// 全量重生成：覆盖所有 L2 数据
    pub fn regenerate(&self, tasks: Vec<TaskSummary>) -> Result<()> {
        let index = SummaryIndex {
            entries: tasks
                .iter()
                .map(|t| SummaryIndexEntry {
                    task_id: t.task_id.clone(),
                    task_type: t.task_type.clone(),
                    task_name: t.task_name.clone(),
                    tags: t.tags.clone(),
                    importance: t.importance,
                })
                .collect(),
            updated_at: Utc::now(),
        };

        // 清空旧文件再写入
        self.storage.clean_dir(&self.storage.l2_dir())?;

        // 写索引
        self.storage
            .write_json(&self.storage.l2_index_path(), &index)?;

        // 写各任务文件
        for task in &tasks {
            let path = self.storage.l2_task_path(&task.task_id);
            self.storage.write_json(&path, task)?;
        }

        Ok(())
    }

    /// 加载所有任务摘要
    pub fn load_all(&self) -> Result<Vec<TaskSummary>> {
        let files = self.storage.list_json_files(&self.storage.l2_dir())?;
        let mut tasks = Vec::new();
        for path in files {
            let task: TaskSummary = self.storage.read_json(&path)?;
            tasks.push(task);
        }
        // 按 importance 降序排序
        tasks.sort_by(|a, b| b.importance.partial_cmp(&a.importance).unwrap_or(std::cmp::Ordering::Equal));
        Ok(tasks)
    }

    /// 加载索引
    pub fn load_index(&self) -> Result<SummaryIndex> {
        let path = self.storage.l2_index_path();
        match self.storage.read_json_optional(&path)? {
            Some(idx) => Ok(idx),
            None => Ok(SummaryIndex {
                entries: Vec::new(),
                updated_at: Utc::now(),
            }),
        }
    }

    /// 按 tags 查找任务
    pub fn find_by_tags(&self, tags: &[String]) -> Result<Vec<TaskSummary>> {
        let all = self.load_all()?;
        let results: Vec<TaskSummary> = all
            .into_iter()
            .filter(|t| {
                tags.iter()
                    .any(|tag| t.tags.iter().any(|t_tag| t_tag.to_lowercase() == tag.to_lowercase()))
            })
            .collect();
        Ok(results)
    }

    /// 按任务 ID 查找
    pub fn find_by_id(&self, task_id: &str) -> Result<Option<TaskSummary>> {
        let path = self.storage.l2_task_path(task_id);
        self.storage.read_json_optional(&path)
    }

    /// 按任务类型查找
    pub fn find_by_type(&self, type_name: &str) -> Result<Vec<TaskSummary>> {
        let all = self.load_all()?;
        let results: Vec<TaskSummary> = all
            .into_iter()
            .filter(|t| task_type_name(&t.task_type) == type_name)
            .collect();
        Ok(results)
    }

    /// 获取底层 PyramidStorage 引用
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }
}

/// 将 TaskType 转为可读名称（用于文件名）
pub fn task_type_name(t: &crate::pyramid_types::TaskType) -> &'static str {
    match t {
        crate::pyramid_types::TaskType::Coding => "coding",
        crate::pyramid_types::TaskType::Writing => "writing",
        crate::pyramid_types::TaskType::Troubleshooting => "troubleshooting",
        crate::pyramid_types::TaskType::Research => "research",
        crate::pyramid_types::TaskType::Multimedia => "multimedia",
        crate::pyramid_types::TaskType::Configuration => "configuration",
        crate::pyramid_types::TaskType::Other(_) => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid_types::{L1Ref, TaskType};

    fn make_pool(persona_id: &str) -> (SummaryPool, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), persona_id);
        storage.ensure_dirs().unwrap();
        (SummaryPool::new(storage), tmp)
    }

    fn make_task(id: &str, name: &str, task_type: TaskType, importance: f64) -> TaskSummary {
        TaskSummary {
            task_id: id.to_string(),
            task_type,
            task_name: name.to_string(),
            summary: format!("摘要: {name}"),
            l1_refs: vec![L1Ref {
                session: "sess-001".into(),
                paragraphs: vec![0],
            }],
            tags: vec!["test".into()],
            importance,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn regenerate_overwrites_old_data() {
        let (pool, _tmp) = make_pool("test");

        // 第一次写入
        pool.regenerate(vec![make_task("t-1", "任务1", TaskType::Coding, 0.8)])
            .unwrap();
        let loaded = pool.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].task_id, "t-1");

        // 第二次全量覆盖
        pool.regenerate(vec![
            make_task("t-2", "任务2", TaskType::Coding, 0.9),
            make_task("t-3", "任务3", TaskType::Writing, 0.5),
        ])
        .unwrap();
        let loaded = pool.load_all().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|t| t.task_id != "t-1"));
    }

    #[test]
    fn index_auto_generated() {
        let (pool, _tmp) = make_pool("test");
        pool.regenerate(vec![
            make_task("t-1", "A", TaskType::Coding, 0.9),
            make_task("t-2", "B", TaskType::Writing, 0.5),
        ])
        .unwrap();

        let index = pool.load_index().unwrap();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].task_id, "t-1");
        assert_eq!(index.entries[1].task_type, TaskType::Writing);
    }

    #[test]
    fn find_by_tags() {
        let (pool, _tmp) = make_pool("test");
        let mut task = make_task("t-1", "TUI修复", TaskType::Coding, 0.8);
        task.tags = vec!["TUI".into(), "鼠标".into()];
        pool.regenerate(vec![task]).unwrap();

        let results = pool.find_by_tags(&["TUI".into()]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "t-1");

        let miss = pool.find_by_tags(&["网络".into()]).unwrap();
        assert!(miss.is_empty());
    }

    #[test]
    fn find_by_id() {
        let (pool, _tmp) = make_pool("test");
        pool.regenerate(vec![make_task("t-1", "任务A", TaskType::Coding, 0.8)])
            .unwrap();

        let found = pool.find_by_id("t-1").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().task_name, "任务A");

        let missing = pool.find_by_id("t-999").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn sorted_by_importance() {
        let (pool, _tmp) = make_pool("test");
        pool.regenerate(vec![
            make_task("t-low", "低", TaskType::Coding, 0.3),
            make_task("t-high", "高", TaskType::Coding, 0.9),
            make_task("t-mid", "中", TaskType::Coding, 0.6),
        ])
        .unwrap();

        let loaded = pool.load_all().unwrap();
        assert_eq!(loaded[0].task_id, "t-high");
        assert_eq!(loaded[1].task_id, "t-mid");
        assert_eq!(loaded[2].task_id, "t-low");
    }

    #[test]
    fn load_index_empty() {
        let (pool, _tmp) = make_pool("test");
        let index = pool.load_index().unwrap();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn find_by_type() {
        let (pool, _tmp) = make_pool("test");
        pool.regenerate(vec![
            make_task("t-1", "编码", TaskType::Coding, 0.8),
            make_task("t-2", "写作", TaskType::Writing, 0.5),
            make_task("t-3", "更多编码", TaskType::Coding, 0.7),
        ])
        .unwrap();

        let coding = pool.find_by_type("coding").unwrap();
        assert_eq!(coding.len(), 2);

        let writing = pool.find_by_type("writing").unwrap();
        assert_eq!(writing.len(), 1);
    }
}
