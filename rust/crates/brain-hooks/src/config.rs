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
    #[serde(default)]
    pub enabled: bool,

    /// 评估触发模式
    /// - "always":      所有非 trivial 查询都触发评估（旧行为）
    /// - "on_file_edit": 仅当主脑调用了文件修改工具（Edit/Write/Bash 含文件操作）时触发
    /// - "never":        从不触发评估
    #[serde(default = "default_eval_mode")]
    pub mode: EvalGateMode,
}

/// 评估触发模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvalGateMode {
    #[serde(rename = "always")]
    Always,
    #[serde(rename = "on_file_edit")]
    OnFileEdit,
    #[serde(rename = "never")]
    Never,
}

fn default_eval_mode() -> EvalGateMode {
    EvalGateMode::OnFileEdit
}

impl Default for EvalGateConfig {
    fn default() -> Self {
        Self {
            // 通用评估脑默认关闭；需要时可在配置中显式启用。
            enabled: false,
            mode: default_eval_mode(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_brain_is_opt_in_by_default() {
        let config = HooksConfig::default();
        assert!(config.enabled);
        assert!(!config.eval_gate.enabled);
        assert_eq!(config.eval_gate.mode, EvalGateMode::OnFileEdit);
    }

    #[test]
    fn omitted_eval_enabled_stays_disabled_when_section_exists() {
        let config: HooksConfig = toml::from_str(
            r#"
enabled = true

[eval_gate]
mode = "on_file_edit"
"#,
        )
        .expect("valid hooks config");
        assert!(!config.eval_gate.enabled);
    }
}
