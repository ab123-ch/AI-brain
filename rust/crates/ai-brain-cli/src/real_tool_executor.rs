//! Real tool executor that bridges to the `tools` crate.
//!
//! This is the production implementation used by the orchestrator,
//! as opposed to `StubToolExecutor` which is only for tests.

use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{ToolCall, ToolDescriptor, ToolExecutionResult};
use std::collections::HashMap;

/// Production tool executor that delegates to `tools::execute_tool`.
pub struct RealToolExecutor {
    /// Tool descriptors (name → descriptor) for list_tools()
    tool_descriptors: HashMap<String, ToolDescriptor>,
}

impl RealToolExecutor {
    /// Create a new executor, registering all MVP tool specs from the tools crate.
    pub fn new() -> Self {
        let specs = tools::mvp_tool_specs();
        let tool_descriptors = specs
            .iter()
            .map(|spec| {
                (
                    spec.name.to_string(),
                    ToolDescriptor {
                        name: spec.name.to_string(),
                        description: spec.description.to_string(),
                        input_schema: spec.input_schema.clone(),
                    },
                )
            })
            .collect();
        Self { tool_descriptors }
    }
}

impl Default for RealToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolExecutor for RealToolExecutor {
    fn execute(
        &self,
        tool_call: &ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + '_>> {
        let name = tool_call.tool_name.clone();
        let input = tool_call.input.clone();
        let tool_name_owned = tool_call.tool_name.clone();

        Box::pin(async move {
            let start = std::time::Instant::now();
            // tools::execute_tool 可能内部创建自己的 runtime（如 bash），
            // 必须在阻塞线程中运行以避免 runtime 冲突
            let result = tokio::task::spawn_blocking(move || tools::execute_tool(&name, &input))
                .await
                .unwrap_or_else(|e| Err(format!("工具执行 panic: {e}")));

            let (output, is_error) = match result {
                Ok(output) => (output, false),
                Err(e) => (e, true),
            };
            let duration_ms = start.elapsed().as_millis() as u64;

            ToolExecutionResult {
                tool_name: tool_name_owned,
                output,
                is_error,
                duration_ms,
            }
        })
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.tool_descriptors.values().cloned().collect()
    }
}

/// Convert tools crate `ToolSpec` to brain-llm `ToolDefinition` for register_tools().
pub fn mvp_tool_definitions() -> Vec<brain_llm::ToolDefinition> {
    tools::mvp_tool_specs()
        .iter()
        .map(|spec| brain_llm::ToolDefinition {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            input_schema: spec.input_schema.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn real_executor_lists_mvp_tools() {
        let exec = RealToolExecutor::new();
        let tools = exec.list_tools();
        assert!(!tools.is_empty(), "MVP 工具列表不应为空");
        // 检查核心工具存在
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"bash"), "应包含 bash 工具");
        assert!(names.contains(&"read_file"), "应包含 read_file 工具");
    }

    #[tokio::test]
    async fn real_executor_executes_echo() {
        let exec = RealToolExecutor::new();
        let call = ToolCall {
            tool_name: "bash".into(),
            input: json!({ "command": "echo hello_world_test" }),
            validated: false,
            validation_id: None,
        };
        let result = exec.execute(&call).await;
        assert_eq!(result.tool_name, "bash");
        assert!(
            result.output.contains("hello_world_test"),
            "bash echo 输出应包含预期文本: {}",
            result.output
        );
        assert!(!result.is_error);
        assert!(result.duration_ms > 0);
    }

    #[tokio::test]
    async fn real_executor_unknown_tool_returns_error() {
        let exec = RealToolExecutor::new();
        let call = ToolCall {
            tool_name: "nonexistent_tool".into(),
            input: json!({}),
            validated: false,
            validation_id: None,
        };
        let result = exec.execute(&call).await;
        assert!(result.is_error, "未知工具应返回错误");
    }

    #[test]
    fn mvp_tool_definitions_not_empty() {
        let defs = mvp_tool_definitions();
        assert!(!defs.is_empty());
    }
}
