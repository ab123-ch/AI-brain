use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Hook 事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    /// 工具执行前（tool_loop 内）
    PreToolUse,
    /// 工具执行后（tool_loop 内）
    PostToolUse,
    /// 主脑回复完成后（orchestrator 内）
    PostQuery,
    /// 会话关闭时
    OnShutdown,
    /// 会话启动时
    SessionStart,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostQuery => "PostQuery",
            Self::OnShutdown => "OnShutdown",
            Self::SessionStart => "SessionStart",
        }
    }
}

/// 工具名匹配器
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// 匹配所有（matcher 为空、"*"、或省略）
    MatchAll,
    /// 管道分隔的多工具名匹配（"Bash|Edit|Write"）
    PipeDelimited(Vec<String>),
}

impl Matcher {
    /// 从配置字符串解析匹配器
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            None | Some("" | "*") => Self::MatchAll,
            Some(s) => Self::PipeDelimited(s.split('|').map(String::from).collect()),
        }
    }

    /// 检查工具名是否匹配
    pub fn matches(&self, tool_name: &str) -> bool {
        match self {
            Self::MatchAll => true,
            Self::PipeDelimited(names) => names.iter().any(|n| n == tool_name),
        }
    }
}

/// Hook handler 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HookHandlerConfig {
    /// Shell 命令执行
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default)]
        matcher: Option<String>,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
    /// 内置 handler
    #[serde(rename = "builtin")]
    Builtin {
        name: String,
    },
}

fn default_timeout() -> u64 {
    30
}

/// Hook 输入上下文
#[derive(Debug, Clone)]
pub struct HookInput {
    pub event: HookEvent,
    pub session_id: String,
    pub cwd: PathBuf,
    /// 工具层字段（PreToolUse / PostToolUse）
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
    pub is_error: bool,
    /// Brain 层字段（PostQuery）
    pub user_input: Option<String>,
    pub ai_output: Option<String>,
}

/// Hook 决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Deny,
}

/// Hook 执行输出
#[derive(Debug, Clone)]
pub struct HookOutput {
    pub decision: HookDecision,
    pub reason: Option<String>,
    /// PostQuery 专用：是否触发评估脑
    pub trigger_eval: bool,
    /// 注入到后续流程的系统消息
    pub system_message: Option<String>,
}

impl HookOutput {
    pub fn allow() -> Self {
        Self {
            decision: HookDecision::Allow,
            reason: None,
            trigger_eval: false,
            system_message: None,
        }
    }

    pub fn deny(reason: String) -> Self {
        Self {
            decision: HookDecision::Deny,
            reason: Some(reason),
            trigger_eval: false,
            system_message: None,
        }
    }
}
