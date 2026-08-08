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
use brain_memory::pyramid_memory_brain::PyramidMemoryBrain;
use brain_plugin::SkillCatalog;
use novel_application::{NovelApplicationError, TaskApplicationPort};
use novel_domain::{
    MainReviewRecord, NovelConversationSource, NovelProject, NovelResumeInput, NovelTaskRequest,
    NovelTaskType, UserDecisionRecord,
};
use serde::Deserialize;
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
    /// One high-level application boundary for every Novel task command.
    novel_application: Option<Arc<dyn TaskApplicationPort>>,
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
            novel_application: None,
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

    pub fn with_novel_application(
        mut self,
        application: Option<Arc<dyn TaskApplicationPort>>,
    ) -> Self {
        self.novel_application = application;
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
    completion_rx: tokio::sync::oneshot::Receiver<tools::AgentCompletion>,
    trace: Option<AgentTracePublisher>,
) {
    tokio::spawn(async move {
        let Ok(completion) = completion_rx.await else {
            tracing::warn!("后台子代理完成通道提前关闭，无法注入通知");
            return;
        };
        let status = match completion.status.as_str() {
            "completed" => brain_dispatch::AgentStatus::Completed,
            "failed" | "cancelled" | "deadline_exceeded" => brain_dispatch::AgentStatus::Failed,
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
        let prompt = input
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Self {
            description: input
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("子代理任务")
                .to_string(),
            prompt,
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
        let name = if actual_name.trim().is_empty() {
            self.request
                .requested_name
                .as_deref()
                .unwrap_or(&self.request.subagent_type)
        } else {
            actual_name
        };
        (
            format!("agent:{}", self.exchange_id),
            format!("子代理 · {name}"),
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
        if name == "novel_task"
            && input.get("action").and_then(serde_json::Value::as_str) == Some("start")
        {
            inject_novel_conversation_scope(&mut input);
        }

        if is_novel_application_tool(&name) {
            if let Err(error) = preflight_novel_application_tool(&name, &input) {
                return Box::pin(async move {
                    ToolExecutionResult {
                        tool_name: tool_name_owned,
                        output: error,
                        is_error: true,
                        duration_ms: 0,
                    }
                });
            }
            let novel_application = self.novel_application.clone();
            let trace = self.runtime_trace_tx.clone();
            let trace_input = redact_novel_trace_input(&name, &input);
            let novel_action = input
                .get("action")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            return Box::pin(async move {
                let start = std::time::Instant::now();
                let exchange_id = format!("novel-{}", Uuid::new_v4());
                let (novel_participant, novel_label) = ("novel-application", "小说任务工作流");
                if let Some(sender) = &trace {
                    let _ = sender.send(RuntimeExchange::new(
                        &exchange_id,
                        "main",
                        "主脑",
                        novel_participant,
                        novel_label,
                        ExchangeKind::Delegation,
                        ExchangePhase::Request,
                        &name,
                        serde_json::to_string_pretty(&trace_input)
                            .unwrap_or_else(|_| trace_input.to_string()),
                        ExchangeStatus::Running,
                        None,
                    ));
                }
                let result = match novel_application {
                    Some(application) => {
                        execute_application_novel_tool(application.as_ref(), &name, input).await
                    }
                    None => Err("小说任务应用服务未初始化".into()),
                };
                let duration_ms = start.elapsed().as_millis() as u64;
                if let Some(sender) = &trace {
                    let (content, status) = match &result {
                        Ok(output) => (output, ExchangeStatus::Completed),
                        Err(error) => (error, ExchangeStatus::Failed),
                    };
                    let trace_content =
                        novel_trace_response_content(novel_action.as_deref(), content, status);
                    let _ = sender.send(RuntimeExchange::new(
                        &exchange_id,
                        novel_participant,
                        novel_label,
                        "main",
                        "主脑",
                        ExchangeKind::Delegation,
                        ExchangePhase::Response,
                        &name,
                        trace_content,
                        status,
                        Some(duration_ms),
                    ));
                }
                match result {
                    Ok(output) => ToolExecutionResult {
                        tool_name: tool_name_owned,
                        output,
                        is_error: false,
                        duration_ms,
                    },
                    Err(error) => ToolExecutionResult {
                        tool_name: tool_name_owned,
                        output: error,
                        is_error: true,
                        duration_ms,
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
                let result = tools::execute_agent_tool_with_completion(&input).await;

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

fn inject_novel_conversation_scope(input: &mut serde_json::Value) {
    let (Some(scope), Some(object)) = (
        crate::query_context::current_conversation_memory_scope(),
        input.as_object_mut(),
    ) else {
        return;
    };
    object.insert(
        "source_conversation_id".into(),
        serde_json::Value::String(scope.conversation_id),
    );
    object.insert(
        "source_generation_id".into(),
        serde_json::Value::String(scope.generation_id),
    );
}

#[derive(Deserialize)]
struct NovelResumeToolInput {
    task_id: String,
    input: String,
    #[serde(default)]
    context_refs: Option<Vec<novel_domain::ContextRef>>,
}

#[derive(Deserialize)]
struct NovelPublishToolInput {
    task_id: String,
    draft_version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NovelUnlockFailedToolInput {
    task_id: String,
    reason: String,
}

const NOVEL_UNLOCK_REASON_TRACE_PLACEHOLDER: &str = "【已脱敏：人工解锁原因】";

fn preflight_novel_application_tool(name: &str, input: &serde_json::Value) -> Result<(), String> {
    let action = required_string(input, "action")?;
    let mut action_input = input.clone();
    action_input
        .as_object_mut()
        .ok_or_else(|| "Novel facade input must be an object".to_string())?
        .remove("action");
    validate_novel_action_input(name, action, &action_input)
}

fn redact_novel_trace_input(name: &str, input: &serde_json::Value) -> serde_json::Value {
    let mut trace_input = input.clone();
    if name == "novel_task"
        && input.get("action").and_then(serde_json::Value::as_str) == Some("unlock_failed")
    {
        if let Some(object) = trace_input.as_object_mut() {
            object.insert(
                "reason".into(),
                serde_json::Value::String(NOVEL_UNLOCK_REASON_TRACE_PLACEHOLDER.into()),
            );
        }
    }
    trace_input
}

fn novel_trace_response_content(
    action: Option<&str>,
    content: &str,
    status: ExchangeStatus,
) -> String {
    if action != Some("unlock_failed") {
        return content.to_owned();
    }
    let status = match status {
        ExchangeStatus::Completed => "completed",
        ExchangeStatus::Failed => "failed",
        ExchangeStatus::Running => "running",
        ExchangeStatus::Empty => "empty",
    };
    serde_json::json!({
        "action": "unlock_failed",
        "status": status,
        "details": "[响应已脱敏]",
    })
    .to_string()
}

async fn execute_application_novel_tool(
    application: &dyn TaskApplicationPort,
    name: &str,
    mut input: serde_json::Value,
) -> Result<String, String> {
    let action = required_string(&input, "action")?.to_owned();
    input
        .as_object_mut()
        .ok_or_else(|| "Novel facade input must be an object".to_string())?
        .remove("action");
    validate_novel_action_input(name, &action, &input)?;
    if name == "novel_task" && matches!(action.as_str(), "resume" | "review" | "decide" | "publish")
    {
        if let (Some(scope), Some(task_id)) = (
            crate::query_context::current_conversation_memory_scope(),
            input.get("task_id").and_then(serde_json::Value::as_str),
        ) {
            application
                .associate_conversation_source(
                    task_id,
                    NovelConversationSource {
                        conversation_id: scope.conversation_id,
                        generation_id: scope.generation_id,
                    },
                )
                .await
                .map_err(novel_error_mapper(&action))?;
        }
    }
    let output = match (name, action.as_str()) {
        ("novel_task", "resume") => {
            let input: NovelResumeToolInput = serde_json::from_value(input)
                .map_err(|error| format!("novel_task resume 格式错误: {error}"))?;
            serde_json::to_value(
                application
                    .resume_task(
                        &input.task_id,
                        NovelResumeInput {
                            input: input.input,
                            context_refs: input.context_refs,
                        },
                    )
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_task", "review") => {
            let review: MainReviewRecord = serde_json::from_value(input)
                .map_err(|error| format!("MainReviewRecord 格式错误: {error}"))?;
            serde_json::to_value(
                application
                    .review_draft(review)
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_task", "decide") => {
            let decision: UserDecisionRecord = serde_json::from_value(input)
                .map_err(|error| format!("UserDecisionRecord 格式错误: {error}"))?;
            serde_json::to_value(
                application
                    .user_decision(decision)
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_task", "publish") => {
            let input: NovelPublishToolInput = serde_json::from_value(input)
                .map_err(|error| format!("novel_task publish 格式错误: {error}"))?;
            serde_json::to_value(
                application
                    .publish(&input.task_id, input.draft_version)
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_task", "unlock_failed") => {
            let input: NovelUnlockFailedToolInput =
                serde_json::from_value(input).map_err(|_| invalid_novel_unlock_format_error())?;
            serde_json::to_value(
                application
                    .unlock_failed_task(&input.task_id, &input.reason)
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_task", "status") => {
            let project_id = input
                .get("project_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            serde_json::to_value(
                application
                    .status(project_id.as_deref())
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_task", "start") => {
            let request: NovelTaskRequest = serde_json::from_value(input)
                .map_err(|error| format!("NovelTaskRequest 格式错误: {error}"))?;
            serde_json::to_value(
                application
                    .start_task(request)
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_project", "create") => {
            let project_id = required_string(&input, "project_id")?;
            let title = required_string(&input, "title")?;
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
            serde_json::to_value(
                application
                    .create_project(project)
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_project", "list") => serde_json::to_value(
            application
                .list_projects()
                .await
                .map_err(novel_error_mapper(&action))?,
        ),
        ("novel_project", "recall") => {
            let project_id = required_string(&input, "project_id")?;
            let task_type: NovelTaskType = serde_json::from_value(
                input
                    .get("task_type")
                    .cloned()
                    .ok_or_else(|| "缺少 task_type".to_string())?,
            )
            .map_err(|error| format!("无效 task_type: {error}"))?;
            serde_json::to_value(
                application
                    .recall_project(project_id, task_type)
                    .await
                    .map_err(novel_error_mapper(&action))?,
            )
        }
        ("novel_project", "consistency") => serde_json::to_value(
            application
                .check_consistency(required_string(&input, "project_id")?)
                .await
                .map_err(novel_error_mapper(&action))?,
        ),
        ("novel_project", "resolve_conflict") => serde_json::to_value(
            application
                .resolve_conflict(
                    required_string(&input, "project_id")?,
                    required_string(&input, "conflict_id")?,
                    required_string(&input, "resolution")?,
                )
                .await
                .map_err(novel_error_mapper(&action))?,
        ),
        _ => {
            return Err(format!(
                "unsupported Novel application command: {name} action={action}"
            ))
        }
    }
    .map_err(|error| error.to_string())?;
    serde_json::to_string_pretty(&output).map_err(|error| error.to_string())
}

fn validate_novel_action_input(
    name: &str,
    action: &str,
    input: &serde_json::Value,
) -> Result<(), String> {
    if name != "novel_task" {
        return Ok(());
    }
    if matches!(action, "resume" | "review" | "decide" | "publish") {
        required_string(input, "task_id")?;
    }
    match action {
        "resume" => {
            if required_string(input, "input").is_err() {
                return Err(
                    "novel_task resume 缺少或无效字段 input；仅当上一次结果为 needs_clarification 时，使用同一 task_id 并提供非空 input 后重试一次"
                        .into(),
                );
            }
            serde_json::from_value::<NovelResumeToolInput>(input.clone())
                .map(|_| ())
                .map_err(|error| format!("novel_task resume 格式错误: {error}"))
        }
        "review" => serde_json::from_value::<MainReviewRecord>(input.clone())
            .map(|_| ())
            .map_err(|error| format!("MainReviewRecord 格式错误: {error}")),
        "decide" => serde_json::from_value::<UserDecisionRecord>(input.clone())
            .map(|_| ())
            .map_err(|error| format!("UserDecisionRecord 格式错误: {error}")),
        "publish" => serde_json::from_value::<NovelPublishToolInput>(input.clone())
            .map(|_| ())
            .map_err(|error| format!("novel_task publish 格式错误: {error}")),
        "unlock_failed" => {
            let task_id = input
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(invalid_novel_unlock_task_id_error)?;
            if task_id.len() > 128
                || task_id.is_empty()
                || !task_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(invalid_novel_unlock_task_id_error());
            }
            let reason = required_string(input, "reason")?;
            if reason.chars().count() > 256 {
                return Err("novel_task unlock_failed 的 reason 超过 256 个字符".into());
            }
            if reason.chars().any(char::is_control) {
                return Err("novel_task unlock_failed 的 reason 包含不允许的控制字符".into());
            }
            serde_json::from_value::<NovelUnlockFailedToolInput>(input.clone())
                .map(|_| ())
                .map_err(|_| invalid_novel_unlock_format_error())
        }
        "start" => serde_json::from_value::<NovelTaskRequest>(input.clone())
            .map(|_| ())
            .map_err(|error| format!("NovelTaskRequest 格式错误: {error}")),
        "status" => Ok(()),
        other => Err(format!(
            "unsupported Novel application command: {name} action={other}"
        )),
    }
}

fn invalid_novel_unlock_task_id_error() -> String {
    "novel_task unlock_failed 的 task_id 无效；必须为 1 到 128 个 ASCII 字母、数字、下划线或连字符"
        .into()
}

fn invalid_novel_unlock_format_error() -> String {
    "novel_task unlock_failed 格式错误；仅允许 task_id 和 reason 字段".into()
}

fn novel_error_mapper(action: &str) -> impl FnOnce(NovelApplicationError) -> String + '_ {
    move |error| map_novel_application_error(error, action)
}

fn map_novel_application_error(error: NovelApplicationError, action: &str) -> String {
    match error {
        NovelApplicationError::ContextHashChanged {
            path,
            expected,
            actual,
        } if action == "start" => format!(
            "Novel resource content hash changed: path={path}, expected={expected}, actual={actual}；请重新读取该资源，在原 start 请求的同 role/path ContextRef 中使用 actual hash，保持同一 task_id，并仅重试 start 一次"
        ),
        NovelApplicationError::ContextHashChanged {
            path,
            expected,
            actual,
        } if action == "resume" => format!(
            "Novel resource content hash changed: path={path}, expected={expected}, actual={actual}；仅当任务仍为 needs_clarification 时，保持同一 task_id 再调用 resume 一次，同时提供完整 context_refs，保持原 role/path/顺序且只把对应 sha256 更新为 actual"
        ),
        NovelApplicationError::ContextHashChanged {
            path,
            expected,
            actual,
        } => format!(
            "Novel resource content hash changed: path={path}, expected={expected}, actual={actual}；当前 action={action} 不支持自动刷新上下文，已停止且未重试"
        ),
        NovelApplicationError::ContextChanged(message) => format!(
            "Novel resource content changed: {message}；当前 action={action} 已停止，不自动重试"
        ),
        other => other.to_string(),
    }
}

fn required_string<'a>(input: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("缺少或无效字段: {key}"))
}

fn is_novel_application_tool(name: &str) -> bool {
    matches!(name, "novel_task" | "novel_project")
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
    use async_trait::async_trait;
    use brain_graph::{
        id::gen_node_id,
        schema::{GraphType, Node, NodeKind},
        store::GraphStore,
    };
    use novel_application::{
        NovelApplicationError, NovelApplicationService, NovelApplicationStatus, NovelDomainStore,
        NovelResourcePort as ApplicationResourcePort, StoreWorkflowEnvironment,
    };
    use novel_domain::{
        ConsistencyReport, NovelDraftEnvelope, NovelMemoryDelta, NovelOutcome, NovelRecallPack,
        NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict, NovelTaskPhase,
        NovelTransition, PublicationReceipt, ReviewCheckStatus,
    };
    use novel_workflow::{
        NovelStartWorkflow, NovelTaskExecutionState, NovelWorkflowBudget, NovelWorkflowModels,
        NovelWorkflowPortError, NovelWriterExecution, NovelWriterInvocation, NovelWriterPort,
        ProfileModel,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use task_engine::{
        ActualUsage, Scheduler, SchedulerLimits, TaskCoordinator, TaskRepository, TaskRunState,
    };

    use crate::novel_adapters::ScopedNovelResourceAdapter;

    struct ExecutorTestWriter {
        calls: AtomicUsize,
    }

    #[derive(Default)]
    struct RecordingTaskApplication {
        association_calls: AtomicUsize,
        unlock_calls: AtomicUsize,
        unlock_arguments: Mutex<Option<(String, String)>>,
        unlock_error: Option<String>,
        unlock_receipt_reason: Option<String>,
        start_context_error: Option<String>,
    }

    #[async_trait]
    impl TaskApplicationPort for RecordingTaskApplication {
        async fn create_project(
            &self,
            _project: NovelProject,
        ) -> novel_application::Result<NovelProject> {
            panic!("unexpected create_project")
        }

        async fn list_projects(&self) -> novel_application::Result<Vec<NovelProject>> {
            panic!("unexpected list_projects")
        }

        async fn recall_project(
            &self,
            _project_id: &str,
            _task_type: NovelTaskType,
        ) -> novel_application::Result<NovelRecallPack> {
            panic!("unexpected recall_project")
        }

        async fn check_consistency(
            &self,
            _project_id: &str,
        ) -> novel_application::Result<ConsistencyReport> {
            panic!("unexpected check_consistency")
        }

        async fn resolve_conflict(
            &self,
            _project_id: &str,
            _conflict_id: &str,
            _resolution: &str,
        ) -> novel_application::Result<NovelProject> {
            panic!("unexpected resolve_conflict")
        }

        async fn start_task(
            &self,
            _request: NovelTaskRequest,
        ) -> novel_application::Result<NovelOutcome> {
            Err(NovelApplicationError::ContextHashChanged {
                path: self
                    .start_context_error
                    .clone()
                    .expect("start_context_error must be configured"),
                expected: "deadbeef".into(),
                actual: "cafebabe".into(),
            })
        }

        async fn resume_task(
            &self,
            _task_id: &str,
            _input: NovelResumeInput,
        ) -> novel_application::Result<NovelOutcome> {
            Err(NovelApplicationError::NotFound("recorded resume".into()))
        }

        async fn unlock_failed_task(
            &self,
            task_id: &str,
            reason: &str,
        ) -> novel_application::Result<novel_application::NovelTaskUnlockReceipt> {
            self.unlock_calls.fetch_add(1, Ordering::SeqCst);
            *self
                .unlock_arguments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((task_id.to_owned(), reason.to_owned()));
            if let Some(error) = &self.unlock_error {
                return Err(NovelApplicationError::Conflict(error.clone()));
            }
            Ok(novel_application::NovelTaskUnlockReceipt {
                task_id: task_id.to_owned(),
                project_id: "project-1".into(),
                previous_phase: NovelTaskPhase::Drafting,
                phase: NovelTaskPhase::Cancelled,
                execution_state: NovelTaskExecutionState::Failed,
                already_unlocked: false,
                reason: self
                    .unlock_receipt_reason
                    .clone()
                    .unwrap_or_else(|| reason.to_owned()),
            })
        }

        async fn review_draft(
            &self,
            _review: MainReviewRecord,
        ) -> novel_application::Result<NovelTransition> {
            Err(NovelApplicationError::NotFound("recorded review".into()))
        }

        async fn user_decision(
            &self,
            _decision: UserDecisionRecord,
        ) -> novel_application::Result<NovelTransition> {
            Err(NovelApplicationError::NotFound("recorded decision".into()))
        }

        async fn publish(
            &self,
            _task_id: &str,
            _draft_version: u32,
        ) -> novel_application::Result<PublicationReceipt> {
            Err(NovelApplicationError::NotFound("recorded publish".into()))
        }

        async fn status(
            &self,
            _project_id: Option<&str>,
        ) -> novel_application::Result<NovelApplicationStatus> {
            Ok(NovelApplicationStatus {
                projects: Vec::new(),
                pending_publications: Vec::new(),
            })
        }

        async fn invalidate_conversation_generations(
            &self,
            _conversation_id: &str,
            _generation_ids: &[String],
            _include_unscoped: bool,
        ) -> novel_application::Result<Vec<String>> {
            panic!("unexpected invalidate_conversation_generations")
        }

        async fn associate_conversation_source(
            &self,
            _task_id: &str,
            _source: NovelConversationSource,
        ) -> novel_application::Result<()> {
            self.association_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl NovelWriterPort for ExecutorTestWriter {
        async fn execute(
            &self,
            invocation: NovelWriterInvocation,
        ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let outcome = NovelOutcome::DraftReady(NovelDraftEnvelope {
                task_id: invocation.request.task_id.clone(),
                draft_version: invocation.next_draft_version,
                project_id: invocation.request.project_id.clone(),
                canon_revision: invocation.request.expected_revision,
                content: "Chapter one from TaskEngine workflow".into(),
                self_review: NovelSelfReview {
                    verdict: NovelSelfReviewVerdict::Pass,
                    checks: NovelSelfReviewChecks {
                        outline_alignment: ReviewCheckStatus::Pass,
                        canon_consistency: ReviewCheckStatus::Pass,
                        character_consistency: ReviewCheckStatus::Pass,
                        timeline_consistency: ReviewCheckStatus::Pass,
                        plot_and_foreshadowing: ReviewCheckStatus::Pass,
                        style_and_repetition: ReviewCheckStatus::Pass,
                    },
                    issues: Vec::new(),
                    unverified_assumptions: Vec::new(),
                    summary: "Self review passed".into(),
                },
                proposed_delta: NovelMemoryDelta {
                    project_id: invocation.request.project_id.clone(),
                    branch_id: invocation.project.active_branch,
                    expected_revision: invocation.request.expected_revision,
                    task_type: invocation.request.task_type,
                    source_ref: invocation
                        .request
                        .output_path
                        .to_string_lossy()
                        .into_owned(),
                    progress: None,
                    proposed_facts: Vec::new(),
                    state_changes: Vec::new(),
                    plot_updates: Vec::new(),
                    foreshadowing_updates: Vec::new(),
                    feedback: Vec::new(),
                    experience_candidates: Vec::new(),
                },
                evidence_refs: vec!["task:requirements".into(), "canon:revision:0".into()],
            });
            Ok(NovelWriterExecution {
                raw_output: serde_json::to_string(&outcome).unwrap(),
                outcome,
                usage: ActualUsage {
                    input_tokens: 21,
                    output_tokens: 34,
                },
            })
        }
    }

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

    #[tokio::test]
    async fn novel_resume_preflight_rejects_missing_input_before_association() {
        let application = RecordingTaskApplication::default();
        let scope = brain_memory::conversation_memory::ConversationMemoryScope::new(
            "chat-1",
            "generation-1",
        )
        .unwrap();

        for input in [
            json!({"action": "resume", "task_id": "task-1"}),
            json!({"action": "resume", "task_id": "task-1", "input": "  "}),
        ] {
            let error = crate::query_context::with_conversation_memory_scope(&scope, async {
                execute_application_novel_tool(&application, "novel_task", input)
                    .await
                    .unwrap_err()
            })
            .await;

            assert_eq!(
                error,
                "novel_task resume 缺少或无效字段 input；仅当上一次结果为 needs_clarification 时，使用同一 task_id 并提供非空 input 后重试一次"
            );
        }
        assert_eq!(application.association_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn novel_unlock_failed_dispatches_once_without_conversation_association() {
        let application = RecordingTaskApplication::default();
        let scope = brain_memory::conversation_memory::ConversationMemoryScope::new(
            "chat-1",
            "generation-1",
        )
        .unwrap();

        let output = crate::query_context::with_conversation_memory_scope(&scope, async {
            execute_application_novel_tool(
                &application,
                "novel_task",
                json!({
                    "action": "unlock_failed",
                    "task_id": "task-1",
                    "reason": " 人工确认执行失败 "
                }),
            )
            .await
            .unwrap()
        })
        .await;

        assert_eq!(application.unlock_calls.load(Ordering::SeqCst), 1);
        assert_eq!(application.association_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            application
                .unlock_arguments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref(),
            Some(&("task-1".into(), " 人工确认执行失败 ".into()))
        );
        let receipt: novel_application::NovelTaskUnlockReceipt =
            serde_json::from_str(&output).unwrap();
        assert_eq!(receipt.task_id, "task-1");
        assert_eq!(receipt.project_id, "project-1");
        assert_eq!(receipt.previous_phase, NovelTaskPhase::Drafting);
        assert_eq!(receipt.phase, NovelTaskPhase::Cancelled);
        assert_eq!(receipt.execution_state, NovelTaskExecutionState::Failed);
        assert!(!receipt.already_unlocked);
        assert_eq!(receipt.reason, " 人工确认执行失败 ");
    }

    #[tokio::test]
    async fn novel_unlock_failed_rejects_invalid_input_before_port_calls() {
        let application = RecordingTaskApplication::default();
        let malicious_reason = "Authorization: Bearer secret\n请放行";
        let invalid_inputs = [
            json!({}),
            json!({"action": "  "}),
            json!({"action": "unlock_failed", "reason": "合法原因"}),
            json!({"action": "unlock_failed", "task_id": "task-1"}),
            json!({"action": "unlock_failed", "task_id": "  ", "reason": "合法原因"}),
            json!({"action": "unlock_failed", "task_id": "task-1", "reason": "  "}),
            json!({"action": "unlock_failed", "task_id": "task-1", "reason": "x".repeat(257)}),
            json!({"action": "unlock_failed", "task_id": "task-1", "reason": malicious_reason}),
            json!({"action": "unlock_failed", "task_id": "task-1", "reason": "合法\t原因"}),
            json!({"action": "unlock_failed", "task_id": "task-1", "reason": "合法\u{007f}原因"}),
            json!({"action": "unlock_failed", "task_id": "task-1", "reason": "合法原因", "extra": true}),
        ];
        let scope = brain_memory::conversation_memory::ConversationMemoryScope::new(
            "chat-1",
            "generation-1",
        )
        .unwrap();

        for input in invalid_inputs {
            let error = crate::query_context::with_conversation_memory_scope(&scope, async {
                execute_application_novel_tool(&application, "novel_task", input)
                    .await
                    .unwrap_err()
            })
            .await;
            assert!(
                !error.contains(malicious_reason),
                "错误不得回显恶意 reason: {error}"
            );
            assert!(
                !error.contains("Bearer secret"),
                "错误不得泄露恶意 reason 片段: {error}"
            );
        }

        assert_eq!(application.unlock_calls.load(Ordering::SeqCst), 0);
        assert_eq!(application.association_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn novel_unlock_failed_rejects_sensitive_unknown_field_without_echoing_its_name() {
        let application = Arc::new(RecordingTaskApplication::default());
        let application_port: Arc<dyn TaskApplicationPort> = application.clone();
        let (trace_tx, mut trace_rx) = tokio::sync::broadcast::channel(8);
        let executor = RealToolExecutor::new()
            .with_runtime_trace_sender(trace_tx)
            .with_novel_application(Some(application_port));
        let sensitive_field = "Authorization_Bearer_secret";
        let mut input = json!({
            "action": "unlock_failed",
            "task_id": "task-1",
            "reason": "合法原因"
        });
        input
            .as_object_mut()
            .unwrap()
            .insert(sensitive_field.into(), json!(true));

        let result = executor
            .execute(&ToolCall {
                tool_name: "novel_task".into(),
                input,
                validated: false,
                validation_id: None,
            })
            .await;

        assert!(result.is_error);
        assert!(
            !result.output.contains(sensitive_field),
            "错误不得回显未知字段名: {}",
            result.output
        );
        assert_eq!(application.unlock_calls.load(Ordering::SeqCst), 0);
        assert_eq!(application.association_calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            trace_rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn novel_unlock_failed_rejects_unsafe_task_ids_without_echoing_or_port_calls() {
        let application = RecordingTaskApplication::default();
        let scope = brain_memory::conversation_memory::ConversationMemoryScope::new(
            "chat-1",
            "generation-1",
        )
        .unwrap();

        for task_id in [
            " ../x ".to_string(),
            "task\nid".to_string(),
            "任务-1".to_string(),
            "x".repeat(129),
        ] {
            let error = crate::query_context::with_conversation_memory_scope(&scope, async {
                execute_application_novel_tool(
                    &application,
                    "novel_task",
                    json!({"action": "unlock_failed", "task_id": task_id, "reason": "合法原因"}),
                )
                .await
                .unwrap_err()
            })
            .await;
            assert!(!error.contains(&task_id), "错误不得回显 task_id: {error}");
        }

        assert_eq!(application.unlock_calls.load(Ordering::SeqCst), 0);
        assert_eq!(application.association_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn novel_unlock_failed_preflight_blocks_invalid_input_before_runtime_trace() {
        let application = Arc::new(RecordingTaskApplication::default());
        let application_port: Arc<dyn TaskApplicationPort> = application.clone();
        let (trace_tx, mut trace_rx) = tokio::sync::broadcast::channel(8);
        let executor = RealToolExecutor::new()
            .with_runtime_trace_sender(trace_tx)
            .with_novel_application(Some(application_port));

        for reason in [
            "Authorization: Bearer secret\n请放行".to_string(),
            "x".repeat(257),
        ] {
            let result = executor
                .execute(&ToolCall {
                    tool_name: "novel_task".into(),
                    input: json!({
                        "action": "unlock_failed",
                        "task_id": "task-1",
                        "reason": reason,
                    }),
                    validated: false,
                    validation_id: None,
                })
                .await;
            assert!(result.is_error);
            assert!(!result.output.contains(&reason));
        }

        assert_eq!(application.association_calls.load(Ordering::SeqCst), 0);
        assert_eq!(application.unlock_calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            trace_rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn novel_unlock_failed_runtime_trace_redacts_legal_reason_but_port_receives_original() {
        let application = Arc::new(RecordingTaskApplication::default());
        let application_port: Arc<dyn TaskApplicationPort> = application.clone();
        let (trace_tx, mut trace_rx) = tokio::sync::broadcast::channel(8);
        let executor = RealToolExecutor::new()
            .with_runtime_trace_sender(trace_tx)
            .with_novel_application(Some(application_port));
        let reason = " 人工确认执行失败 ";

        let result = executor
            .execute(&ToolCall {
                tool_name: "novel_task".into(),
                input: json!({
                    "action": "unlock_failed",
                    "task_id": "task-1",
                    "reason": reason,
                }),
                validated: false,
                validation_id: None,
            })
            .await;

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains(reason));
        let exchanges = std::iter::from_fn(|| trace_rx.try_recv().ok()).collect::<Vec<_>>();
        let request = exchanges
            .iter()
            .find(|exchange| exchange.phase == ExchangePhase::Request)
            .expect("novel request trace");
        let request_value: serde_json::Value = serde_json::from_str(&request.content).unwrap();
        let mut request_fields = request_value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        request_fields.sort_unstable();
        assert_eq!(request_fields, vec!["action", "reason", "task_id"]);
        assert_eq!(request_value["action"], "unlock_failed");
        assert_eq!(request_value["task_id"], "task-1");
        assert_eq!(request_value["reason"], "【已脱敏：人工解锁原因】");
        assert!(exchanges
            .iter()
            .all(|exchange| !exchange.content.contains(reason)));
        let response = exchanges
            .iter()
            .find(|exchange| exchange.phase == ExchangePhase::Response)
            .expect("novel response trace");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response.content).unwrap(),
            json!({
                "action": "unlock_failed",
                "status": "completed",
                "details": "[响应已脱敏]"
            })
        );
        assert_eq!(application.unlock_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            application
                .unlock_arguments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref(),
            Some(&("task-1".into(), reason.into()))
        );
    }

    #[tokio::test]
    async fn novel_unlock_failed_response_trace_is_fixed_when_application_trims_receipt_reason() {
        let raw_reason = " 原因 ";
        let trimmed_reason = "原因";
        let application = Arc::new(RecordingTaskApplication {
            unlock_receipt_reason: Some(trimmed_reason.into()),
            ..Default::default()
        });
        let application_port: Arc<dyn TaskApplicationPort> = application.clone();
        let (trace_tx, mut trace_rx) = tokio::sync::broadcast::channel(8);
        let executor = RealToolExecutor::new()
            .with_runtime_trace_sender(trace_tx)
            .with_novel_application(Some(application_port));

        let result = executor
            .execute(&ToolCall {
                tool_name: "novel_task".into(),
                input: json!({
                    "action": "unlock_failed",
                    "task_id": "task-1",
                    "reason": raw_reason,
                }),
                validated: false,
                validation_id: None,
            })
            .await;

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains(trimmed_reason));
        let exchanges = std::iter::from_fn(|| trace_rx.try_recv().ok()).collect::<Vec<_>>();
        let response = exchanges
            .iter()
            .find(|exchange| exchange.phase == ExchangePhase::Response)
            .expect("novel response trace");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response.content).unwrap(),
            json!({
                "action": "unlock_failed",
                "status": "completed",
                "details": "[响应已脱敏]"
            })
        );
        assert!(!response.content.contains(raw_reason));
        assert!(!response.content.contains(trimmed_reason));
        assert_eq!(
            application
                .unlock_arguments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref(),
            Some(&("task-1".into(), raw_reason.into()))
        );
    }

    #[tokio::test]
    async fn novel_unlock_failed_runtime_trace_redacts_reason_from_non_json_port_error() {
        let reason = "人工解锁审计原因";
        let application = Arc::new(RecordingTaskApplication {
            unlock_error: Some(format!("端口拒绝：{reason}")),
            ..Default::default()
        });
        let application_port: Arc<dyn TaskApplicationPort> = application.clone();
        let (trace_tx, mut trace_rx) = tokio::sync::broadcast::channel(8);
        let executor = RealToolExecutor::new()
            .with_runtime_trace_sender(trace_tx)
            .with_novel_application(Some(application_port));

        let result = executor
            .execute(&ToolCall {
                tool_name: "novel_task".into(),
                input: json!({
                    "action": "unlock_failed",
                    "task_id": "task-1",
                    "reason": reason,
                }),
                validated: false,
                validation_id: None,
            })
            .await;

        assert!(result.is_error);
        assert!(result.output.contains(reason));
        let exchanges = std::iter::from_fn(|| trace_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(exchanges
            .iter()
            .all(|exchange| !exchange.content.contains(reason)));
        let response = exchanges
            .iter()
            .find(|exchange| exchange.phase == ExchangePhase::Response)
            .expect("novel response trace");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response.content).unwrap(),
            json!({
                "action": "unlock_failed",
                "status": "failed",
                "details": "[响应已脱敏]"
            })
        );
    }

    #[test]
    fn novel_unlock_failed_runtime_trace_is_fixed_for_nested_and_non_json_success() {
        let reason = "嵌套敏感原因";
        let nested_content = json!({
            "outer": {
                "reason": reason,
                "message": format!("拒绝：{reason}")
            }
        })
        .to_string();
        let expected = json!({
            "action": "unlock_failed",
            "status": "completed",
            "details": "[响应已脱敏]"
        });

        for content in [&nested_content, "非 JSON 敏感响应"] {
            let redacted = novel_trace_response_content(
                Some("unlock_failed"),
                content,
                ExchangeStatus::Completed,
            );

            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&redacted).unwrap(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn novel_unlock_failed_conversation_association_uses_exact_action_allowlist() {
        let scope = brain_memory::conversation_memory::ConversationMemoryScope::new(
            "chat-1",
            "generation-1",
        )
        .unwrap();
        let associated_inputs = [
            json!({"action": "resume", "task_id": "task-1", "input": "继续"}),
            json!({
                "action": "review",
                "task_id": "task-1",
                "draft_version": 1,
                "reviewed_canon_revision": 0,
                "verdict": "pass",
                "checks": {
                    "user_requirements": "pass",
                    "outline_alignment": "pass",
                    "canon_consistency": "pass",
                    "character_consistency": "pass",
                    "timeline_consistency": "pass",
                    "plot_and_foreshadowing": "pass",
                    "style_quality": "pass",
                    "pacing_and_hook": "pass"
                },
                "issues": [],
                "evidence_refs": [],
                "summary": "通过"
            }),
            json!({"action": "decide", "task_id": "task-1", "draft_version": 1, "decision": "accept"}),
            json!({"action": "publish", "task_id": "task-1", "draft_version": 1}),
        ];
        for input in associated_inputs {
            let application = RecordingTaskApplication::default();
            let _ = crate::query_context::with_conversation_memory_scope(&scope, async {
                execute_application_novel_tool(&application, "novel_task", input).await
            })
            .await;
            assert_eq!(application.association_calls.load(Ordering::SeqCst), 1);
        }

        let unassociated_inputs = [
            json!({"action": "status"}),
            json!({"action": "unlock_failed", "task_id": "task-1", "reason": "合法原因"}),
            json!({
                "action": "start",
                "task_id": "task-1",
                "project_id": "project-1",
                "task_type": "body",
                "task_brief": "写第一章",
                "expected_revision": 0,
                "output_path": "chapters/0001.md",
                "context_refs": [],
                "must_happen": [],
                "must_not_change": [],
                "acceptance_criteria": ["完成第一章"],
                "allow_web_research": false,
                "publication_policy": "require_user_acceptance"
            }),
        ];
        for input in unassociated_inputs {
            let application = RecordingTaskApplication {
                start_context_error: Some("outline.md".into()),
                ..Default::default()
            };
            let _ = crate::query_context::with_conversation_memory_scope(&scope, async {
                execute_application_novel_tool(&application, "novel_task", input).await
            })
            .await;
            assert_eq!(application.association_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn novel_context_change_error_contains_finite_recovery_instruction() {
        let application = RecordingTaskApplication {
            start_context_error: Some("outline.md".into()),
            ..Default::default()
        };

        let error = execute_application_novel_tool(
            &application,
            "novel_task",
            json!({
                "action": "start",
                "task_id": "task-1",
                "project_id": "project-1",
                "task_type": "body",
                "task_brief": "Write chapter one",
                "target_chapter": 1,
                "expected_revision": 0,
                "output_path": "chapters/0001.md",
                "context_refs": [],
                "must_happen": [],
                "must_not_change": [],
                "acceptance_criteria": ["Complete chapter one"],
                "allow_web_research": false,
                "publication_policy": "require_user_acceptance"
            }),
        )
        .await
        .unwrap_err();

        for required in [
            "expected=deadbeef",
            "actual=cafebabe",
            "重新读取该资源",
            "使用 actual hash",
            "保持同一 task_id",
            "仅重试 start 一次",
        ] {
            assert!(
                error.contains(required),
                "error missing {required}: {error}"
            );
        }
    }

    #[test]
    fn context_change_guidance_is_action_specific_and_never_guesses() {
        let resume = map_novel_application_error(
            NovelApplicationError::ContextHashChanged {
                path: "outline.md".into(),
                expected: "old".into(),
                actual: "new".into(),
            },
            "resume",
        );
        for required in [
            "needs_clarification",
            "完整 context_refs",
            "原 role/path/顺序",
            "sha256 更新为 actual",
        ] {
            assert!(
                resume.contains(required),
                "resume error missing {required}: {resume}"
            );
        }

        let publish = map_novel_application_error(
            NovelApplicationError::ContextHashChanged {
                path: "chapter.md".into(),
                expected: "old".into(),
                actual: "new".into(),
            },
            "publish",
        );
        assert!(publish.contains("不支持自动刷新上下文"), "{publish}");
        assert!(publish.contains("未重试"), "{publish}");
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
        assert!(names.contains(&"novel_task"));
        assert!(names.contains(&"novel_project"));
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("novel_"))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn novel_project_facade_create_and_recall_use_domain_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(NovelDomainStore::open(dir.path().join("novel.db")).unwrap());
        let resources: Arc<dyn ApplicationResourcePort> =
            Arc::new(ScopedNovelResourceAdapter::new(dir.path()).unwrap());
        let application: Arc<dyn TaskApplicationPort> =
            Arc::new(NovelApplicationService::new(store, None, resources));
        let exec = RealToolExecutor::new().with_novel_application(Some(application));

        let create = exec
            .execute(&ToolCall {
                tool_name: "novel_project".into(),
                input: json!({
                    "action": "create",
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
                tool_name: "novel_project".into(),
                input: json!({ "action": "recall", "project_id": "dark-city", "task_type": "outline" }),
                validated: false,
                validation_id: None,
            })
            .await;
        assert!(!recall.is_error, "{}", recall.output);
        assert!(recall.output.contains("dark-city"));
        assert!(recall.output.contains("暗城"));
    }

    #[tokio::test]
    async fn ephemeral_novel_agent_is_blocked_by_real_executor() {
        let result = RealToolExecutor::new()
            .execute(&ToolCall {
                tool_name: "Agent".into(),
                input: json!({
                    "description": "写第一章",
                    "prompt": "写正文",
                    "subagent_type": "Novel"
                }),
                validated: false,
                validation_id: None,
            })
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("unsupported built-in agent role"));
    }

    #[tokio::test]
    async fn novel_tool_reports_missing_workflow() {
        let result = RealToolExecutor::new()
            .execute(&ToolCall {
                tool_name: "novel_task".into(),
                input: json!({ "action": "status" }),
                validated: false,
                validation_id: None,
            })
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("未初始化"));
    }

    #[tokio::test]
    async fn novel_start_uses_task_workflow_without_a_resident_handle() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(NovelDomainStore::open(dir.path().join("novel.db")).unwrap());
        store
            .import_project(&NovelProject::new("project-1", "Project One"))
            .unwrap();
        let resources: Arc<dyn ApplicationResourcePort> =
            Arc::new(ScopedNovelResourceAdapter::new(dir.path()).unwrap());
        let environment = Arc::new(StoreWorkflowEnvironment::new(
            Arc::clone(&store),
            Arc::clone(&resources),
        ));
        let repository = Arc::new(TaskRepository::open(dir.path().join("runtime.db")).unwrap());
        let scheduler = Scheduler::new(SchedulerLimits {
            max_workers: 1,
            max_global: 1,
            max_per_room: 1,
            max_per_member: 1,
            max_per_provider: 1,
            max_per_profile: 1,
            max_per_task: 1,
        })
        .unwrap();
        let writer = Arc::new(ExecutorTestWriter {
            calls: AtomicUsize::new(0),
        });
        let model = ProfileModel::new("test-provider", "test-model");
        let workflow = Arc::new(NovelStartWorkflow::new(
            Arc::clone(&repository),
            TaskCoordinator::new(Arc::clone(&repository), scheduler),
            environment,
            writer.clone(),
            NovelWorkflowModels {
                writer: model.clone(),
                reviewer: model.clone(),
                canon_extractor: model,
            },
            NovelWorkflowBudget {
                input_tokens: 1_000,
                output_tokens: 1_000,
            },
        ));
        let application = Arc::new(NovelApplicationService::new(
            Arc::clone(&store),
            Some(Arc::clone(&workflow)),
            resources,
        ));
        let application_port: Arc<dyn TaskApplicationPort> = application;
        let executor = RealToolExecutor::new().with_novel_application(Some(application_port));
        assert!(executor.novel_application.is_some());

        let input = json!({
            "action": "start",
            "task_id": "task-1",
            "project_id": "project-1",
            "task_type": "body",
            "task_brief": "Write chapter one",
            "target_chapter": 1,
            "expected_revision": 0,
            "output_path": "chapters/0001.md",
            "context_refs": [],
            "must_happen": [],
            "must_not_change": [],
            "acceptance_criteria": ["Complete chapter one"],
            "allow_web_research": false,
            "publication_policy": "require_user_acceptance"
        });
        let call = ToolCall {
            tool_name: "novel_task".into(),
            input,
            validated: false,
            validation_id: None,
        };
        let first = executor.execute(&call).await;
        assert!(!first.is_error, "{}", first.output);
        assert!(matches!(
            serde_json::from_str::<NovelOutcome>(&first.output).unwrap(),
            NovelOutcome::DraftReady(_)
        ));
        assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository.task("novel-task-task-1").unwrap().state,
            TaskRunState::Completed
        );

        let replay = executor.execute(&call).await;
        assert!(!replay.is_error, "{}", replay.output);
        assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
        let replay = store.load_checkpoint("task-1").unwrap().unwrap();
        assert_eq!(replay.phase, NovelTaskPhase::AwaitingMainReview);
        assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
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

    #[tokio::test]
    async fn novel_start_input_receives_server_owned_conversation_scope() {
        let scope = brain_memory::conversation_memory::ConversationMemoryScope::new(
            "chat_1",
            "generation_1",
        )
        .unwrap();
        let mut input = json!({
            "task_id": "task-1",
            "source_conversation_id": "forged",
            "source_generation_id": "forged"
        });

        crate::query_context::with_conversation_memory_scope(&scope, async {
            inject_novel_conversation_scope(&mut input);
        })
        .await;

        assert_eq!(input["source_conversation_id"], "chat_1");
        assert_eq!(input["source_generation_id"], "generation_1");
    }

    #[test]
    fn agent_trace_pairs_full_request_and_response() {
        let (sender, mut receiver) = tokio::sync::broadcast::channel(4);
        let trace = AgentTracePublisher {
            sender,
            request: AgentTraceRequest {
                description: "验证模块".into(),
                prompt: "这是交给验证代理的完整任务正文".into(),
                subagent_type: "Verification".into(),
                requested_name: Some("module-verifier".into()),
            },
            exchange_id: "delegation-test".into(),
        };

        trace.publish_request();
        trace.publish_response(
            "agent-1",
            "module-verifier",
            "completed",
            "这是验证代理返回的完整最终结果",
            None,
            42,
        );

        let request = receiver.try_recv().unwrap();
        let response = receiver.try_recv().unwrap();
        assert_eq!(request.exchange_id, response.exchange_id);
        assert_eq!(request.content, "这是交给验证代理的完整任务正文");
        assert_eq!(response.content, "这是验证代理返回的完整最终结果");
        assert_eq!(request.phase, ExchangePhase::Request);
        assert_eq!(response.phase, ExchangePhase::Response);
        assert!(request.receiver.starts_with("agent:"));
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

        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
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
