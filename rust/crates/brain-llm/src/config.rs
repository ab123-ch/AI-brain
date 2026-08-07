use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LlmError, Result};
use crate::gemini::GeminiClient;
use crate::openai_compat::OpenAiCompatClient;
use crate::provider::LlmProvider;

/// Provider 协议类型，决定路由到哪个 Client
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    OpenAi,
    Gemini,
}

/// 全局代理配置段
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxySection {
    /// 全局默认代理地址，不配则全程不走代理
    #[serde(default)]
    pub default: Option<String>,
}

/// LLM 层完整配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub llm: LlmSection,
    #[serde(default)]
    pub brain: BrainSection,
    /// 全局代理配置
    #[serde(default)]
    pub proxy: ProxySection,
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
    /// 可供实例直接选择的模型目录，保持 TOML 声明顺序。
    #[serde(default)]
    pub instance_models: Vec<InstanceModelConfig>,
    #[serde(default)]
    pub brain_models: HashMap<String, String>,
    /// 每个脑独立的生成参数（max_tokens / temperature），未配置的脑走 defaults
    #[serde(default)]
    pub brain_params: HashMap<String, BrainParams>,
    /// 每脑独立厂商映射（未配置的脑走 default_provider）
    #[serde(default)]
    pub brain_providers: HashMap<String, String>,
    #[serde(default)]
    pub defaults: LlmDefaults,
}

/// 单个实例可选择的模型配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceModelConfig {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub model: String,
}

