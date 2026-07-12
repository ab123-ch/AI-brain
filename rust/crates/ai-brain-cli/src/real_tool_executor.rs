//! Real tool executor that bridges to the `tools` crate.
//!
//! This is the production implementation used by the orchestrator,
//! as opposed to `StubToolExecutor` which is only for tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{ToolCall, ToolDescriptor, ToolExecutionResult};
use brain_mcp::McpClientPool;
use brain_memory::novel::{NovelMemoryDelta, NovelProject, NovelTaskType};
use brain_memory::pyramid_memory_brain::PyramidMemoryBrain;
use brain_plugin::SkillCatalog;
use uuid::Uuid;

use crate::runtime_trace::{ExchangeKind, ExchangePhase, ExchangeStatus, RuntimeExchange};

/// Production tool executor that delegates to `tools::execute_tool` for built-in tools
/// and handles `search_memory` directly via PyramidMemoryBrain.
pub struct RealToolExecutor {
    /// Tool descriptors (name → descriptor) for list_tools()
    tool_descriptors: HashMap<String, ToolDescriptor>,
    /// PyramidMemoryBrain for search_memory tool
    memory_brain: Option<Arc<tokio::sync::Mutex<PyramidMemoryBrain>>>,
    /// Dispatch bus for async agent completion notifications
    dispatch: Option<brain_dispatch::TokioDispatch>,
    /// Skill catalog for skill-based tool routing
    skill_catalog: Option<Arc<SkillCatalog>>,
    /// MCP client pool for external tool routing (mcp__server__tool)
    mcp_pool: Option<Arc<McpClientPool>>,
    /// Default SQLite path injected for graph_* tools.
    graph_db_path: Option<PathBuf>,
    /// Live brain/sub-agent exchanges consumed by WebSocket clients.
    runtime_trace_tx: Option<tokio::sync::broadcast::Sender<RuntimeExchange>>,
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
            skill_catalog: None,
            mcp_pool: None,
            graph_db_path: default_graph_db_path(),
            runtime_trace_tx: None,
        }
    }

    /// Create with an optional PyramidMemoryBrain for search_memory support.
    pub fn with_memory(memory_brain: Option<Arc<tokio::sync::Mutex<PyramidMemoryBrain>>>) -> Self {
        let mut exec = Self::new();
        exec.memory_brain = memory_brain;
        exec
    }

    /// Create with PyramidMemoryBrain and dispatch bus for async agent notifications.
    pub fn with_dispatch(
        memory_brain: Option<Arc<tokio::sync::Mutex<PyramidMemoryBrain>>>,
        dispatch: brain_dispatch::TokioDispatch,
    ) -> Self {
        let mut exec = Self::with_memory(memory_brain);
        exec.dispatch = Some(dispatch);
        exec
    }

    /// Attach a SkillCatalog for skill-based tool routing.
    pub fn with_skill_catalog(mut self, catalog: Arc<SkillCatalog>) -> Self {
        self.skill_catalog = Some(catalog);
        self
    }

    /// Attach an McpClientPool for external MCP tool routing.
    pub fn with_mcp_pool(mut self, pool: Arc<McpClientPool>) -> Self {
        self.mcp_pool = Some(pool);
        self
    }

    /// Override the graph database path injected into graph_* tools.
    pub fn with_graph_db_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.graph_db_path = Some(path.into());
        self
    }

    pub fn with_runtime_trace_sender(
        mut self,
        sender: tokio::sync::broadcast::Sender<RuntimeExchange>,
    ) -> Self {
        self.runtime_trace_tx = Some(sender);
        self
    }
}

impl Default for RealToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

