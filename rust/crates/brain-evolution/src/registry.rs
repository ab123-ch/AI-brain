use std::collections::HashMap;
use std::path::PathBuf;

use brain_core::types::{BrainId, Weight};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dormancy::{DormancyManager, DormantRecord};
use crate::error::{EvolutionError, Result};
use crate::template::{BrainTemplate, builtin_templates};

/// 活跃副脑条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveBrainEntry {
    pub template: BrainTemplate,
    pub registered_at: DateTime<Utc>,
    pub task_count: u32,
}

/// 副脑注册中心
///
/// 管理所有副脑的生命周期：创建、注册、休眠、唤醒、销毁。
/// 持久化通过 DormancyManager 处理。
pub struct BrainRegistry {
    /// 活跃副脑
    active: HashMap<BrainId, ActiveBrainEntry>,
    /// 可用模板
    templates: HashMap<String, BrainTemplate>,
    /// 休眠管理器
    dormancy_mgr: DormancyManager,
}

impl Default for BrainRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BrainRegistry {
    /// 创建注册中心（使用默认存储路径和内置模板）
    pub fn new() -> Self {
        Self::with_storage_dir(crate::dormancy::default_storage_dir())
    }

    /// 使用指定存储目录
    pub fn with_storage_dir(storage_dir: PathBuf) -> Self {
        let dormancy_mgr = DormancyManager::new(storage_dir);
        let mut templates = HashMap::new();

        // 加载内置模板
        for t in builtin_templates() {
            templates.insert(t.name.clone(), t);
        }

        // 从磁盘加载休眠副脑
        let active = HashMap::new();
        for record in dormancy_mgr.list_dormant() {
            tracing::info!("加载休眠副脑: {}", record.template.name);
            // 休眠副脑不放入 active，只记录到 dormant（通过 dormancy_mgr 管理）
            // 但也注册其模板，以便唤醒时使用
            templates.insert(record.template.name.clone(), record.template.clone());
        }

        Self {
            active,
            templates,
            dormancy_mgr,
        }
    }

    /// 注册一个新模板
    pub fn register_template(&mut self, template: BrainTemplate) -> Result<()> {
        if self.templates.contains_key(&template.name) {
            return Err(EvolutionError::TemplateNameConflict(template.name.clone()));
        }
        self.templates.insert(template.name.clone(), template);
        Ok(())
    }

    /// 获取模板
    pub fn get_template(&self, name: &str) -> Option<&BrainTemplate> {
        self.templates.get(name)
    }

    /// 列出所有模板
    pub fn list_templates(&self) -> Vec<&BrainTemplate> {
        self.templates.values().collect()
    }

    /// 从模板创建并注册活跃副脑
    ///
    /// 返回新副脑的 BrainId
    pub fn create_from_template(&mut self, template_name: &str) -> Result<BrainId> {
        let template = self
            .templates
            .get(template_name)
            .ok_or_else(|| EvolutionError::TemplateNotFound(template_name.into()))?
            .clone();

        let id = template.brain_id();
        if self.active.contains_key(&id) {
            return Err(EvolutionError::BrainAlreadyActive(template_name.into()));
        }

        let entry = ActiveBrainEntry {
            template,
            registered_at: Utc::now(),
            task_count: 0,
        };

        tracing::info!("创建新副脑: {} ({})", id, entry.template.description);
        self.active.insert(id.clone(), entry);
        Ok(id)
    }

    /// 直接注册一个活跃副脑（用于内置副脑）
    pub fn register_active(&mut self, template: BrainTemplate) -> Result<()> {
        let id = template.brain_id();
        if self.active.contains_key(&id) {
            return Err(EvolutionError::BrainAlreadyActive(id.to_string()));
        }
        self.active.insert(
            id,
            ActiveBrainEntry {
                template,
                registered_at: Utc::now(),
                task_count: 0,
            },
        );
        Ok(())
    }

