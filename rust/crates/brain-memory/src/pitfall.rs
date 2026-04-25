//! 踩坑库存储与查询（v2 四步分析第三步产出）
//!
//! 职责：
//! - 持久化踩坑记录到 `memory/pitfall/` 目录
//! - 按类别查询踩坑记录
//! - 合并重复踩坑（递增 occurrence_count）
//! - 提供活跃踩坑记录的快照

use chrono::Utc;

use brain_core::types::{PitfallCategory, PitfallRecord};

use crate::error::Result;
use crate::storage::Storage;

/// 踩坑库存储管理器
pub struct PitfallStore {
    storage: Storage,
}

impl PitfallStore {
    /// 创建踩坑库存储管理器
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 获取存储根目录
    pub fn base_dir(&self) -> &std::path::Path {
        self.storage.base_dir()
    }

    /// 获取踩坑记录存储目录
    fn pitfall_dir(&self) -> std::path::PathBuf {
        self.storage.base_dir().join("memory").join("pitfall")
    }

    /// 存储一条踩坑记录（原子写入）
    pub fn store(&self, record: &PitfallRecord) -> Result<()> {
        let dir = self.pitfall_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", record.id));
        self.storage.write_json_atomic(&path, &record)
    }

    /// 加载所有踩坑记录
    pub fn load_all(&self) -> Result<Vec<PitfallRecord>> {
        let dir = self.pitfall_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let files = self.storage.list_json_files(&dir)?;
        let mut records = Vec::new();
        for file in &files {
            if let Ok(record) = self.storage.read_json::<PitfallRecord>(file) {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// 按类别查询踩坑记录
    pub fn load_by_category(&self, category: PitfallCategory) -> Result<Vec<PitfallRecord>> {
        let all = self.load_all()?;
        Ok(all.into_iter().filter(|r| r.category == category).collect())
    }

    /// 获取活跃踩坑记录（occurrence_count > 0 且未 superseded）
    pub fn load_active(&self) -> Result<Vec<PitfallRecord>> {
        let all = self.load_all()?;
        Ok(all
            .into_iter()
            .filter(|r| r.occurrence_count > 0 && !r.superseded)
            .collect())
    }

    /// 标记为已取代
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

    /// 合并 LLM 分析产出的新踩坑记录
    ///
    /// 策略：按 description 精确匹配去重
    /// - 匹配到：递增 occurrence_count
    /// - 未匹配：新建记录
    pub fn merge_analysis(&self, new_records: &[NewPitfall]) -> Result<Vec<PitfallRecord>> {
        let mut existing = self.load_all()?;
        let mut result = Vec::new();

        for new_pitfall in new_records {
            if let Some(record) = existing.iter_mut().find(|r| {
                r.description
                    .trim()
                    .eq_ignore_ascii_case(new_pitfall.description.trim())
            }) {
                record.occurrence_count += 1;
                if let Some(ref correction) = new_pitfall.user_correction {
                    record.user_correction = Some(correction.clone());
                }
                result.push(record.clone());
            } else {
                let record = PitfallRecord {
                    id: format!("pit-{}", Utc::now().timestamp_millis()),
                    category: new_pitfall.category,
                    description: new_pitfall.description.clone(),
                    user_correction: new_pitfall.user_correction.clone(),
                    occurred_at: Utc::now(),
                    occurrence_count: 1,
                    superseded: false,
                };
                let cloned = record.clone();
                self.store(&record)?;
                existing.push(record);
                result.push(cloned);
            }
        }

        // 保存所有更新过的记录
        for record in &existing {
            self.store(record)?;
        }

        Ok(result)
    }

    /// 统计总记录数
    pub fn count(&self) -> Result<u32> {
        Ok(self.load_all()?.len() as u32)
    }
}

/// 新踩坑记录输入（LLM 分析产出）
#[derive(Debug, Clone, serde::Serialize)]
pub struct NewPitfall {
    pub category: PitfallCategory,
    pub description: String,
    pub user_correction: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, PitfallStore) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        (tmp, PitfallStore::new(storage))
    }

    fn make_record(id: &str, category: PitfallCategory, desc: &str) -> PitfallRecord {
        PitfallRecord {
            id: id.into(),
            category,
            description: desc.into(),
            user_correction: None,
            occurred_at: Utc::now(),
            occurrence_count: 1,
            superseded: false,
        }
    }

    #[test]
    fn store_and_load_all() {
        let (_tmp, store) = make_store();
        store
            .store(&make_record(
                "p1",
                PitfallCategory::ToolFailure,
                "工具调用超时",
            ))
            .unwrap();
        store
            .store(&make_record("p2", PitfallCategory::WrongAnswer, "回答错误"))
            .unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn load_by_category() {
        let (_tmp, store) = make_store();
        store
            .store(&make_record("p1", PitfallCategory::ToolFailure, "超时"))
            .unwrap();
        store
            .store(&make_record("p2", PitfallCategory::WrongAnswer, "错误"))
            .unwrap();

        let tool_failures = store
            .load_by_category(PitfallCategory::ToolFailure)
            .unwrap();
        assert_eq!(tool_failures.len(), 1);
        assert_eq!(tool_failures[0].id, "p1");
    }

    #[test]
    fn merge_analysis_creates_new() {
        let (_tmp, store) = make_store();
        let result = store
            .merge_analysis(&[NewPitfall {
                category: PitfallCategory::LazyBehavior,
                description: "未读取完整文件".into(),
                user_correction: None,
            }])
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].occurrence_count, 1);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn merge_analysis_increments_existing() {
        let (_tmp, store) = make_store();

        // 第一次
        store
            .merge_analysis(&[NewPitfall {
                category: PitfallCategory::FormatIssue,
                description: "输出格式不正确".into(),
                user_correction: None,
            }])
            .unwrap();

        // 第二次：相同描述
        let result = store
            .merge_analysis(&[NewPitfall {
                category: PitfallCategory::FormatIssue,
                description: "输出格式不正确".into(),
                user_correction: Some("使用 JSON 格式".into()),
            }])
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].occurrence_count, 2);
        assert_eq!(result[0].user_correction, Some("使用 JSON 格式".into()));
        // 只有一条记录
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn load_active_filters_zero_count() {
        let (_tmp, store) = make_store();
        let mut record = make_record("p1", PitfallCategory::Other, "测试");
        record.occurrence_count = 0;
        store.store(&record).unwrap();

        let active = store.load_active().unwrap();
        assert!(active.is_empty());
    }
}
