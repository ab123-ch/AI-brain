//! 原生人格管理器
//!
//! 人格注册表的 CRUD 操作、人格切换、prompt 生成。
//! 不依赖任何外部 MCP，完全自包含。

use crate::error::MemoryError;
use crate::persona_types::{Persona, PersonaConfig, PersonaRegistry};
use std::path::{Path, PathBuf};

/// 人格管理器
pub struct PersonaManager {
    base_dir: PathBuf,
    registry: PersonaRegistry,
}

impl PersonaManager {
    /// 从磁盘加载或创建默认注册表
    pub fn load_or_create(base_dir: &Path) -> Result<Self, MemoryError> {
        let registry_path = base_dir.join("personas").join("registry.json");
        let registry = if registry_path.exists() {
            let data = std::fs::read_to_string(&registry_path)?;
            serde_json::from_str(&data)?
        } else {
            PersonaRegistry::default()
        };
        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            registry,
        })
    }

    /// 列出所有人格
    pub fn list(&self) -> &[Persona] {
        &self.registry.personas
    }

    /// 获取当前激活人格
    pub fn active(&self) -> &Persona {
        self.registry
            .personas
            .iter()
            .find(|p| p.id == self.registry.active_persona_id)
            .unwrap_or(&self.registry.personas[0])
    }

    /// 获取当前激活人格 ID
    pub fn active_id(&self) -> &str {
        &self.registry.active_persona_id
    }

    /// 切换人格
    pub fn switch(&mut self, persona_id: &str) -> Result<&Persona, MemoryError> {
        self.registry
            .personas
            .iter()
            .find(|p| p.id == persona_id)
            .ok_or_else(|| MemoryError::NotFound(format!("人格不存在: {persona_id}")))?;

        self.registry.active_persona_id = persona_id.to_string();

        // 更新最后激活时间
        if let Some(p) = self
            .registry
            .personas
            .iter_mut()
            .find(|p| p.id == persona_id)
        {
            p.last_active_at = chrono::Utc::now();
        }

        self.persist()?;
        Ok(self.active())
    }

    /// 创建新人格
    pub fn create(
        &mut self,
        id: String,
        name: String,
        description: String,
        system_prompt: String,
        config: PersonaConfig,
    ) -> Result<&Persona, MemoryError> {
        // 检查 ID 唯一性
        if self.registry.personas.iter().any(|p| p.id == id) {
            return Err(MemoryError::Conflict(format!("人格ID已存在: {id}")));
        }
        let persona = Persona {
            id,
            name,
            description,
            system_prompt,
            config,
            created_at: chrono::Utc::now(),
            last_active_at: chrono::Utc::now(),
        };
        self.registry.personas.push(persona);
        self.persist()?;
        Ok(self.registry.personas.last().unwrap())
    }

    /// 删除人格（不能删除 default 和当前激活的）
    pub fn delete(&mut self, persona_id: &str) -> Result<(), MemoryError> {
        if persona_id == "default" {
            return Err(MemoryError::Conflict("不能删除默认人格".into()));
        }
        if persona_id == self.registry.active_persona_id {
            return Err(MemoryError::Conflict("不能删除当前激活的人格".into()));
        }
        let idx = self
            .registry
            .personas
            .iter()
            .position(|p| p.id == persona_id)
            .ok_or_else(|| MemoryError::NotFound(format!("人格不存在: {persona_id}")))?;

        self.registry.personas.remove(idx);

        // 删除该人格的记忆目录
        let persona_dir = self.base_dir.join("personas").join(persona_id);
        if persona_dir.exists() {
            std::fs::remove_dir_all(&persona_dir)?;
        }

        self.persist()?;
        Ok(())
    }

    /// 生成人格的 prompt 注入内容
    pub fn build_persona_prompt(&self) -> String {
        let persona = self.active();
        if persona.system_prompt.is_empty() {
            return String::new();
        }
        format!(
            "[人格: {}]\n{}\n[语言: {}, 风格: {}]",
            persona.name,
            persona.system_prompt,
            persona.config.language,
            persona.config.output_style
        )
    }

    /// 获取当前人格的分析间隔配置
    pub fn analysis_interval(&self) -> u32 {
        self.active().config.analysis_interval
    }

    /// 获取当前人格的评估敏感度
    pub fn eval_sensitivity(&self) -> f64 {
        self.active().config.eval_sensitivity
    }

    /// 持久化注册表到磁盘
    fn persist(&self) -> Result<(), MemoryError> {
        let dir = self.base_dir.join("personas");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("registry.json");
        let json = serde_json::to_string_pretty(&self.registry)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_switch_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert_eq!(mgr.active_id(), "default");

        mgr.create(
            "writer".into(),
            "作家".into(),
            "网文".into(),
            "你是网文助手".into(),
            PersonaConfig::default(),
        )
        .unwrap();
        assert_eq!(mgr.list().len(), 2);

        mgr.switch("writer").unwrap();
        assert_eq!(mgr.active_id(), "writer");
        assert_eq!(mgr.active().name, "作家");
    }

    #[test]
    fn cannot_delete_default_or_active() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert!(mgr.delete("default").is_err());

        mgr.create(
            "writer".into(),
            "作家".into(),
            "网文".into(),
            "".into(),
            PersonaConfig::default(),
        )
        .unwrap();
        mgr.switch("writer").unwrap();
        assert!(mgr.delete("writer").is_err()); // 当前激活的不能删
    }

    #[test]
    fn persona_prompt_includes_config() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        mgr.create(
            "writer".into(),
            "作家".into(),
            "网文".into(),
            "模仿滚开风格".into(),
            PersonaConfig {
                language: "zh-CN".into(),
                output_style: "literary".into(),
                ..Default::default()
            },
        )
        .unwrap();
        mgr.switch("writer").unwrap();
        let prompt = mgr.build_persona_prompt();
        assert!(prompt.contains("滚开风格"));
        assert!(prompt.contains("literary"));
    }

    #[test]
    fn default_persona_empty_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert!(mgr.build_persona_prompt().is_empty());
    }

    #[test]
    fn persist_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        mgr.create(
            "writer".into(),
            "作家".into(),
            "网文".into(),
            "".into(),
            PersonaConfig::default(),
        )
        .unwrap();
        mgr.switch("writer").unwrap();

        // 重新加载
        let mgr2 = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert_eq!(mgr2.active_id(), "writer");
        assert_eq!(mgr2.list().len(), 2);
    }

    #[test]
    fn analysis_interval_per_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert_eq!(mgr.analysis_interval(), 5); // default

        mgr.create(
            "writer".into(),
            "作家".into(),
            "网文".into(),
            "".into(),
            PersonaConfig {
                analysis_interval: 20,
                ..Default::default()
            },
        )
        .unwrap();
        mgr.switch("writer").unwrap();
        assert_eq!(mgr.analysis_interval(), 20);
    }

    #[test]
    fn cannot_create_duplicate_id() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        mgr.create(
            "writer".into(),
            "作家".into(),
            "网文".into(),
            "".into(),
            PersonaConfig::default(),
        )
        .unwrap();
        let result = mgr.create(
            "writer".into(),
            "另一个".into(),
            "不同描述".into(),
            "".into(),
            PersonaConfig::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn cannot_switch_to_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert!(mgr.switch("nonexistent").is_err());
    }

    #[test]
    fn delete_persona_removes_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        mgr.create(
            "temp".into(),
            "临时".into(),
            "测试用".into(),
            "".into(),
            PersonaConfig::default(),
        )
        .unwrap();

        // 手动创建人格目录
        let persona_dir = tmp.path().join("personas").join("temp");
        std::fs::create_dir_all(&persona_dir).unwrap();
        assert!(persona_dir.exists());

        mgr.delete("temp").unwrap();
        assert!(!persona_dir.exists());
        assert_eq!(mgr.list().len(), 1); // 只剩 default
    }
}
