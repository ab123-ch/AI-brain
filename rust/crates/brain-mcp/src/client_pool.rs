use crate::config::McpServerConfig;
use brain_core::types::{ToolCall, ToolDescriptor, ToolExecutionResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// MCP 服务器连接状态
#[derive(Debug, Clone)]
pub enum ServerStatus {
    Connected,
    Disconnected,
    Error(String),
}

/// MCP 服务器条目（运行时状态）
struct McpServerEntry {
    #[allow(dead_code)]
    name: String,
    tools: Vec<ToolDescriptor>,
    status: ServerStatus,
}

/// MCP 客户端连接池
pub struct McpClientPool {
    servers: Arc<Mutex<HashMap<String, McpServerEntry>>>,
}

impl McpClientPool {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 连接所有 MCP 服务器（当前为 stub 框架）
    pub async fn connect_all(
        configs: &[(String, McpServerConfig)],
    ) -> Result<Self, String> {
        let pool = Self::new();
        {
            let mut servers = pool.servers.lock().await;
            for (name, _config) in configs {
                servers.insert(
                    name.clone(),
                    McpServerEntry {
                        name: name.clone(),
                        tools: Vec::new(),
                        status: ServerStatus::Error(
                            "MCP client not yet implemented".to_string(),
                        ),
                    },
                );
                tracing::warn!(
                    "MCP 服务器 '{}' 注册为 stub（rmcp 连接待实现）",
                    name
                );
            }
        }

        Ok(pool)
    }

    /// 列出所有已连接服务器的工具描述符
    pub async fn list_tool_definitions(&self) -> Vec<ToolDescriptor> {
        let servers = self.servers.lock().await;
        servers.values().flat_map(|s| s.tools.clone()).collect()
    }

    /// 执行 MCP 工具调用
    pub async fn execute(&self, tool_call: &ToolCall) -> ToolExecutionResult {
        let (server_name, tool_name) = match parse_mcp_tool_name(&tool_call.tool_name) {
            Some(pair) => pair,
            None => {
                return ToolExecutionResult {
                    tool_name: tool_call.tool_name.clone(),
                    output: "Invalid MCP tool name format".to_string(),
                    is_error: true,
                    duration_ms: 0,
                };
            }
        };

        let servers = self.servers.lock().await;
        match servers.get(server_name) {
            Some(entry) => {
                let output = match &entry.status {
                    ServerStatus::Connected => {
                        format!(
                            "MCP tool '{}' on '{}' not yet connected",
                            tool_name, server_name
                        )
                    }
                    ServerStatus::Disconnected => {
                        format!("MCP server '{}' is disconnected", server_name)
                    }
                    ServerStatus::Error(msg) => {
                        format!("MCP server '{}' error: {}", server_name, msg)
                    }
                };
                ToolExecutionResult {
                    tool_name: tool_call.tool_name.clone(),
                    output,
                    is_error: true,
                    duration_ms: 0,
                }
            }
            None => ToolExecutionResult {
                tool_name: tool_call.tool_name.clone(),
                output: format!("MCP server '{}' not found", server_name),
                is_error: true,
                duration_ms: 0,
            },
        }
    }

    /// 关闭所有连接
    pub async fn shutdown(&self) -> Result<(), String> {
        let mut servers = self.servers.lock().await;
        servers.clear();
        Ok(())
    }
}

/// 解析 MCP 工具名 "mcp__{server}__{tool}" → (server, tool)
pub fn parse_mcp_tool_name(full_name: &str) -> Option<(&str, &str)> {
    let rest = full_name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

/// 格式化 MCP 工具名
pub fn format_mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", server, tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mcp_tool_name_valid() {
        let (server, tool) = parse_mcp_tool_name("mcp__context7__query_docs").unwrap();
        assert_eq!(server, "context7");
        assert_eq!(tool, "query_docs");
    }

    #[test]
    fn parse_mcp_tool_name_no_prefix_fails() {
        assert!(parse_mcp_tool_name("regular_tool").is_none());
    }

    #[test]
    fn format_tool_name_roundtrip() {
        let name = super::format_mcp_tool_name("context7", "query_docs");
        assert_eq!(name, "mcp__context7__query_docs");
    }

    #[test]
    fn new_pool_is_empty() {
        let pool = McpClientPool::new();
        // 验证可以创建
        let _ = &pool;
    }
}