/// 单个脑的生成参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainParams {
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedModelPolicy {
    pub policy_id: String,
    #[serde(default)]
    pub label: String,
    pub provider: String,
    pub model: String,
    pub max_output_tokens: u32,
    pub temperature: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_base: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// 协议类型，缺省 openai（向后兼容旧配置）
    #[serde(default)]
    pub kind: ProviderKind,
    /// 代理设置：None=跟随全局，Some("none")=关闭，Some(url)=独立地址，Some("")=视为无代理
    #[serde(default)]
    pub proxy: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            api_base: String::new(),
            api_key_env: String::new(),
            api_key: None,
            kind: ProviderKind::OpenAi,
            proxy: None,
        }
    }
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

    /// 获取某个副脑应使用的 provider 名
    pub fn provider_for_brain(&self, brain_name: &str) -> &str {
        self.llm
            .brain_providers
            .get(brain_name)
            .map_or(&self.llm.default_provider, |s| s.as_str())
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

    #[must_use]
    pub fn resolve_model_policy(&self, policy_id: &str) -> ResolvedModelPolicy {
        let (max_output_tokens, temperature) = self.params_for_brain(policy_id);
        ResolvedModelPolicy {
            policy_id: policy_id.to_string(),
            label: policy_id.to_string(),
            provider: self.provider_for_brain(policy_id).to_string(),
            model: self.model_for_brain(policy_id).to_string(),
            max_output_tokens,
            temperature,
        }
    }

    /// 解析实例模型目录中的一个模型策略。
    pub fn resolve_instance_model_policy(&self, policy_id: &str) -> Result<ResolvedModelPolicy> {
        let instance_model = self
            .llm
            .instance_models
            .iter()
            .find(|model| model.id == policy_id)
            .ok_or_else(|| {
                LlmError::Config(format!("实例模型目录中不存在模型策略: {policy_id}"))
            })?;

        if instance_model.id.trim().is_empty() {
            return Err(LlmError::Config("实例模型目录项的 id 不能为空".into()));
        }
        if instance_model.id != instance_model.id.trim() {
            return Err(LlmError::Config(
                "实例模型目录项的 id 不能包含首尾空白字符".into(),
            ));
        }
        if instance_model.label.trim().is_empty() {
            return Err(LlmError::Config("实例模型目录项的 label 不能为空".into()));
        }
        if instance_model.model.trim().is_empty() {
            return Err(LlmError::Config("实例模型目录项的 model 不能为空".into()));
        }

        self.resolve_api_key(&instance_model.provider)?;

        Ok(ResolvedModelPolicy {
            policy_id: instance_model.id.clone(),
            label: instance_model.label.clone(),
            provider: instance_model.provider.clone(),
            model: instance_model.model.clone(),
            max_output_tokens: self.llm.defaults.max_tokens,
            temperature: self.llm.defaults.temperature,
        })
    }

    /// 返回 Web 实例可选择的模型策略。
    ///
    /// 始终保留遗留的 `main` 策略；目录项则按 TOML 声明顺序追加。
    #[must_use]
    pub fn available_instance_model_policies(&self) -> Vec<ResolvedModelPolicy> {
        let mut policies = vec![self.resolve_model_policy("main")];
        let mut seen_ids = HashSet::from(["main".to_string()]);

        for instance_model in &self.llm.instance_models {
            let id = instance_model.id.trim();
            if id.is_empty() {
                tracing::warn!("跳过实例模型目录项：模型 id 为空");
                continue;
            }

            if instance_model.id != id {
                tracing::warn!(
                    model_id = id,
                    "跳过实例模型目录项：模型 id 包含首尾空白字符"
                );
                continue;
            }

            if instance_model.label.trim().is_empty() {
                tracing::warn!(model_id = id, "跳过实例模型目录项：模型 label 为空");
                continue;
            }

            if instance_model.model.trim().is_empty() {
                tracing::warn!(model_id = id, "跳过实例模型目录项：模型名称为空");
                continue;
            }

            if !seen_ids.insert(id.to_string()) {
                tracing::warn!(model_id = id, "跳过实例模型目录项：模型 id 重复");
                continue;
            }

            match self.resolve_instance_model_policy(&instance_model.id) {
                Ok(policy) => policies.push(policy),
                Err(LlmError::ProviderNotFound(_)) => {
                    tracing::warn!(model_id = id, "跳过实例模型目录项：provider 不存在");
                }
                Err(LlmError::ApiKeyNotFound(_)) => {
                    tracing::warn!(model_id = id, "跳过实例模型目录项：provider 未配置 API Key");
                }
                Err(error) => {
                    tracing::warn!(model_id = id, error = %error, "跳过实例模型目录项：配置无效");
                }
            }
        }

        policies
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
                if !key.trim().is_empty() {
                    return Ok(key);
                }
            }
        }

        // 其次直接配置
        if let Some(key) = &provider.api_key {
            if !key.trim().is_empty() {
                return Ok(key.clone());
            }
        }

        Err(LlmError::ApiKeyNotFound(
            if provider.api_key_env.is_empty() {
                "未配置 api_key 或 api_key_env".into()
            } else {
                provider.api_key_env.clone()
            },
        ))
    }

    /// 解析某 provider 最终使用的代理地址
    ///
    /// 优先级：provider.proxy（"none"/空串=关闭，url=独立）> proxy.default（全局）
    pub fn resolve_proxy(&self, provider_name: &str) -> Result<Option<String>> {
        let provider = self
            .llm
            .providers
            .get(provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound(provider_name.to_string()))?;

        Ok(match &provider.proxy {
            Some(s) if s == "none" => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(s.clone()),
            None => self.proxy.default.clone(),
        })
    }

    /// 为某个副脑构建 LLM 客户端
    ///
    /// 如果副脑不需要 LLM（不在 brain_models 中且不在 defaults 中），返回 None
    pub fn create_brain_client(&self, brain_name: &str) -> Result<Box<dyn LlmProvider>> {
        let policy = self.resolve_model_policy(brain_name);
        self.create_resolved_model_policy_client(&policy)
    }

    /// 为实例模型策略构建 LLM 客户端。
    ///
    /// 目录中没有该策略时兼容遗留副脑策略，沿用原有的默认回退规则。
    pub fn create_model_policy_client(&self, policy_id: &str) -> Result<Box<dyn LlmProvider>> {
        let policy = if policy_id != "main"
            && self
                .llm
                .instance_models
                .iter()
                .any(|model| model.id == policy_id)
        {
            self.resolve_instance_model_policy(policy_id)?
        } else {
            self.resolve_model_policy(policy_id)
        };

        self.create_resolved_model_policy_client(&policy)
    }

    fn create_resolved_model_policy_client(
        &self,
        policy: &ResolvedModelPolicy,
    ) -> Result<Box<dyn LlmProvider>> {
        let api_key = self.resolve_api_key(&policy.provider)?;

        let provider_config = self
            .llm
            .providers
            .get(&policy.provider)
            .ok_or_else(|| LlmError::ProviderNotFound(policy.provider.clone()))?;

        let proxy_url = self.resolve_proxy(&policy.provider)?;
        match provider_config.kind {
            ProviderKind::OpenAi => Ok(Box::new(
                OpenAiCompatClient::new(
                    provider_config.api_base.clone(),
                    api_key,
                    policy.model.clone(),
                    policy.max_output_tokens,
                    policy.temperature,
                )
                .with_proxy(proxy_url),
            )),
            ProviderKind::Gemini => Ok(Box::new(GeminiClient::try_new(
                provider_config.api_base.clone(),
                api_key,
                policy.model.clone(),
                policy.max_output_tokens,
                policy.temperature,
                proxy_url,
            )?)),
        }
    }

    /// 生成默认配置（用于首次运行）
    pub fn default_config() -> Self {
        let providers = Self::default_providers();

        let mut brain_providers = HashMap::new();
        brain_providers.insert("main".into(), "xiaomi".into());
        brain_providers.insert("memory".into(), "xiaomi".into());
        brain_providers.insert("eval".into(), "deepseek".into());
        brain_providers.insert("evolver".into(), "xiaomi".into());

        let mut brain_models = HashMap::new();
        brain_models.insert("main".into(), "mimo-7b".into());
        brain_models.insert("sensory".into(), "mimo-7b".into());
        brain_models.insert("reasoning".into(), "mimo-7b".into());
        brain_models.insert("memory".into(), "mimo-7b".into());
        brain_models.insert("eval".into(), "deepseek-chat".into());
        brain_models.insert("evolver".into(), "mimo-7b".into());

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
                default_provider: "xiaomi".into(),
                default_model: "mimo-7b".into(),
                providers,
                instance_models: Vec::new(),
                brain_models,
                brain_params,
                brain_providers,
                defaults: LlmDefaults::default(),
            },
            brain: BrainSection::default(),
            proxy: ProxySection::default(),
            hooks: None,
        }
    }

    /// 默认 provider 列表（含 gemini 示例）
    fn default_providers() -> HashMap<String, ProviderConfig> {
        let mut providers = HashMap::new();
        providers.insert(
            "xiaomi".into(),
            ProviderConfig {
                api_base: "https://xiaomi-llm.example.com/v1".into(),
                api_key_env: "XIAOMI_API_KEY".into(),
                ..Default::default()
            },
        );
        providers.insert(
            "deepseek".into(),
            ProviderConfig {
                api_base: "https://api.deepseek.com/v1".into(),
                api_key_env: "DEEPSEEK_API_KEY".into(),
                ..Default::default()
            },
        );
        providers.insert(
            "zhipu".into(),
            ProviderConfig {
                api_base: "https://open.bigmodel.cn/api/paas/v4".into(),
                api_key_env: "ZHIPU_API_KEY".into(),
                ..Default::default()
            },
        );
        providers.insert(
            "kimi".into(),
            ProviderConfig {
                api_base: "https://api.moonshot.ai/v1".into(),
                api_key_env: "MOONSHOT_API_KEY".into(),
                ..Default::default()
            },
        );
        providers.insert(
            "gemini".into(),
            ProviderConfig {
                api_base: "https://generativelanguage.googleapis.com/v1beta".into(),
                api_key_env: "GEMINI_API_KEY".into(),
                kind: ProviderKind::Gemini,
                ..Default::default()
            },
        );
        providers
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
        assert_eq!(config.llm.default_provider, "xiaomi");
        assert_eq!(config.llm.default_model, "mimo-7b");
        assert!(config.llm.instance_models.is_empty());
        assert!(config.llm.brain_providers.contains_key("main"));
        assert!(!config.llm.brain_providers.contains_key("novel"));
        assert!(!config.llm.brain_models.contains_key("novel"));
        assert_eq!(config.provider_for_brain("eval"), "deepseek");
        assert_eq!(config.model_for_brain("main"), "mimo-7b");
        assert_eq!(config.model_for_brain("eval"), "deepseek-chat");
        let main_policy = config.resolve_model_policy("main");
        assert_eq!(main_policy.policy_id, "main");
        assert_eq!(main_policy.label, "main");
        assert_eq!(main_policy.provider, "xiaomi");
        assert_eq!(main_policy.model, "mimo-7b");
        assert_eq!(
            main_policy.max_output_tokens,
            config.params_for_brain("main").0
        );

        let kimi = config
            .llm
            .providers
            .get("kimi")
            .expect("默认配置应包含 Kimi provider");
        assert_eq!(kimi.api_base, "https://api.moonshot.ai/v1");
        assert_eq!(kimi.api_key_env, "MOONSHOT_API_KEY");
        assert_eq!(kimi.kind, ProviderKind::OpenAi);
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

[[llm.instance_models]]
id = "glm-5-1"
label = "GLM 5.1"
provider = "zhipu"
model = "glm-5.1"

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
        assert_eq!(config.llm.instance_models.len(), 1);
        assert_eq!(config.llm.instance_models[0].id, "glm-5-1");
    }

    #[test]
    fn instance_model_entries_round_trip_from_toml_in_declared_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[llm]