    /// 将活跃副脑休眠（硬休眠）
    ///
    /// 1. 序列化配置到磁盘
    /// 2. 从活跃列表移除
    pub fn dormant(&mut self, id: &BrainId, current_weight: f64) -> Result<()> {
        let entry = self
            .active
            .remove(id)
            .ok_or_else(|| EvolutionError::BrainNotFound(id.to_string()))?;

        let record = DormantRecord {
            template: entry.template,
            dormant_since: Utc::now(),
            original_weight: current_weight,
            task_count: entry.task_count,
        };

        self.dormancy_mgr.persist(&record)?;
        tracing::info!("副脑 {} 已进入硬休眠（权重={:.2}）", id, current_weight);
        Ok(())
    }

    /// 唤醒休眠副脑
    ///
    /// 1. 从磁盘加载配置
    /// 2. 重新注册为活跃
    /// 3. 删除磁盘记录
    /// 4. 返回 (BrainId, 唤醒后权重)
    pub fn wake(&mut self, id: &BrainId) -> Result<(BrainId, Weight)> {
        if self.active.contains_key(id) {
            return Err(EvolutionError::BrainAlreadyActive(id.to_string()));
        }

        let record = self.dormancy_mgr.load(id)?;
        let wake_weight = self.dormancy_mgr.wake_weight(record.original_weight);

        let entry = ActiveBrainEntry {
            template: record.template.clone(),
            registered_at: Utc::now(),
            task_count: record.task_count,
        };

        // 注册模板（如果还没有）
        self.templates
            .entry(record.template.name.clone())
            .or_insert(record.template);

        self.active.insert(id.clone(), entry);
        self.dormancy_mgr.remove(id)?;

        tracing::info!(
            "副脑 {} 已唤醒，权重 {:.2} → {:.2}",
            id,
            record.original_weight,
            wake_weight.value()
        );
        Ok((id.clone(), wake_weight))
    }

    /// 检查活跃副脑是否应该休眠
    ///
    /// 返回需要休眠的副脑 ID 列表
    pub fn check_dormancy(&self, weights: &HashMap<BrainId, Weight>) -> Vec<BrainId> {
        weights
            .iter()
            .filter(|(id, w)| {
                // 只检查活跃的自定义副脑（跳过内置副脑）
                self.active.contains_key(*id) && self.dormancy_mgr.should_dormant(w)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// 获取活跃副脑列表
    pub fn active_brains(&self) -> &HashMap<BrainId, ActiveBrainEntry> {
        &self.active
    }

    /// 获取活跃副脑数量
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// 获取休眠副脑列表
    pub fn dormant_brains(&self) -> Vec<DormantRecord> {
        self.dormancy_mgr.list_dormant()
    }

    /// 获取休眠副脑数量
    pub fn dormant_count(&self) -> usize {
        self.dormancy_mgr.list_dormant().len()
    }

    /// 增加活跃副脑的任务计数
    pub fn increment_task_count(&mut self, id: &BrainId) {
        if let Some(entry) = self.active.get_mut(id) {
            entry.task_count += 1;
        }
    }

    /// 获取活跃副脑的模板
    pub fn get_active_template(&self, id: &BrainId) -> Option<&BrainTemplate> {
        self.active.get(id).map(|e| &e.template)
    }

    /// 获取休眠管理器引用
    pub fn dormancy_manager(&self) -> &DormancyManager {
        &self.dormancy_mgr
    }

    /// 获取所有活跃+休眠副脑的摘要状态
    pub fn status_summary(&self) -> BrainRegistryStatus {
        let active: Vec<BrainStatusEntry> = self
            .active
            .iter()
            .map(|(id, e)| BrainStatusEntry {
                id: id.clone(),
                name: e.template.name.clone(),
                description: e.template.description.clone(),
                state: BrainState::Active,
                task_count: e.task_count,
                capabilities: e.template.capabilities.clone(),
            })
            .collect();

        let dormant: Vec<BrainStatusEntry> = self
            .dormancy_mgr
            .list_dormant()
            .into_iter()
            .map(|r| BrainStatusEntry {
                id: r.template.brain_id(),
                name: r.template.name.clone(),
                description: r.template.description.clone(),
                state: BrainState::Dormant,
                task_count: r.task_count,
                capabilities: r.template.capabilities.clone(),
            })
            .collect();

        BrainRegistryStatus { active, dormant }
    }
}

/// 副脑状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrainState {
    Active,
    Dormant,
}

/// 副脑状态条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainStatusEntry {
    pub id: BrainId,
    pub name: String,
    pub description: String,
    pub state: BrainState,
    pub task_count: u32,
    pub capabilities: Vec<String>,
}

/// 注册中心状态摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainRegistryStatus {
    pub active: Vec<BrainStatusEntry>,
    pub dormant: Vec<BrainStatusEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_new() {
        let registry = BrainRegistry::new();
        assert_eq!(registry.active_count(), 0);
        // 内置模板应该被加载
        assert!(!registry.list_templates().is_empty());
    }

