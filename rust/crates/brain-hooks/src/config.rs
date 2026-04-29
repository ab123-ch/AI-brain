use serde::{Deserialize, Serialize};

use crate::types::{HookEvent, HookHandlerConfig};

/// Hook 配置段（对应 config.toml 的 [hooks]）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    /// 全局开关
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// eval_gate 内置 handler 配置
    #[serde(default)]
    pub eval_gate: EvalGateConfig,

    /// PreToolUse command handlers
    #[serde(default)]
    pub pre_tool_use: Vec<HookHandlerConfig>,

    /// PostToolUse command handlers
    #[serde(default)]
    pub post_tool_use: Vec<HookHandlerConfig>,

    /// PostQuery command handlers
    #[serde(default)]
    pub post_query: Vec<HookHandlerConfig>,

    /// OnShutdown command handlers
    #[serde(default)]
    pub on_shutdown: Vec<HookHandlerConfig>,

    /// SessionStart command handlers
    #[serde(default)]
    pub session_start: Vec<HookHandlerConfig>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            eval_gate: EvalGateConfig::default(),
            pre_tool_use: Vec::new(),
            post_tool_use: Vec::new(),
            post_query: Vec::new(),
            on_shutdown: Vec::new(),
            session_start: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// eval_gate 内置 handler 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalGateConfig {
    /// 是否启用评估脑自决策（纯规则判断，不调 LLM）
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for EvalGateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
        }
    }
}

impl HooksConfig {
    /// 从 toml::Value 解析
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into().unwrap_or_default()
    }

    /// 获取某个事件的所有 handler 配置
    pub fn handlers_for_event(&self, event: HookEvent) -> &[HookHandlerConfig] {
        match event {
            HookEvent::PreToolUse => &self.pre_tool_use,
            HookEvent::PostToolUse => &self.post_tool_use,
            HookEvent::PostQuery => &self.post_query,
            HookEvent::OnShutdown => &self.on_shutdown,
            HookEvent::SessionStart => &self.session_start,
        }
    }
}
