//! L3 经验抽象层（金字塔版）
//!
//! 按任务类型存储经验，每个类型一个文件。全量重生成模式。
//! 路径: `personas/{persona_id}/pyramid/l3-abstract/`

use crate::error::Result;
use crate::pyramid_storage::PyramidStorage;
use crate::pyramid_types::{
    AbstractIndex, AbstractIndexEntry, Experience, TaskType, TypeExperience,
};
use crate::summary_pool::task_type_name;
use chrono::Utc;

/// L3 经验抽象层
pub struct AbstractLayer {
    storage: PyramidStorage,
}

impl AbstractLayer {
    /// 创建 AbstractLayer
    pub fn new(storage: PyramidStorage) -> Self {
        Self { storage }
    }

    /// 全量重生成：覆盖所有 L3 数据
    pub fn regenerate(&self, type_experiences: Vec<TypeExperience>) -> Result<()> {
        // 构建索引
        let index = AbstractIndex {
            entries: type_experiences
                .iter()
                .map(|te| AbstractIndexEntry {
                    task_type: te.task_type.clone(),
                    experience_count: te.experiences.len(),
                    injectable_count: te.experiences.iter().filter(|e| e.injectable).count(),
                    l2_task_count: te.l2_refs.len(),
                })
                .collect(),
            updated_at: Utc::now(),
        };

        // 清空旧文件
        self.storage.clean_dir(&self.storage.l3_dir())?;

        // 写索引
        self.storage
            .write_json(&self.storage.l3_index_path(), &index)?;

        // 写各类型文件
        for te in &type_experiences {
            let name = task_type_name(&te.task_type);
            let path = self.storage.l3_type_path(name);
            self.storage.write_json(&path, te)?;
        }

        Ok(())
    }

    /// 加载指定类型的经验
    pub fn load_type(&self, task_type: &TaskType) -> Result<Option<TypeExperience>> {
        let name = task_type_name(task_type);
        let path = self.storage.l3_type_path(name);
        self.storage.read_json_optional(&path)
    }

    /// 加载所有类型的经验
    pub fn load_all(&self) -> Result<Vec<TypeExperience>> {
        let files = self.storage.list_json_files(&self.storage.l3_dir())?;
        let mut all = Vec::new();
        for path in files {
            let te: TypeExperience = self.storage.read_json(&path)?;
            all.push(te);
        }
        Ok(all)
    }

    /// 加载索引
    pub fn load_index(&self) -> Result<AbstractIndex> {
        let path = self.storage.l3_index_path();
        match self.storage.read_json_optional(&path)? {
            Some(idx) => Ok(idx),
            None => Ok(AbstractIndex {
                entries: Vec::new(),
                updated_at: Utc::now(),
            }),
        }
    }

    /// 加载所有标记为 injectable=true 的经验
    pub fn load_injectable(&self) -> Result<Vec<Experience>> {
        let all = self.load_all()?;
        let mut injectable = Vec::new();
        for te in all {
            for exp in te.experiences {
                if exp.injectable {
                    injectable.push(exp);
                }
            }
        }
        Ok(injectable)
    }

    /// 按关键词在 L3 索引中查找匹配的类型
    pub fn find_by_keyword(&self, keyword: &str) -> Result<Vec<TypeExperience>> {
        let all = self.load_all()?;
        let keyword_lower = keyword.to_lowercase();
        let results: Vec<TypeExperience> = all
            .into_iter()
            .filter(|te| {
                // 检查关键词索引
                te.index.iter().any(|ki| ki.keyword.to_lowercase() == keyword_lower)
                    // 或检查经验描述
                    || te.experiences.iter().any(|e| {
                        e.pattern.to_lowercase().contains(&keyword_lower)
                            || e.description.to_lowercase().contains(&keyword_lower)
                    })
            })
            .collect();
        Ok(results)
    }

    /// 获取底层 PyramidStorage 引用
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid_types::KeywordIndex;

