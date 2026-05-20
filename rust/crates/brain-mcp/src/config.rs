use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 单个 MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, rename = "type")]
    pub transport_type: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}