fn spawn_agent_completion_notifier(
    dispatch: Option<brain_dispatch::TokioDispatch>,
    completion_rx: std::sync::mpsc::Receiver<tools::AgentCompletion>,
    trace: Option<AgentTracePublisher>,
) {
    std::thread::spawn(move || {
        let Ok(completion) = completion_rx.recv() else {
            tracing::warn!("后台子代理完成通道提前关闭，无法注入通知");
            return;
        };
        let status = match completion.status.as_str() {
            "completed" => brain_dispatch::AgentStatus::Completed,
            "failed" => brain_dispatch::AgentStatus::Failed,
            _ => brain_dispatch::AgentStatus::Running,
        };
        if let Some(trace) = trace {
            trace.publish_response(
                &completion.agent_id,
                &completion.name,
                &completion.status,
                &completion.output,
                completion.error.as_deref(),
                completion.duration_ms,
            );
        }
        if let Some(dispatch) = dispatch {
            dispatch.inject_sync(
                completion.agent_id,
                completion.name,
                status,
                completion.output,
                completion.error,
                completion.duration_ms,
            );
        }
    });
}

#[derive(Debug, Clone)]
struct AgentTraceRequest {
    description: String,
    prompt: String,
    subagent_type: String,
    requested_name: Option<String>,
}

impl AgentTraceRequest {
    fn from_input(input: &serde_json::Value) -> Self {
        Self {
            description: input
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("子代理任务")
                .to_string(),
            prompt: input
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            subagent_type: input
                .get("subagent_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("general-purpose")
                .to_string(),
            requested_name: input
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }
    }
}

#[derive(Clone)]
struct AgentTracePublisher {
    sender: tokio::sync::broadcast::Sender<RuntimeExchange>,
    request: AgentTraceRequest,
    exchange_id: String,
}

impl AgentTracePublisher {
    fn participant(&self, actual_name: &str) -> (String, String) {
        let is_novel = self.request.subagent_type.eq_ignore_ascii_case("novel")
            || self.request.subagent_type.contains("小说");
        let prefix = if is_novel { "novel" } else { "agent" };
        let role = if is_novel { "小说脑" } else { "子代理" };
        let name = if actual_name.trim().is_empty() {
            self.request
                .requested_name
                .as_deref()
                .unwrap_or(&self.request.subagent_type)
        } else {
            actual_name
        };
        (
            format!("{prefix}:{}", self.exchange_id),
            format!("{role} · {name}"),
        )
    }

    fn publish_request(&self) {
        let (participant, label) = self.participant("");
        let _ = self.sender.send(RuntimeExchange::new(
            &self.exchange_id,
            "main",
            "主脑",
            participant,
            label,
            ExchangeKind::Delegation,
            ExchangePhase::Request,
            &self.request.description,
            &self.request.prompt,
            ExchangeStatus::Running,
            None,
        ));
    }

