//! 用户评估要求存储（动态积累）
//!
//! 职责：
//! - 持久化用户对评估脑的评估要求到 `memory/eval-requirement/` 目录
//! - 支持从用户反馈中积累评估要求
//! - 提供 CRUD 和查询接口

use chrono::Utc;

use brain_core::types::EvalRequirement;

use crate::error::Result;
use crate::storage::Storage;

/// 用户评估要求存储管理器
pub struct EvalRequirementStore {
    storage: Storage,
}

impl EvalRequirementStore {
    /// 创建评估要求存储管理器
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 获取存储目录
    fn requirement_dir(&self) -> std::path::PathBuf {
        self.storage
            .base_dir()
            .join("memory")
            .join("eval-requirement")
    }

    /// 存储一条评估要求（原子写入）
    pub fn store(&self, record: &EvalRequirement) -> Result<()> {
        let dir = self.requirement_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", record.id));
        self.storage.write_json_atomic(&path, &record)
    }

    /// 加载所有评估要求
    pub fn load_all(&self) -> Result<Vec<EvalRequirement>> {
        let dir = self.requirement_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let files = self.storage.list_json_files(&dir)?;
        let mut records = Vec::new();
        for file in &files {
            if let Ok(record) = self.storage.read_json::<EvalRequirement>(file) {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// 获取活跃评估要求（未 superseded）
    pub fn load_active(&self) -> Result<Vec<EvalRequirement>> {
        let all = self.load_all()?;
        Ok(all.into_iter().filter(|r| !r.superseded).collect())
    }

    /// 标记为已废弃
    pub fn mark_superseded(&self, id: &str) -> Result<()> {
        let all = self.load_all()?;
        for mut record in all {
            if record.id == id {
                record.superseded = true;
                self.store(&record)?;
                return Ok(());
            }
        }
        Ok(())
    }

    /// 添加新的评估要求
    ///
    /// 如果内容完全相同则跳过（不重复创建）
    pub fn add(&self, content: &str, source: &str) -> Result<EvalRequirement> {
        let existing = self.load_active()?;
        if existing.iter().any(|r| r.content.trim() == content.trim()) {
            // 已存在相同内容，返回已有记录
            return Ok(existing
                .into_iter()
                .find(|r| r.content.trim() == content.trim())
                .unwrap());
        }

        let record = EvalRequirement {
            id: format!("evreq-{}", Utc::now().timestamp_millis()),
            content: content.to_string(),
            source: source.to_string(),
            created_at: Utc::now(),
            superseded: false,
        };
        self.store(&record)?;
        Ok(record)
    }

    /// 统计总记录数
    pub fn count(&self) -> Result<u32> {
        Ok(self.load_all()?.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, EvalRequirementStore) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        (tmp, EvalRequirementStore::new(storage))
    }

    #[test]
    fn add_and_load() {
        let (_tmp, store) = make_store();
        let req = store.add("不要将简单问答判定为问题", "用户反馈").unwrap();

        assert!(req.id.starts_with("evreq-"));
        assert_eq!(req.content, "不要将简单问答判定为问题");

        let active = store.load_active().unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn add_deduplicates_same_content() {
        let (_tmp, store) = make_store();
        store.add("关注代码安全性", "用户反馈").unwrap();
        store.add("关注代码安全性", "记忆脑分析").unwrap();

        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn mark_superseded() {
        let (_tmp, store) = make_store();
        let req = store.add("测试要求", "测试").unwrap();
        store.mark_superseded(&req.id).unwrap();

        let active = store.load_active().unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn load_all_includes_superseded() {
        let (_tmp, store) = make_store();
        let req = store.add("测试要求", "测试").unwrap();
        store.mark_superseded(&req.id).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
    }
}