default_provider = "relay"
default_model = "default-model"

[llm.providers.relay]
api_base = "https://relay.example.com/v1"
api_key = "fake-key"

[[llm.instance_models]]
id = "deepseek-v4-pro"
label = "DeepSeek V4 Pro"
provider = "relay"
model = "deepseek-v4-pro"

[[llm.instance_models]]
id = "gemini-2-5-flash"
label = "Gemini 2.5 Flash"
provider = "relay"
model = "gemini-2.5-flash"
"#,
        )
        .unwrap();

        let config = LlmConfig::load(&path).unwrap();
        let encoded = toml::to_string(&config).unwrap();
        let deepseek = encoded.find("deepseek-v4-pro").unwrap();
        let gemini = encoded.find("gemini-2-5-flash").unwrap();

        assert!(deepseek < gemini);
        assert!(encoded.contains("DeepSeek V4 Pro"));
        assert!(encoded.contains("Gemini 2.5 Flash"));
    }

    fn config_with_instance_models() -> LlmConfig {
        let mut config = LlmConfig::default_config();
        config.llm.providers.insert(
            "catalog".into(),
            ProviderConfig {
                api_base: "https://relay.example.com/v1".into(),
                api_key: Some("fake-key".into()),
                ..Default::default()
            },
        );
        config.llm.providers.insert(
            "without-key".into(),
            ProviderConfig {
                api_base: "https://relay.example.com/v1".into(),
                api_key_env: "BRAIN_LLM_INSTANCE_MODEL_MISSING_KEY".into(),
                ..Default::default()
            },
        );
        config.llm.providers.insert(
            "blank-key".into(),
            ProviderConfig {
                api_base: "https://relay.example.com/v1".into(),
                api_key: Some("   ".into()),
                ..Default::default()
            },
        );
        config.llm.instance_models = vec![
            InstanceModelConfig {
                id: "deepseek-v4-pro".into(),
                label: "DeepSeek V4 Pro".into(),
                provider: "catalog".into(),
                model: "deepseek-v4-pro".into(),
            },
            InstanceModelConfig {
                id: "gemini-2-5-flash".into(),
                label: "Gemini 2.5 Flash".into(),
                provider: "catalog".into(),
                model: "gemini-2.5-flash".into(),
            },
        ];
        config
    }

    #[test]
    fn available_instance_model_policies_keep_main_then_toml_order() {
        let config = config_with_instance_models();

        let policies = config.available_instance_model_policies();

        assert_eq!(
            policies
                .iter()
                .map(|policy| policy.policy_id.as_str())
                .collect::<Vec<_>>(),
            vec!["main", "deepseek-v4-pro", "gemini-2-5-flash"]
        );
        assert_eq!(policies[0].label, "main");
        assert_eq!(policies[1].label, "DeepSeek V4 Pro");
        assert_eq!(policies[1].provider, "catalog");
        assert_eq!(policies[1].model, "deepseek-v4-pro");
    }

    #[test]
    fn available_instance_model_policies_skip_invalid_catalog_entries() {
        let mut config = config_with_instance_models();
        config.llm.instance_models.extend([
            InstanceModelConfig {
                id: " ".into(),
                label: "空标识".into(),
                provider: "catalog".into(),
                model: "empty-id".into(),
            },
            InstanceModelConfig {
                id: "deepseek-v4-pro".into(),
                label: "重复标识".into(),
                provider: "catalog".into(),
                model: "duplicate".into(),
            },
            InstanceModelConfig {
                id: "unknown-provider".into(),
                label: "未知厂商".into(),
                provider: "does-not-exist".into(),
                model: "unknown".into(),
            },
            InstanceModelConfig {
                id: "without-key".into(),
                label: "无密钥".into(),
                provider: "without-key".into(),
                model: "without-key".into(),
            },
            InstanceModelConfig {
                id: "blank-key".into(),
                label: "空白密钥".into(),
                provider: "blank-key".into(),
                model: "blank-key".into(),
            },
            InstanceModelConfig {
                id: "blank-label".into(),
                label: " \t".into(),
                provider: "catalog".into(),
                model: "blank-label".into(),
            },
            InstanceModelConfig {
                id: "blank-model".into(),
                label: "空白模型".into(),
                provider: "catalog".into(),
                model: " \t".into(),
            },
            InstanceModelConfig {
                id: " spaced-id ".into(),
                label: "空白边界 ID".into(),
                provider: "catalog".into(),
                model: "spaced-id-model".into(),
            },
        ]);

        let policies = config.available_instance_model_policies();

        assert_eq!(
            policies
                .iter()
                .map(|policy| policy.policy_id.as_str())
                .collect::<Vec<_>>(),
            vec!["main", "deepseek-v4-pro", "gemini-2-5-flash"]
        );
    }

    #[test]
    fn resolved_model_policy_without_label_remains_json_compatible() {
        let policy: ResolvedModelPolicy = serde_json::from_str(
            r#"{
                "policy_id": "main",
                "provider": "deepseek",
                "model": "deepseek-v4-pro",
                "max_output_tokens": 4096,
                "temperature": 0.7
            }"#,
        )
        .unwrap();

        assert!(policy.label.is_empty());
    }

    #[test]
    fn resolve_api_key_without_configured_source_has_clear_error() {
        let mut config = LlmConfig::default_config();
        config.llm.providers.insert(
            "no-key-source".into(),
            ProviderConfig {
                api_base: "https://relay.example.com/v1".into(),
                ..Default::default()
            },
        );

        let error = config.resolve_api_key("no-key-source").unwrap_err();

        assert_eq!(
            error.to_string(),
            "API key not found: 未配置 api_key 或 api_key_env"
        );
    }

    #[test]
    fn resolve_api_key_ignores_blank_values_without_changing_nonempty_priority() {
        let mut config = LlmConfig::default_config();
        let provider = config.llm.providers.get_mut("xiaomi").unwrap();
        provider.api_key_env = "BRAIN_LLM_BLANK_API_KEY".into();
        provider.api_key = Some("direct-key".into());
        std::env::set_var("BRAIN_LLM_BLANK_API_KEY", " \t");

        let resolved = config.resolve_api_key("xiaomi").unwrap();

        assert_eq!(resolved, "direct-key");
        config.llm.providers.get_mut("xiaomi").unwrap().api_key = Some(" \n".into());
        assert!(config.resolve_api_key("xiaomi").is_err());
        std::env::remove_var("BRAIN_LLM_BLANK_API_KEY");
    }

    #[test]
    fn resolve_instance_model_policy_keeps_catalog_label_and_defaults() {
        let config = config_with_instance_models();

        let policy = config
            .resolve_instance_model_policy("gemini-2-5-flash")
            .unwrap();

        assert_eq!(policy.policy_id, "gemini-2-5-flash");
        assert_eq!(policy.label, "Gemini 2.5 Flash");
        assert_eq!(policy.provider, "catalog");
        assert_eq!(policy.model, "gemini-2.5-flash");
        assert_eq!(policy.max_output_tokens, config.llm.defaults.max_tokens);
        assert!((policy.temperature - config.llm.defaults.temperature).abs() < f64::EPSILON);
    }

    #[test]
    fn create_model_policy_client_uses_openai_compatible_catalog_model() {
        let config = config_with_instance_models();

        let client = config
            .create_model_policy_client("deepseek-v4-pro")
            .unwrap();

        assert_eq!(client.model(), "deepseek-v4-pro");
    }

    #[test]
    fn create_model_policy_client_routes_gemini_catalog_model() {
        let mut config = config_with_instance_models();
        let provider = config.llm.providers.get_mut("catalog").unwrap();
        provider.api_base = "https://generativelanguage.googleapis.com/v1beta".into();
        provider.kind = ProviderKind::Gemini;

        let client = config
            .create_model_policy_client("gemini-2-5-flash")
            .unwrap();

        assert_eq!(client.model(), "gemini-2.5-flash");
    }

    #[test]
    fn create_model_policy_client_keeps_main_compatible() {
        let mut config = config_with_instance_models();
        config.llm.providers.get_mut("xiaomi").unwrap().api_key = Some("fake-key".into());

        let client = config.create_model_policy_client("main").unwrap();

        assert_eq!(client.model(), "mimo-7b");
    }

    #[test]
    fn load_default_returns_default_when_no_file() {
        let original_home = std::env::var_os("HOME");
        let original_userprofile = std::env::var_os("USERPROFILE");
        let dir = tempfile::tempdir().unwrap();

        std::env::set_var("HOME", dir.path());
        std::env::remove_var("USERPROFILE");

        let config = LlmConfig::load_default().unwrap();

        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_userprofile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }

        assert_eq!(config.llm.default_provider, "xiaomi");
    }

    #[test]
    fn resolve_api_key_from_env() {
        let config = LlmConfig::default_config();
        std::env::set_var("XIAOMI_API_KEY", "test-key-123");
        let key = config.resolve_api_key("xiaomi").unwrap();
        assert_eq!(key, "test-key-123");
        std::env::remove_var("XIAOMI_API_KEY");
    }

    #[test]
    fn resolve_api_key_direct_config() {
        let mut config = LlmConfig::default_config();
        if let Some(provider) = config.llm.providers.get_mut("xiaomi") {
            provider.api_key = Some("direct-key".into());
            provider.api_key_env = "NONEXISTENT_VAR".into();
        }
        let key = config.resolve_api_key("xiaomi").unwrap();
        assert_eq!(key, "direct-key");
    }

    #[test]
    fn resolve_api_key_env_priority_over_direct() {
        let mut config = LlmConfig::default_config();
        if let Some(provider) = config.llm.providers.get_mut("xiaomi") {
            provider.api_key = Some("direct-key".into());
            provider.api_key_env = "TEST_PRIORITY_KEY".into();
        }
        std::env::set_var("TEST_PRIORITY_KEY", "env-key");
        let key = config.resolve_api_key("xiaomi").unwrap();
        assert_eq!(key, "env-key");
        std::env::remove_var("TEST_PRIORITY_KEY");
    }

    #[test]
    fn brain_providers_fallback_to_default() {
        let config = LlmConfig::default_config();
        assert_eq!(
            config.provider_for_brain("main"),
            config.llm.default_provider
        );
        assert_eq!(
            config.provider_for_brain("unknown"),
            config.llm.default_provider
        );
    }

    #[test]
    fn brain_providers_per_brain_routing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[llm]