    fn make_layer(persona_id: &str) -> (AbstractLayer, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), persona_id);
        storage.ensure_dirs().unwrap();
        (AbstractLayer::new(storage), tmp)
    }

    fn make_type_experience(task_type: TaskType, patterns: Vec<(&str, bool)>) -> TypeExperience {
        TypeExperience {
            task_type: task_type.clone(),
            experiences: patterns
                .into_iter()
                .enumerate()
                .map(|(i, (pattern, injectable))| Experience {
                    pattern: pattern.to_string(),
                    description: format!("描述: {pattern}"),
                    source_tasks: vec![format!("task-{}", i)],
                    frequency: (i + 1) as u32,
                    injectable,
                })
                .collect(),
            l2_refs: vec!["task-0".into()],
            index: vec![KeywordIndex {
                keyword: "测试".into(),
                l2_task_ids: vec!["task-0".into()],
            }],
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn regenerate_overwrites_old_data() {
        let (layer, _tmp) = make_layer("test");

        // 第一次写入
        layer
            .regenerate(vec![make_type_experience(
                TaskType::Coding,
                vec![("模式A", false)],
            )])
            .unwrap();
        let loaded = layer.load_all().unwrap();
        assert_eq!(loaded.len(), 1);

        // 第二次覆盖
        layer
            .regenerate(vec![
                make_type_experience(TaskType::Coding, vec![("模式B", true)]),
                make_type_experience(TaskType::Writing, vec![("模式C", false)]),
            ])
            .unwrap();
        let loaded = layer.load_all().unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn load_type_specific() {
        let (layer, _tmp) = make_layer("test");
        layer
            .regenerate(vec![
                make_type_experience(TaskType::Coding, vec![("编码模式", true)]),
                make_type_experience(TaskType::Writing, vec![("写作模式", false)]),
            ])
            .unwrap();

        let coding = layer.load_type(&TaskType::Coding).unwrap();
        assert!(coding.is_some());
        assert_eq!(coding.unwrap().experiences[0].pattern, "编码模式");

        let writing = layer.load_type(&TaskType::Writing).unwrap();
        assert!(writing.is_some());
        assert_eq!(writing.unwrap().experiences[0].pattern, "写作模式");
    }

    #[test]
    fn load_injectable_experiences() {
        let (layer, _tmp) = make_layer("test");
        layer
            .regenerate(vec![
                make_type_experience(
                    TaskType::Coding,
                    vec![("可注入", true), ("不可注入", false)],
                ),
                make_type_experience(TaskType::Writing, vec![("也可注入", true)]),
            ])
            .unwrap();

        let injectable = layer.load_injectable().unwrap();
        assert_eq!(injectable.len(), 2);
        assert!(injectable.iter().all(|e| e.injectable));
    }

    #[test]
    fn find_by_keyword() {
        let (layer, _tmp) = make_layer("test");
        layer
            .regenerate(vec![make_type_experience(
                TaskType::Coding,
                vec![("TUI修复", false)],
            )])
            .unwrap();

        // 关键词索引中包含"测试"
        let results = layer.find_by_keyword("测试").unwrap();
        assert_eq!(results.len(), 1);

        // 经验描述中包含
        let results = layer.find_by_keyword("TUI").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn index_auto_generated() {
        let (layer, _tmp) = make_layer("test");
        layer
            .regenerate(vec![
                make_type_experience(TaskType::Coding, vec![("A", true), ("B", false)]),
                make_type_experience(TaskType::Writing, vec![("C", true)]),
            ])
            .unwrap();

        let index = layer.load_index().unwrap();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].experience_count, 2);
        assert_eq!(index.entries[0].injectable_count, 1);
        assert_eq!(index.entries[1].injectable_count, 1);
    }

    #[test]
    fn load_index_empty() {
        let (layer, _tmp) = make_layer("test");
        let index = layer.load_index().unwrap();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn load_type_nonexistent_returns_none() {
        let (layer, _tmp) = make_layer("test");
        let result = layer.load_type(&TaskType::Research).unwrap();
        assert!(result.is_none());
    }
}
