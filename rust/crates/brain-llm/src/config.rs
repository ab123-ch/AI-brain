use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LlmError, Result};
use crate::openai_compat::OpenAiCompatClient;
use crate::provider::LlmProvider;

/// LLM 层完整配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub llm: LlmSection,
    #[serde(default)]
    pub brain: BrainSection,
    /// Hook 系统配置（原始 toml::Value，由 ai-brain-cli 层解析为 brain-hooks::HooksConfig）
    #[serde(default)]
    pub hooks: Option<toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSection {
    pub default_provider: String,
    pub default_model: String,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub brain_models: HashMap<String, String>,
    /// 每个脑独立的生成参数（max_tokens / temperature），未配置的脑走 defaults
    #[serde(default)]
    pub brain_params: HashMap<String, BrainParams>,
    #[serde(default)]
    pub defaults: LlmDefaults,
}

/// 单个脑的生成参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainParams {
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_base: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmDefaults {
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
}

impl Default for LlmDefaults {
    fn default() -> Self {
        Self {
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
        }
    }
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_temperature() -> f64 {
    0.7
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrainSection {
    #[serde(default)]
    pub thresholds: HashMap<String, f64>,
}

impl LlmConfig {
    /// 从指定路径加载配置
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| LlmError::Config(format!("读取配置失败: {e}")))?;
        let config: Self =
            toml::from_str(&content).map_err(|e| LlmError::Config(format!("解析配置失败: {e}")))?;
        Ok(config)
    }

    /// 从默认路径加载 (~/.ai-brain/config.toml)
    pub fn load_default() -> Result<Self> {
        let path = Self::default_config_path();
        if path.exists() {
            Self::load(&path)
        } else {
            Ok(Self::default_config())
        }
    }

    /// 默认配置文件路径
    pub fn default_config_path() -> PathBuf {
        dirs_home().join(".ai-brain").join("config.toml")
    }

    /// 获取某个副脑应使用的模型名
    pub fn model_for_brain(&self, brain_name: &str) -> &str {
        self.llm
            .brain_models
            .get(brain_name)
            .map_or(&self.llm.default_model, |s| s.as_str())
    }

    /// 获取某个脑的生成参数（max_tokens, temperature）
    ///
    /// 优先查 brain_params 中该脑的独立配置，未配置则走 defaults
    pub fn params_for_brain(&self, brain_name: &str) -> (u32, f64) {
        if let Some(params) = self.llm.brain_params.get(brain_name) {
            (params.max_tokens, params.temperature)
        } else {
            (self.llm.defaults.max_tokens, self.llm.defaults.temperature)
        }
    }

    /// 解析 API Key（优先环境变量，其次直接配置）
    pub fn resolve_api_key(&self, provider_name: &str) -> Result<String> {
        let provider = self
            .llm
            .providers
            .get(provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound(provider_name.to_string()))?;

        // 优先环境变量
        if !provider.api_key_env.is_empty() {
            if let Ok(key) = std::env::var(&provider.api_key_env) {
                return Ok(key);
            }
        }

        // 其次直接配置
        if let Some(key) = &provider.api_key {
            return Ok(key.clone());
        }

        Err(LlmError::ApiKeyNotFound(
            if provider.api_key_env.is_empty() {
                provider.api_key.clone().unwrap_or_default()
            } else {
                provider.api_key_env.clone()
            },
        ))
    }

    /// 为某个副脑构建 LLM 客户端
    ///
    /// 如果副脑不需要 LLM（不在 brain_models 中且不在 defaults 中），返回 None
    pub fn create_brain_client(&self, brain_name: &str) -> Result<Box<dyn LlmProvider>> {
        let model = self.model_for_brain(brain_name);
        let provider_name = &self.llm.default_provider;
        let api_key = self.resolve_api_key(provider_name)?;
        let (max_tokens, temperature) = self.params_for_brain(brain_name);

        let provider_config = self
            .llm
            .providers
            .get(provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound(provider_name.clone()))?;

        let client = OpenAiCompatClient::new(
            provider_config.api_base.clone(),
            api_key,
            model.to_string(),
            max_tokens,
            temperature,
        );

        Ok(Box::new(client))
    }