    #[test]
    fn test_register_template() {
        let mut registry = BrainRegistry::new();
        let t = BrainTemplate::new("custom", "custom brain");
        registry.register_template(t).unwrap();
        assert!(registry.get_template("custom").is_some());
    }

    #[test]
    fn test_register_duplicate_template() {
        let mut registry = BrainRegistry::new();
        let t1 = BrainTemplate::new("dup", "first");
        let t2 = BrainTemplate::new("dup", "second");
        registry.register_template(t1).unwrap();
        assert!(registry.register_template(t2).is_err());
    }

    #[test]
    fn test_create_from_template() {
        let mut registry = BrainRegistry::new();
        let t = BrainTemplate::new("test-create", "test");
        registry.register_template(t).unwrap();

        let id = registry.create_from_template("test-create").unwrap();
        assert_eq!(id.0, "test-create");
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn test_create_nonexistent_template() {
        let mut registry = BrainRegistry::new();
        assert!(registry.create_from_template("nonexistent").is_err());
    }

    #[test]
    fn test_dormant_and_wake() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = BrainRegistry::with_storage_dir(dir.path().to_path_buf());

        let t = BrainTemplate::new("dormant-test", "test dormancy");
        registry.register_template(t).unwrap();
        let id = registry.create_from_template("dormant-test").unwrap();

        // 休眠
        registry.dormant(&id, 0.15).unwrap();
        assert_eq!(registry.active_count(), 0);
        assert_eq!(registry.dormant_count(), 1);

        // 唤醒
        let (woke_id, wake_weight) = registry.wake(&id).unwrap();
        assert_eq!(woke_id, id);
        // 0.15 + 0.1 = 0.25
        assert!((wake_weight.value() - 0.25).abs() < f64::EPSILON);
        assert_eq!(registry.active_count(), 1);
        assert_eq!(registry.dormant_count(), 0);
    }

    #[test]
    fn test_check_dormancy() {
        let mut registry = BrainRegistry::new();
        let t = BrainTemplate::new("check-dorm", "test check");
        registry.register_template(t).unwrap();
        let id = registry.create_from_template("check-dorm").unwrap();

        let mut weights = HashMap::new();
        weights.insert(id.clone(), Weight(0.15));
        weights.insert(BrainId::reasoning(), Weight(0.8));

        let should_dorm = registry.check_dormancy(&weights);
        assert_eq!(should_dorm.len(), 1);
        assert_eq!(should_dorm[0], id);
    }

    #[test]
    fn test_status_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = BrainRegistry::with_storage_dir(dir.path().to_path_buf());

        let t = BrainTemplate::new("status-test", "test status")
            .with_capabilities(vec!["test"]);
        registry.register_template(t).unwrap();
        registry.create_from_template("status-test").unwrap();

        let status = registry.status_summary();
        assert_eq!(status.active.len(), 1);
        assert!(status.dormant.is_empty());
        assert_eq!(status.active[0].name, "status-test");
    }

    #[test]
    fn test_increment_task_count() {
        let mut registry = BrainRegistry::new();
        let t = BrainTemplate::new("count-test", "test");
        registry.register_template(t).unwrap();
        let id = registry.create_from_template("count-test").unwrap();

        registry.increment_task_count(&id);
        registry.increment_task_count(&id);
        let entry = registry.active_brains().get(&id).unwrap();
        assert_eq!(entry.task_count, 2);
    }
}
