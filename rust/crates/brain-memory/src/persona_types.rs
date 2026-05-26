//! 原生人格系统类型定义
//!
//! 人格注册表、人格配置、人格 CRUD 类型。
//! 不依赖任何外部 MCP，完全自包含。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 人格定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    /// 唯一标识符（英文，如 "cyber-brain", "writer"）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 人格描述（一段话说明该人格的角色定位）
    pub description: String,
    /// 人格专属 system prompt 片段（注入主脑 prompt）
    pub system_prompt: String,
    /// 人格专属配置
    pub config: PersonaConfig,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 最后激活时间
    pub last_active_at: DateTime<Utc>,
}

/// 人格配置（影响主脑和评估脑行为）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaConfig {
    /// 默认使用的语言（如 "zh-CN", "en"）
    pub language: String,
    /// 输出风格偏好（如 "concise", "detailed", "academic"）
    pub output_style: String,
    /// 额外的模型参数覆盖（可选）
    pub model_override: Option<String>,
    /// 评估脑敏感度（0.0-1.0，越高越严格）
    pub eval_sensitivity: f64,
    /// 四步分析间隔（轮次）
    pub analysis_interval: u32,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            language: "zh-CN".into(),
            output_style: "concise".into(),
            model_override: None,
            eval_sensitivity: 0.7,
            analysis_interval: 5,
        }
    }
}

/// 人格注册表（存储在 ~/.ai-brain/personas/registry.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaRegistry {
    /// 所有注册的人格
    pub personas: Vec<Persona>,
    /// 当前激活的人格 ID（空字符串 = 默认）
    pub active_persona_id: String,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

impl Default for PersonaRegistry {
    fn default() -> Self {
        Self {
            personas: vec![Persona::default_persona()],
            active_persona_id: "default".into(),
            updated_at: Utc::now(),
        }
    }
}

impl Persona {
    /// 创建默认人格
    pub fn default_persona() -> Self {
        Self {
            id: "default".into(),
            name: "智脑".into(),
            description: "默认人格，通用AI助手".into(),
            system_prompt: String::new(),
            config: PersonaConfig::default(),
            created_at: Utc::now(),
            last_active_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_default_persona() {
        let reg = PersonaRegistry::default();
        assert_eq!(reg.personas.len(), 1);
        assert_eq!(reg.active_persona_id, "default");
        assert_eq!(reg.personas[0].id, "default");
    }

    #[test]
    fn persona_config_default_values() {
        let config = PersonaConfig::default();
        assert_eq!(config.language, "zh-CN");
        assert_eq!(config.analysis_interval, 5);
        assert!(config.model_override.is_none());
        assert!((config.eval_sensitivity - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn persona_serde_roundtrip() {
        let p = Persona {
            id: "writer".into(),
            name: "滚开作家".into(),
            description: "网文创作".into(),
            system_prompt: "你是一个网文写作助手...".into(),
            config: PersonaConfig {
                output_style: "literary".into(),
                eval_sensitivity: 0.5,
                ..Default::default()
            },
            created_at: Utc::now(),
            last_active_at: Utc::now(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Persona = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "writer");
        assert_eq!(back.name, "滚开作家");
        assert!((back.config.eval_sensitivity - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn registry_serde_roundtrip() {
        let mut reg = PersonaRegistry::default();
        reg.personas.push(Persona {
            id: "cyber-brain".into(),
            name: "赛博大脑".into(),
            description: "多语言开发".into(),
            system_prompt: "你是赛博大脑".into(),
            config: PersonaConfig {
                language: "en".into(),
                ..Default::default()
            },
            created_at: Utc::now(),
            last_active_at: Utc::now(),
        });
        let json = serde_json::to_string_pretty(&reg).unwrap();
        let back: PersonaRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.personas.len(), 2);
        assert_eq!(back.personas[1].id, "cyber-brain");
    }

    #[test]
    fn default_persona_has_empty_prompt() {
        let p = Persona::default_persona();
        assert!(p.system_prompt.is_empty());
        assert_eq!(p.config.language, "zh-CN");
    }
}