    /// 生成默认配置（用于首次运行）
    pub fn default_config() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "zhipu".into(),
            ProviderConfig {
                api_base: "https://open.bigmodel.cn/api/paas/v4".into(),
                api_key_env: "ZHIPU_API_KEY".into(),
                api_key: None,
            },
        );

        let mut brain_models = HashMap::new();
        brain_models.insert("sensory".into(), "glm-4.7".into());
        brain_models.insert("reasoning".into(), "glm-5.1".into());
        brain_models.insert("memory".into(), "glm-4.7".into());
        brain_models.insert("motor".into(), "glm-5.1".into());
        brain_models.insert("validation".into(), "glm-5-turbo".into());

        let mut brain_params = HashMap::new();
        brain_params.insert(
            "main".into(),
            BrainParams {
                max_tokens: 32768,
                temperature: 0.7,
            },
        );
        brain_params.insert(
            "memory".into(),
            BrainParams {
                max_tokens: 32768,
                temperature: 0.3,
            },
        );
        brain_params.insert(
            "eval".into(),
            BrainParams {
                max_tokens: 16384,
                temperature: 0.3,
            },
        );
        brain_params.insert(
            "evolver".into(),
            BrainParams {
                max_tokens: 16384,
                temperature: 0.3,
            },
        );
        brain_params.insert(
            "sensory".into(),
            BrainParams {
                max_tokens: 8192,
                temperature: 0.3,
            },
        );
        brain_params.insert(
            "reasoning".into(),
            BrainParams {
                max_tokens: 8192,
                temperature: 0.5,
            },
        );
        brain_params.insert(
            "compact".into(),
            BrainParams {
                max_tokens: 4096,
                temperature: 0.3,
            },
        );

        Self {
            llm: LlmSection {
                default_provider: "zhipu".into(),
                default_model: "glm-4.7".into(),
                providers,
                brain_models,
                brain_params,
                defaults: LlmDefaults::default(),
            },
            brain: BrainSection::default(),
            hooks: None,
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_loads() {
        let config = LlmConfig::default_config();
        assert_eq!(config.llm.default_provider, "zhipu");
        assert_eq!(config.llm.default_model, "glm-4.7");
        assert_eq!(config.model_for_brain("reasoning"), "glm-5.1");
        assert_eq!(config.model_for_brain("motor"), "glm-5.1");
        assert_eq!(config.model_for_brain("validation"), "glm-5-turbo");
        // 未配置的副脑走 default
        assert_eq!(config.model_for_brain("unknown"), "glm-4.7");
    }

    #[test]
    fn load_from_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[llm]
default_provider = "zhipu"
default_model = "glm-4.7"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"

[llm.providers.local]
api_base = "http://localhost:11434/v1"
api_key_env = ""

[llm.brain_models]
reasoning = "glm-5.1"
motor = "glm-5.1"
validation = "glm-5-turbo"

[llm.defaults]
max_tokens = 2048
temperature = 0.5
"#,
        )
        .unwrap();

        let config = LlmConfig::load(&path).unwrap();
        assert_eq!(config.llm.default_provider, "zhipu");
        assert_eq!(config.model_for_brain("reasoning"), "glm-5.1");
        assert_eq!(config.llm.defaults.max_tokens, 2048);
        assert!((config.llm.defaults.temperature - 0.5).abs() < f64::EPSILON);
        assert!(config.llm.providers.contains_key("local"));
    }

    #[test]
    fn load_default_returns_default_when_no_file() {
        let config = LlmConfig::load_default().unwrap();
        assert_eq!(config.llm.default_provider, "zhipu");
    }

    #[test]
    fn resolve_api_key_from_env() {
        let config = LlmConfig::default_config();
        std::env::set_var("ZHIPU_API_KEY", "test-key-123");
        let key = config.resolve_api_key("zhipu").unwrap();
        assert_eq!(key, "test-key-123");
        std::env::remove_var("ZHIPU_API_KEY");
    }

    #[test]
    fn resolve_api_key_direct_config() {
        let mut config = LlmConfig::default_config();
        // 直接设置 api_key
        if let Some(provider) = config.llm.providers.get_mut("zhipu") {
            provider.api_key = Some("direct-key".into());
            provider.api_key_env = "NONEXISTENT_VAR".into();
        }
        let key = config.resolve_api_key("zhipu").unwrap();
        assert_eq!(key, "direct-key");
    }

    #[test]
    fn resolve_api_key_env_priority_over_direct() {
        let mut config = LlmConfig::default_config();
        if let Some(provider) = config.llm.providers.get_mut("zhipu") {
            provider.api_key = Some("direct-key".into());
            provider.api_key_env = "TEST_PRIORITY_KEY".into();
        }
        std::env::set_var("TEST_PRIORITY_KEY", "env-key");
        let key = config.resolve_api_key("zhipu").unwrap();
        assert_eq!(key, "env-key");
        std::env::remove_var("TEST_PRIORITY_KEY");
    }
}
