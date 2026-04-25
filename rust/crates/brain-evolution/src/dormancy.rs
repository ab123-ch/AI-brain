use std::path::{Path, PathBuf};

use brain_core::types::{BrainId, Weight};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{EvolutionError, Result};
use crate::template::BrainTemplate;

/// 休眠副脑的持久化记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DormantRecord {
    pub template: BrainTemplate,
    pub dormant_since: DateTime<Utc>,
    pub original_weight: f64,
    pub task_count: u32,
}

/// 硬休眠管理器
///
/// 负责副脑的休眠判定、持久化存储和唤醒加载。
/// "硬休眠"指完全释放副脑对象和消息通道资源，
/// 仅将配置信息持久化到磁盘，唤醒时重新初始化。
/// 硬休眠管理器
///
/// 负责副脑的休眠判定、持久化存储和唤醒加载。
/// "硬休眠"指完全释放副脑对象和消息通道资源，
/// 仅将配置信息持久化到磁盘，唤醒时重新初始化。
#[must_use]
pub struct DormancyManager {
    /// 持久化目录 ~/.ai-brain/brains/
    storage_dir: PathBuf,
    /// 休眠阈值：权重 <= 此值触发休眠
    dormancy_threshold: f64,
    /// 唤醒奖励：唤醒后权重 = original_weight + 此值
    wake_bonus: f64,
}

impl DormancyManager {
    /// 创建休眠管理器
    pub fn new(storage_dir: PathBuf) -> Self {
        Self {
            storage_dir,
            dormancy_threshold: 0.2,
            wake_bonus: 0.1,
        }
    }

    /// 使用自定义阈值
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.dormancy_threshold = threshold;
        self
    }

    /// 使用自定义唤醒奖励
    pub fn with_wake_bonus(mut self, bonus: f64) -> Self {
        self.wake_bonus = bonus;
        self
    }

    /// 判断是否应该进入休眠
    pub fn should_dormant(&self, weight: &Weight) -> bool {
        weight.value() <= self.dormancy_threshold
    }

    /// 计算唤醒后的初始权重
    pub fn wake_weight(&self, original_weight: f64) -> Weight {
        let new_weight = (original_weight + self.wake_bonus).clamp(0.1, 1.0);
        Weight(new_weight)
    }

    /// 持久化休眠记录到磁盘
    pub fn persist(&self, record: &DormantRecord) -> Result<()> {
        self.ensure_storage_dir()?;
        let path = self.brain_path(&record.template.brain_id());
        let content = serde_json::to_string_pretty(record)
            .map_err(|e| EvolutionError::PersistenceFailed(e.to_string()))?;
        std::fs::write(&path, content)?;
        tracing::info!(
            "副脑 {} 已持久化到 {}",
            record.template.name,
            path.display()
        );
        Ok(())
    }

    /// 从磁盘加载休眠记录
    pub fn load(&self, id: &BrainId) -> Result<DormantRecord> {
        let path = self.brain_path(id);
        if !path.exists() {
            return Err(EvolutionError::LoadFailed(format!(
                "休眠记录不存在: {}",
                path.display()
            )));
        }
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).map_err(|e| EvolutionError::LoadFailed(e.to_string()))
    }

    /// 列出所有休眠副脑
    pub fn list_dormant(&self) -> Vec<DormantRecord> {
        if !self.storage_dir.exists() {
            return Vec::new();
        }
        let mut records = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.storage_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<DormantRecord>(&content) {
                            records.push(record);
                        }
                    }
                }
            }
        }
        records
    }

    /// 删除休眠记录（唤醒成功后）
    pub fn remove(&self, id: &BrainId) -> Result<()> {
        let path = self.brain_path(id);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// 检查某个副脑是否有休眠记录
    pub fn has_dormant_record(&self, id: &BrainId) -> bool {
        self.brain_path(id).exists()
    }

    /// 获取休眠阈值
    pub fn dormancy_threshold(&self) -> f64 {
        self.dormancy_threshold
    }

    /// 获取存储目录
    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    fn ensure_storage_dir(&self) -> Result<()> {
        if !self.storage_dir.exists() {
            std::fs::create_dir_all(&self.storage_dir)?;
        }
        Ok(())
    }

    fn brain_path(&self, id: &BrainId) -> PathBuf {
        self.storage_dir.join(format!("{}.json", id.0))
    }
}

/// 创建默认存储路径
pub fn default_storage_dir() -> PathBuf {
    dirs_home().join(".ai-brain").join("brains")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_template(name: &str) -> BrainTemplate {
        BrainTemplate::new(name, format!("{name} 副脑"))
    }

    #[test]
    fn test_should_dormant() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DormancyManager::new(dir.path().to_path_buf());
        assert!(mgr.should_dormant(&Weight(0.1)));
        assert!(mgr.should_dormant(&Weight(0.2)));
        assert!(!mgr.should_dormant(&Weight(0.21)));
        assert!(!mgr.should_dormant(&Weight(0.5)));
    }

    #[test]
    fn test_wake_weight() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DormancyManager::new(dir.path().to_path_buf());
        let w = mgr.wake_weight(0.2);
        // 0.2 + 0.1 = 0.3
        assert!((w.value() - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_wake_weight_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DormancyManager::new(dir.path().to_path_buf());
        let w = mgr.wake_weight(0.95);
        // 0.95 + 0.1 = 1.05 → clamped to 1.0
        assert!((w.value() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_persist_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DormancyManager::new(dir.path().to_path_buf());

        let record = DormantRecord {
            template: make_template("test-brain"),
            dormant_since: Utc::now(),
            original_weight: 0.15,
            task_count: 5,
        };

        mgr.persist(&record).unwrap();

        let loaded = mgr.load(&BrainId("test-brain".into())).unwrap();
        assert_eq!(loaded.template.name, "test-brain");
        assert!((loaded.original_weight - 0.15).abs() < f64::EPSILON);
        assert_eq!(loaded.task_count, 5);
    }

    #[test]
    fn test_list_dormant() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DormancyManager::new(dir.path().to_path_buf());

        mgr.persist(&DormantRecord {
            template: make_template("brain-a"),
            dormant_since: Utc::now(),
            original_weight: 0.1,
            task_count: 0,
        })
        .unwrap();

        mgr.persist(&DormantRecord {
            template: make_template("brain-b"),
            dormant_since: Utc::now(),
            original_weight: 0.2,
            task_count: 3,
        })
        .unwrap();

        let list = mgr.list_dormant();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_remove_dormant() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DormancyManager::new(dir.path().to_path_buf());

        let id = BrainId("temp-brain".into());
        mgr.persist(&DormantRecord {
            template: make_template("temp-brain"),
            dormant_since: Utc::now(),
            original_weight: 0.1,
            task_count: 0,
        })
        .unwrap();

        assert!(mgr.has_dormant_record(&id));
        mgr.remove(&id).unwrap();
        assert!(!mgr.has_dormant_record(&id));
    }

    #[test]
    fn test_load_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = DormancyManager::new(dir.path().to_path_buf());
        assert!(mgr.load(&BrainId("nonexistent".into())).is_err());
    }
}