    fn publish_response(
        &self,
        _agent_id: &str,
        actual_name: &str,
        status: &str,
        output: &str,
        error: Option<&str>,
        duration_ms: u64,
    ) {
        let (participant, label) = self.participant(actual_name);
        let failed = status.eq_ignore_ascii_case("failed") || error.is_some();
        let content = if output.is_empty() {
            error.unwrap_or("子代理未返回正文")
        } else {
            output
        };
        let _ = self.sender.send(RuntimeExchange::new(
            &self.exchange_id,
            participant,
            label,
            "main",
            "主脑",
            ExchangeKind::Delegation,
            ExchangePhase::Response,
            format!("{} · 最终结果", self.request.description),
            content,
            if failed {
                ExchangeStatus::Failed
            } else {
                ExchangeStatus::Completed
            },
            Some(duration_ms),
        ));
    }
}

impl ToolExecutor for RealToolExecutor {
    fn execute(
        &self,
        tool_call: &ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + '_>> {
        let name = tool_call.tool_name.clone();
        let mut input = tool_call.input.clone();
        let tool_name_owned = tool_call.tool_name.clone();
        if is_graph_tool(&name) {
            inject_graph_db_path(&mut input, self.graph_db_path.as_ref());
        }

        if matches!(
            name.as_str(),
            "novel_create_project"
                | "novel_list_projects"
                | "novel_recall_project"
                | "novel_check_consistency"
                | "novel_commit_delta"
                | "novel_resolve_conflict"
        ) {
            let memory_brain = self.memory_brain.clone();
            return Box::pin(async move {
                let start = std::time::Instant::now();
                let Some(memory_brain) = memory_brain else {
                    return ToolExecutionResult {
                        tool_name: tool_name_owned,
                        output: format!("{name}: PyramidMemoryBrain not available"),
                        is_error: true,
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                };
                let guard = memory_brain.lock().await;
                let result = execute_novel_memory_tool(&guard, &name, &input);
                match result {
                    Ok(output) => ToolExecutionResult {
                        tool_name: tool_name_owned,
                        output,
                        is_error: false,
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                    Err(error) => ToolExecutionResult {
                        tool_name: tool_name_owned,
                        output: error,
                        is_error: true,
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                }
            });
        }

        // special-case: search_memory 由 PyramidMemoryBrain 处理
        if name == "search_memory" {
            let memory_brain = self.memory_brain.clone();
            return Box::pin(async move {
                let start = std::time::Instant::now();
                let mem: Arc<tokio::sync::Mutex<PyramidMemoryBrain>> = match memory_brain {
                    Some(m) => m,
                    None => {
                        return ToolExecutionResult {
                            tool_name: tool_name_owned,
                            output: "search_memory: PyramidMemoryBrain not available".into(),
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

        // special-case: list_recent_memories 由 PyramidMemoryBrain 处理
        if name == "list_recent_memories" {
            let memory_brain = self.memory_brain.clone();
            return Box::pin(async move {
                let start = std::time::Instant::now();
                let mem: Arc<tokio::sync::Mutex<PyramidMemoryBrain>> = match memory_brain {
                    Some(m) => m,
                    None => {
                        return ToolExecutionResult {
                            tool_name: tool_name_owned,
                            output: "list_recent_memories: PyramidMemoryBrain not available".into(),
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

        // Skill 工具 → SkillCatalog
        if name == "Skill" {
            let catalog = self.skill_catalog.clone();
            let n = name;
            let inp = input;
            return Box::pin(async move {
                if let Some(ref catalog) = catalog {
                    let skill_name = inp.get("skill").and_then(|v| v.as_str()).unwrap_or("");
                    match catalog.resolve(skill_name) {
                        Some(meta) => match catalog.load_content(meta) {
                            Ok(content) => ToolExecutionResult {
                                tool_name: n,
                                output: content,
                                is_error: false,
                                duration_ms: 0,
                            },
                            Err(e) => ToolExecutionResult {
                                tool_name: n,
                                output: format!("加载技能失败: {e}"),
                                is_error: true,
                                duration_ms: 0,
                            },
                        },
                        None => ToolExecutionResult {
                            tool_name: n,
                            output: format!("未知技能: {skill_name}"),
                            is_error: true,
                            duration_ms: 0,
                        },
                    }
                } else {
                    // 没有 SkillCatalog 时走旧的 tools::execute_tool 路径
                    let n_clone = n.clone();
                    let result =
                        tokio::task::spawn_blocking(move || tools::execute_tool(&n_clone, &inp))
                            .await
                            .unwrap_or_else(|e| Err(format!("工具执行 panic: {e}")));
                    match result {
                        Ok(output) => ToolExecutionResult {
                            tool_name: n.clone(),
                            output,
                            is_error: false,
                            duration_ms: 0,
                        },
                        Err(e) => ToolExecutionResult {
                            tool_name: n,
                            output: e,
                            is_error: true,
                            duration_ms: 0,
                        },
                    }
                }
            });
        }

        // MCP 工具 → McpClientPool (mcp__server__tool)
        if name.starts_with("mcp__") {
            let mcp_pool = self.mcp_pool.clone();
            let call = ToolCall {
                tool_name: name,
                input,
                validated: tool_call.validated,
                validation_id: tool_call.validation_id.clone(),
            };
            return Box::pin(async move {
                if let Some(pool) = mcp_pool {
                    pool.execute(&call).await
                } else {
                    ToolExecutionResult {
                        tool_name: call.tool_name,
                        output: "MCP 系统未初始化".to_string(),
                        is_error: true,
                        duration_ms: 0,
                    }
                }
            });
        }

        if name == "Agent" {
            let dispatch = self.dispatch.clone();
            let trace = self
                .runtime_trace_tx
                .clone()
                .map(|sender| AgentTracePublisher {
                    sender,
                    request: AgentTraceRequest::from_input(&input),
                    exchange_id: format!("delegation-{}", Uuid::new_v4()),
                });
            return Box::pin(async move {
                let start = std::time::Instant::now();
                if let Some(trace) = &trace {
                    trace.publish_request();
                }
                let result = tokio::task::spawn_blocking(move || {
                    tools::execute_agent_tool_with_completion(&input)
                })
                .await
                .unwrap_or_else(|e| Err(format!("工具执行 panic: {e}")));

                match result {
                    Ok(launch) => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        let manifest =
                            serde_json::from_str::<serde_json::Value>(&launch.output_json)
                                .unwrap_or(serde_json::Value::Null);
                        let agent_id = manifest
                            .get("agentId")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown-agent");
                        let agent_name = manifest
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        if let Some(completion_rx) = launch.completion_rx {
                            spawn_agent_completion_notifier(dispatch, completion_rx, trace);
                        } else if let Some(trace) = &trace {
                            let status = manifest
                                .get("status")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("completed");
                            let output = manifest
                                .get("result")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default();
                            let error = manifest.get("error").and_then(serde_json::Value::as_str);
                            trace.publish_response(
                                agent_id,
                                agent_name,
                                status,
                                output,
                                error,
                                duration_ms,
                            );
                        }
                        ToolExecutionResult {
                            tool_name: tool_name_owned,
                            output: launch.output_json,
                            is_error: false,
                            duration_ms,
                        }
                    }
                    Err(e) => {
                        let duration_ms = start.elapsed().as_millis() as u64;
                        if let Some(trace) = &trace {
                            trace.publish_response(
                                "unknown-agent",
                                "",
                                "failed",
                                "",
                                Some(&e),
                                duration_ms,
                            );
                        }
                        ToolExecutionResult {
                            tool_name: tool_name_owned,
                            output: e,
                            is_error: true,
                            duration_ms,
                        }
                    }
                }
            });
        }

        Box::pin(async move {
            let start = std::time::Instant::now();

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

fn execute_novel_memory_tool(
    memory: &PyramidMemoryBrain,
    name: &str,
    input: &serde_json::Value,
) -> Result<String, String> {
    match name {
        "novel_create_project" => {
            let project_id = required_string(input, "project_id")?;
            let title = required_string(input, "title")?;
            let mut project = NovelProject::new(project_id, title);
            project.genres = input
                .get("genres")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            project.target_platform = input
                .get("target_platform")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            memory
                .create_novel_project(&project)
                .map_err(|error| error.to_string())?;
            serde_json::to_string_pretty(&project).map_err(|error| error.to_string())
        }
        "novel_list_projects" => {
            let projects = memory
                .novel_memory_store()
                .list_projects()
                .map_err(|error| error.to_string())?;
            serde_json::to_string_pretty(&projects).map_err(|error| error.to_string())
        }
        "novel_recall_project" => {
            let project_id = required_string(input, "project_id")?;
            let task_type: NovelTaskType = serde_json::from_value(
                input
                    .get("task_type")
                    .cloned()
                    .ok_or_else(|| "缺少 task_type".to_string())?,
            )
            .map_err(|error| format!("无效 task_type: {error}"))?;
            let pack = memory
                .recall_novel_project(project_id, task_type)
                .map_err(|error| error.to_string())?;
            serde_json::to_string_pretty(&pack).map_err(|error| error.to_string())
        }
        "novel_check_consistency" => {
            let report = memory
                .check_novel_consistency(required_string(input, "project_id")?)
                .map_err(|error| error.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())
        }
        "novel_commit_delta" => {
            let delta: NovelMemoryDelta = serde_json::from_value(
                input
                    .get("delta")
                    .cloned()
                    .ok_or_else(|| "缺少 delta".to_string())?,
            )
            .map_err(|error| format!("NovelMemoryDelta 格式错误: {error}"))?;
            let report = memory
                .commit_novel_delta(&delta)
                .map_err(|error| error.to_string())?;
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())
        }
        "novel_resolve_conflict" => {
            memory
                .resolve_novel_conflict(
                    required_string(input, "project_id")?,
                    required_string(input, "conflict_id")?,
                    required_string(input, "resolution")?,
                )
                .map_err(|error| error.to_string())?;
            Ok("小说 Canon 冲突已标记为已处理".into())
        }
        _ => Err(format!("unsupported novel memory tool: {name}")),
    }
}

fn required_string<'a>(input: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("缺少或无效字段: {key}"))
}

fn is_graph_tool(name: &str) -> bool {
    matches!(
        name,
        "graph_search_catalog"
            | "graph_get_node_detail"
            | "graph_trace_memory"
            | "graph_list_domains"
            | "graph_add_memory"
            | "graph_add_concept"
            | "graph_add_code_node"
            | "graph_index_code_workspace"
            | "graph_link_nodes"
    )
}

fn inject_graph_db_path(input: &mut serde_json::Value, graph_db_path: Option<&PathBuf>) {
    let Some(path) = graph_db_path else {
        return;
    };
    let serde_json::Value::Object(object) = input else {
        return;
    };
    if object
        .get("db_path")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.trim().is_empty())
    {
        return;
    }
    object.insert(
        "db_path".into(),
        serde_json::Value::String(path.display().to_string()),
    );
}

fn default_graph_db_path() -> Option<PathBuf> {
    std::env::var_os("AI_BRAIN_GRAPH_DB")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| {
                    PathBuf::from(home)
                        .join(".ai-brain")
                        .join("graph")
                        .join("graph.db")
                })
        })
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
    use brain_graph::{
        id::gen_node_id,
        schema::{GraphType, Node, NodeKind},
        store::GraphStore,
    };
    use serde_json::json;
    use std::collections::HashMap;

    fn graph_node(title: &str) -> Node {
        let now = chrono::Utc::now().timestamp_millis();
        Node {
            id: gen_node_id(GraphType::Memory, NodeKind::Memory),
            kind: NodeKind::Memory,
            graph_type: GraphType::Memory,
            props: HashMap::from([("catalog_title".into(), json!(title))]),
            importance: 0.8,
            created_at: now,
            last_accessed: now,
            superseded: false,
        }
    }

    #[test]
    fn real_executor_lists_mvp_tools() {
        let exec = RealToolExecutor::new();
        let tools = exec.list_tools();
        assert!(!tools.is_empty(), "MVP 工具列表不应为空");
        // 检查核心工具存在
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"bash"), "应包含 bash 工具");
        assert!(names.contains(&"read_file"), "应包含 read_file 工具");
        assert!(names.contains(&"novel_create_project"));
        assert!(names.contains(&"novel_list_projects"));
        assert!(names.contains(&"novel_recall_project"));
        assert!(names.contains(&"novel_check_consistency"));
        assert!(names.contains(&"novel_commit_delta"));
        assert!(names.contains(&"novel_resolve_conflict"));
    }

    #[tokio::test]
    async fn novel_project_tools_create_and_recall_isolated_memory() {
        let dir = tempfile::tempdir().unwrap();
        let memory = PyramidMemoryBrain::new(
            brain_memory::pyramid_memory_brain::PyramidMemoryBrainConfig {
                base_dir: dir.path().to_path_buf(),
                session_id: "novel-tool-test".into(),
                graph_db_path: Some(dir.path().join("graph.db")),
            },
        )
        .unwrap();
        let exec = RealToolExecutor::with_memory(Some(Arc::new(tokio::sync::Mutex::new(memory))));

        let create = exec
            .execute(&ToolCall {
                tool_name: "novel_create_project".into(),
                input: json!({
                    "project_id": "dark-city",
                    "title": "暗城",
                    "genres": ["悬疑"],
                    "target_platform": "起点"
                }),
                validated: false,
                validation_id: None,
            })
            .await;
        assert!(!create.is_error, "{}", create.output);

        let recall = exec
            .execute(&ToolCall {
                tool_name: "novel_recall_project".into(),
                input: json!({ "project_id": "dark-city", "task_type": "outline" }),
                validated: false,
                validation_id: None,
            })
            .await;
        assert!(!recall.is_error, "{}", recall.output);
        assert!(recall.output.contains("dark-city"));
        assert!(recall.output.contains("暗城"));
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

    #[tokio::test]
    async fn real_executor_injects_graph_db_path() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("graph.db");
        let store = GraphStore::open(&db_path).unwrap();
        store
            .upsert_node(&graph_node("红冲资费生成逻辑解释"))
            .unwrap();

        let exec = RealToolExecutor::new().with_graph_db_path(db_path);
        let call = ToolCall {
            tool_name: "graph_list_domains".into(),
            input: json!({}),
            validated: false,
            validation_id: None,
        };

        let result = exec.execute(&call).await;

        assert!(
            !result.is_error,
            "graph_list_domains should run: {}",
            result.output
        );
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output.as_array().unwrap()[0]["node_count"], 1);
    }

    #[test]
    fn mvp_tool_definitions_not_empty() {
        let defs = mvp_tool_definitions();
        assert!(!defs.is_empty());
    }

    #[test]
    fn agent_trace_pairs_full_request_and_response() {
        let (sender, mut receiver) = tokio::sync::broadcast::channel(4);
        let trace = AgentTracePublisher {
            sender,
            request: AgentTraceRequest {
                description: "续写第三章".into(),
                prompt: "这是交给小说脑的完整任务正文".into(),
                subagent_type: "Novel".into(),
                requested_name: Some("chapter-writer".into()),
            },
            exchange_id: "delegation-test".into(),
        };

        trace.publish_request();
        trace.publish_response(
            "agent-1",
            "chapter-writer",
            "completed",
            "这是小说脑返回的完整最终结果",
            None,
            42,
        );

        let request = receiver.try_recv().unwrap();
        let response = receiver.try_recv().unwrap();
        assert_eq!(request.exchange_id, response.exchange_id);
        assert_eq!(request.content, "这是交给小说脑的完整任务正文");
        assert_eq!(response.content, "这是小说脑返回的完整最终结果");
        assert_eq!(request.phase, ExchangePhase::Request);
        assert_eq!(response.phase, ExchangePhase::Response);
        assert!(request.receiver.starts_with("novel:"));
    }

    #[tokio::test]
    async fn agent_completion_notifier_waits_for_real_completion() {
        let dispatch = brain_dispatch::TokioDispatch::new(8);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(4);
        let dispatch_loop = {
            let dispatch = dispatch.clone();
            tokio::spawn(async move {
                dispatch.run_dispatch_loop(output_tx).await;
            })
        };

        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        spawn_agent_completion_notifier(Some(dispatch.clone()), completion_rx, None);

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), output_rx.recv())
                .await
                .is_err(),
            "通知不应在子代理真实完成前出现"
        );

        completion_tx
            .send(tools::AgentCompletion {
                agent_id: "agent-real-done".into(),
                name: "real-done".into(),
                status: "completed".into(),
                output: "最终结果".into(),
                error: None,
                duration_ms: 42,
            })
            .expect("send completion");

        let msg = tokio::time::timeout(std::time::Duration::from_secs(1), output_rx.recv())
            .await
            .expect("completion notification should arrive")
            .expect("dispatch output channel open");
        match msg {
            brain_dispatch::MainLoopMessage::AgentNotification(result) => {
                assert_eq!(result.agent_id, "agent-real-done");
                assert_eq!(result.status, brain_dispatch::AgentStatus::Completed);
                assert_eq!(result.output, "最终结果");
                assert_eq!(result.duration_ms, 42);
            }
            brain_dispatch::MainLoopMessage::BrainTaskNotification { .. } => {
                panic!("expected AgentNotification");
            }
        }

        dispatch.shutdown().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), dispatch_loop).await;
    }
}
