use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 单个 MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// stdio 模式：启动命令
    #[serde(default)]
    pub command: Option<String>,
    /// stdio 模式：命令参数
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// 环境变量
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// 传输类型：sse / http（无此字段则为 stdio）
    #[serde(default, rename = "type")]
    pub transport_type: Option<String>,
    /// SSE/HTTP 模式的 URL
    #[serde(default)]
    pub url: Option<String>,
}

/// mcp-servers.json 根结构
#[derive(Debug, Deserialize)]
struct McpServersFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: HashMap<String, McpServerConfig>,
}

/// 加载 MCP 服务器配置文件，返回 (服务器名, 配置) 列表
pub fn load_mcp_servers(path: &Path) -> Result<Vec<(String, McpServerConfig)>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path).map_err(|e| format!("读取 MCP 配置失败: {e}"))?;
    let file: McpServersFile =
        serde_json::from_str(&content).map_err(|e| format!("解析 MCP 配置失败: {e}"))?;
    Ok(file.mcp_servers.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stdio_config() {
        let json = r#"{"command": "npx", "args": ["-y", "@upstreamapi/context7-mcp@latest"], "env": {"API_KEY": "test"}}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.command, Some("npx".to_string()));
        assert_eq!(config.args.unwrap().len(), 2);
    }

    #[test]
    fn parse_sse_config() {
        let json = r#"{"type": "sse", "url": "http://localhost:8080/sse"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.transport_type, Some("sse".to_string()));
        assert_eq!(config.url, Some("http://localhost:8080/sse".to_string()));
    }

    #[test]
    fn load_mcp_servers_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_path = dir.path().join("mcp-servers.json");
        let content =
            r#"{"mcpServers": {"context7": {"command": "npx", "args": ["-y", "context7"]}}}"#;
        std::fs::write(&config_path, content).unwrap();

        let servers = load_mcp_servers(&config_path).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].0, "context7");
    }

    #[test]
    fn load_nonexistent_returns_empty() {
        let servers =
            load_mcp_servers(&std::path::PathBuf::from("/tmp/does-not-exist-xyz")).unwrap();
        assert!(servers.is_empty());
    }
}
