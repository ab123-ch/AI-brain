//! Real tool executor that bridges to the `tools` crate.
//!
//! This is the production implementation used by the orchestrator,
//! as opposed to `StubToolExecutor` which is only for tests.

use std::collections::HashMap;
use std::sync::Arc;

use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{ToolCall, ToolDescriptor, ToolExecutionResult};
use brain_memory::memory_brain::MemoryBrain;

/// Production tool executor that delegates to `tools::execute_tool` for built-in tools
/// and handles `search_memory` directly via MemoryBrain.
pub struct RealToolExecutor {
    /// Tool descriptors (name → descriptor) for list_tools()
    tool_descriptors: HashMap<String, ToolDescriptor>,
    /// MemoryBrain for search_memory tool
    memory_brain: Option<Arc<tokio::sync::Mutex<MemoryBrain>>>,
    /// Dispatch bus for async agent completion notifications
    dispatch: Option<brain_dispatch::TokioDispatch>,
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
        Self {
            tool_descriptors,
            memory_brain: None,
            dispatch: None,
        }
    }

    /// Create with an optional MemoryBrain for search_memory support.
    pub fn with_memory(memory_brain: Option<Arc<tokio::sync::Mutex<MemoryBrain>>>) -> Self {
        let mut exec = Self::new();
        exec.memory_brain = memory_brain;
        exec
    }

    /// Create with MemoryBrain and dispatch bus for async agent notifications.
    pub fn with_dispatch(
        memory_brain: Option<Arc<tokio::sync::Mutex<MemoryBrain>>>,
        dispatch: brain_dispatch::TokioDispatch,
    ) -> Self {
        let mut exec = Self::with_memory(memory_brain);
        exec.dispatch = Some(dispatch);
        exec
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

        // special-case: search_memory 由 MemoryBrain 处理
        if name == "search_memory" {
            let memory_brain = self.memory_brain.clone();
            return Box::pin(async move {
                let start = std::time::Instant::now();
                let mem: Arc<tokio::sync::Mutex<MemoryBrain>> = match memory_brain {
                    Some(m) => m,
                    None => {
                        return ToolExecutionResult {
                            tool_name: tool_name_owned,
                            output: "search_memory: MemoryBrain not available".into(),
                            is_error: true,
                            duration_ms: start.elapsed().as_millis() as u64,
                        };
                    }
                };
                let query = input
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let max_results = input
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5)
                    .min(20) as usize;

                let entries = {
                    let guard = mem.lock().await;
                    guard.search(&query, max_results)
                };

                let output = if entries.is_empty() {
                    "未找到相关记忆".into()
                } else {
                    let lines: Vec<String> = entries
                        .iter()
                        .enumerate()
                        .map(|(i, e)| {
                            let preview: String = e.content.chars().take(300).collect();
                            format!(
                                "{}. [{}] (层级={:?}, 重要度={:.2})\n   {}",
                                i + 1,
                                e.id,
                                e.layer,
                                e.importance,
                                preview
                            )
                        })
                        .collect();
                    lines.join("\n\n")
                };

                ToolExecutionResult {
                    tool_name: tool_name_owned,
                    output,
                    is_error: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                }
            });
        }

        // special-case: list_recent_memories 由 MemoryBrain 处理
        if name == "list_recent_memories" {
            let memory_brain = self.memory_brain.clone();
            return Box::pin(async move {
                let start = std::time::Instant::now();
                let mem: Arc<tokio::sync::Mutex<MemoryBrain>> = match memory_brain {
                    Some(m) => m,
                    None => {
                        return ToolExecutionResult {
                            tool_name: tool_name_owned,
                            output: "list_recent_memories: MemoryBrain not available".into(),
                            is_error: true,
                            duration_ms: start.elapsed().as_millis() as u64,
                        };
                    }
                };
                let limit = input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5)
                    .min(20) as usize;

                let summaries = {
                    let guard = mem.lock().await;
                    guard.list_recent_summaries(limit)
                };

                let output = if summaries.is_empty() {
                    "暂无历史会话记忆".into()
                } else {
                    let lines: Vec<String> = summaries
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let tags = s.tags.join(", ");
                            format!(
                                "{}. [{}] {} ~ {}\n   标签: {}\n   摘要: {}\n   路径: {}",
                                i + 1,
                                s.session_id,
                                s.session_start,
                                s.session_end,
                                tags,
                                s.summary_preview,
                                s.file_path,
                            )
                        })
                        .collect();
                    lines.join("\n\n")
                };

                ToolExecutionResult {
                    tool_name: tool_name_owned,
                    output,
                    is_error: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                }
            });
        }

        Box::pin(async move {
            let start = std::time::Instant::now();

            // 检查是否是异步子代理 (run_in_background=true)
            let is_async_agent = name == "Agent"
                && input
                    .get("run_in_background")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

            let result = tokio::task::spawn_blocking(move || tools::execute_tool(&name, &input))
                .await
                .unwrap_or_else(|e| Err(format!("工具执行 panic: {e}")));

            let (output, is_error) = match result {
                Ok(output) => (output, false),
                Err(e) => (e, true),
            };

            // 异步子代理完成后，通过 dispatch 注入通知
            if is_async_agent && !is_error {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&output) {
                    if let Some(dispatch) = &self.dispatch {
                        let agent_id = parsed
                            .get("agentId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let agent_name = parsed
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        dispatch.inject_sync(
                            agent_id,
                            agent_name,
                            brain_dispatch::AgentStatus::Completed,
                            output.clone(),
                            None,
                            start.elapsed().as_millis() as u64,
                        );
                    }
                }
            }

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
