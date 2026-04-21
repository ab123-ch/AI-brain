//! Unified tool executor trait.
//!
//! Defines the async interface for executing tool calls from the reasoning brain.
//! The concrete implementation lives in `brain-motor` where it bridges to the
//! actual `runtime` and `tools` crates.

use crate::types::ToolCall;
use crate::types::ToolExecutionResult;

// ---------------------------------------------------------------------------
// Tool Executor Trait
// ---------------------------------------------------------------------------

/// Async trait for executing tool calls.
///
/// Implementations bridge to the underlying tool backends:
/// - Builtin tools (file ops, bash, search) via `tools` crate
/// - MCP tools via `McpServerManager`
/// - Plugin tools via `GlobalToolRegistry`
pub trait ToolExecutor: Send + Sync {
    /// Execute a tool call and return the result.
    ///
    /// The caller is responsible for calling `guard_check()` before
    /// invoking this method.
    fn execute(
        &self,
        tool_call: &ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + '_>>;

    /// List all available tools as descriptors (for LLM tool definitions).
    fn list_tools(&self) -> Vec<ToolDescriptor>;
}

// ---------------------------------------------------------------------------
// Tool Descriptor (re-export from types for convenience)
// ---------------------------------------------------------------------------

/// Use the existing ToolDescriptor from types.rs.
pub use crate::types::ToolDescriptor;

// ---------------------------------------------------------------------------
// Stub Executor (for testing)
// ---------------------------------------------------------------------------

/// A no-op executor that records tool calls without executing them.
/// Useful in tests where you don't want real file I/O or bash execution.
pub struct StubToolExecutor {
    /// Pre-configured responses keyed by tool name.
    responses: std::collections::HashMap<String, String>,
}

impl StubToolExecutor {
    /// Create a stub that returns empty success for all tools.
    #[must_use]
    pub fn new() -> Self {
        Self {
            responses: std::collections::HashMap::new(),
        }
    }

    /// Add a canned response for a tool name.
    #[must_use]
    pub fn with_response(mut self, tool_name: &str, output: String) -> Self {
        self.responses.insert(tool_name.into(), output);
        self
    }
}

impl Default for StubToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolExecutor for StubToolExecutor {
    fn execute(
        &self,
        tool_call: &ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + '_>> {
        let tool_name = tool_call.tool_name.clone();
        let output = self.responses.get(&tool_name).cloned().unwrap_or_default();

        Box::pin(std::future::ready(ToolExecutionResult {
            tool_name,
            output,
            is_error: false,
            duration_ms: 0,
        }))
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.responses
            .keys()
            .map(|name| ToolDescriptor {
                name: name.clone(),
                description: format!("Stub tool: {name}"),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn stub_executor_returns_canned_response() {
        let exec = StubToolExecutor::new().with_response("read_file", "file contents".into());
        let call = ToolCall {
            tool_name: "read_file".into(),
            input: json!({ "path": "/tmp/test.rs" }),
            validated: false,
            validation_id: None,
        };
        let result = exec.execute(&call).await;
        assert_eq!(result.tool_name, "read_file");
        assert_eq!(result.output, "file contents");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn stub_executor_returns_empty_for_unknown() {
        let exec = StubToolExecutor::new();
        let call = ToolCall {
            tool_name: "bash".into(),
            input: json!({ "command": "ls" }),
            validated: false,
            validation_id: None,
        };
        let result = exec.execute(&call).await;
        assert_eq!(result.output, "");
    }

    #[test]
    fn stub_executor_lists_tools() {
        let exec = StubToolExecutor::new()
            .with_response("read_file", "r".into())
            .with_response("bash", "b".into());
        let tools = exec.list_tools();
        assert_eq!(tools.len(), 2);
    }
}