default_provider = "xiaomi"
default_model = "mimo-7b"

[llm.providers.xiaomi]
api_base = "https://xiaomi.example.com/v1"
api_key_env = "XIAOMI_API_KEY"

[llm.providers.deepseek]
api_base = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[llm.brain_providers]
main = "xiaomi"
eval = "deepseek"
"#,
        )
        .unwrap();

        let config = LlmConfig::load(&path).unwrap();
        assert_eq!(config.provider_for_brain("main"), "xiaomi");
        assert_eq!(config.provider_for_brain("eval"), "deepseek");
        assert_eq!(config.provider_for_brain("memory"), "xiaomi");
        assert_eq!(config.provider_for_brain("unknown"), "xiaomi");
    }

    #[test]
    fn load_config_without_brain_providers_is_backward_compatible() {
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

[llm.brain_models]
reasoning = "glm-5.1"
"#,
        )
        .unwrap();

        let config = LlmConfig::load(&path).unwrap();
        assert!(config.llm.brain_providers.is_empty());
        assert_eq!(config.provider_for_brain("main"), "zhipu");
        assert_eq!(config.provider_for_brain("eval"), "zhipu");
    }

    // ===== T1: ProviderKind + resolve_proxy（正常/边界/异常）=====

    #[test]
    fn provider_kind_defaults_to_openai_for_old_config() {
        let toml = r#"
[llm]
default_provider = "x"
default_model = "m"

[llm.providers.x]
api_base = "https://x.example.com/v1"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, toml).unwrap();
        let cfg = LlmConfig::load(&path).unwrap();
        let pc = cfg.llm.providers.get("x").unwrap();
        assert_eq!(pc.kind, ProviderKind::OpenAi);
        assert!(pc.proxy.is_none());
    }

    #[test]
    fn provider_kind_gemini_parsed() {
        let toml = r#"
[llm]
default_provider = "g"
default_model = "gemini-2.5-flash"

[llm.providers.g]
api_base = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY"
kind = "gemini"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, toml).unwrap();
        let cfg = LlmConfig::load(&path).unwrap();
        let pc = cfg.llm.providers.get("g").unwrap();
        assert_eq!(pc.kind, ProviderKind::Gemini);
    }

    #[test]
    fn resolve_proxy_follows_global_default() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        assert_eq!(
            cfg.resolve_proxy("xiaomi").unwrap(),
            Some("http://127.0.0.1:7890".to_string())
        );
    }

    #[test]
    fn resolve_proxy_custom_overrides_global() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        if let Some(p) = cfg.llm.providers.get_mut("xiaomi") {
            p.proxy = Some("http://127.0.0.1:1080".into());
        }
        assert_eq!(
            cfg.resolve_proxy("xiaomi").unwrap(),
            Some("http://127.0.0.1:1080".to_string())
        );
    }

    #[test]
    fn resolve_proxy_none_when_no_global() {
        let cfg = LlmConfig::default_config();
        assert_eq!(cfg.resolve_proxy("xiaomi").unwrap(), None);
    }

    #[test]
    fn resolve_proxy_none_disables_global() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        if let Some(p) = cfg.llm.providers.get_mut("xiaomi") {
            p.proxy = Some("none".into());
        }
        assert_eq!(cfg.resolve_proxy("xiaomi").unwrap(), None);
    }

    #[test]
    fn resolve_proxy_empty_string_treated_as_none() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        if let Some(p) = cfg.llm.providers.get_mut("xiaomi") {
            p.proxy = Some(String::new());
        }
        assert_eq!(cfg.resolve_proxy("xiaomi").unwrap(), None);
    }

    #[test]
    fn resolve_proxy_provider_not_found_errors() {
        let cfg = LlmConfig::default_config();
        assert!(cfg.resolve_proxy("nonexistent").is_err());
    }

    #[test]
    fn create_brain_client_routes_gemini_kind() {
        let mut cfg = LlmConfig::default_config();
        cfg.llm.default_provider = "gemini".into();
        cfg.llm.default_model = "gemini-2.5-flash".into();
        cfg.llm
            .brain_providers
            .insert("main".into(), "gemini".into());
        cfg.llm.brain_models.remove("main");
        cfg.llm.providers.get_mut("gemini").unwrap().api_key = Some("fake-key".into());

        let client = cfg.create_brain_client("main").unwrap();
        assert_eq!(client.model(), "gemini-2.5-flash");
    }

    #[test]
    fn create_brain_client_routes_openai_by_default() {
        let mut cfg = LlmConfig::default_config();
        cfg.llm.providers.get_mut("xiaomi").unwrap().api_key = Some("fake-key".into());
        let client = cfg.create_brain_client("main").unwrap();
        assert_eq!(client.model(), "mimo-7b");
    }

    #[test]
    fn create_brain_client_injects_proxy_into_gemini() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        cfg.llm.default_provider = "gemini".into();
        cfg.llm
            .brain_providers
            .insert("main".into(), "gemini".into());
        cfg.llm.providers.get_mut("gemini").unwrap().api_key = Some("fake-key".into());
        assert!(cfg.create_brain_client("main").is_ok());
    }

    #[test]
    fn create_brain_client_rejects_invalid_gemini_proxy() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("not-a-url".into());
        cfg.llm.default_provider = "gemini".into();
        cfg.llm
            .brain_providers
            .insert("main".into(), "gemini".into());
        cfg.llm.providers.get_mut("gemini").unwrap().api_key = Some("fake-key".into());
        assert!(cfg.create_brain_client("main").is_err());
    }
}
