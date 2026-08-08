use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use agent_runtime::{
    AgentProfileSnapshot, AgentRunOutcome, AgentRunSpec, AgentRunStatus, AgentRuntime,
    AgentRuntimeError, AgentWorkerPool, ArtifactEnvelope, ArtifactSink, BudgetReservation,
    ContextSnapshot, OutputContract, ReasoningPolicy, ResolvedModelPolicy, ToolGrant,
};
use api::{
    max_tokens_for_model, ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest,
    MessageResponse, OutputContentBlock, ProviderClient, StreamEvent as ApiStreamEvent, ToolChoice,
    ToolDefinition, ToolResultContentBlock,
};
use brain_graph::{
    id::gen_node_id,
    schema::{Edge, EdgeKind, GraphType, Node, NodeKind, TraceDirection},
    store::{CatalogQuery, GraphStore, TraceQuery},
};
use plugins::PluginTool;
use reqwest::blocking::Client;
use runtime::{
    edit_file_in_dir, execute_bash_in_dir, glob_search_in_dir, grep_search_in_dir,
    read_file_in_dir, write_file_in_dir, ApiClient, ApiRequest, AssistantEvent, BashCommandInput,
    ContentBlock, ConversationMessage, GrepSearchInput, MessageRole, PermissionMode,
    PermissionPolicy, PromptCacheEvent, RuntimeError, ToolError, ToolExecutor,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolManifestEntry {
    pub name: String,
    pub source: ToolSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    Base,
    Conditional,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRegistry {
    entries: Vec<ToolManifestEntry>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new(entries: Vec<ToolManifestEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[ToolManifestEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub required_permission: PermissionMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlobalToolRegistry {
    plugin_tools: Vec<PluginTool>,
}

impl GlobalToolRegistry {
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            plugin_tools: Vec::new(),
        }
    }

    pub fn with_plugin_tools(plugin_tools: Vec<PluginTool>) -> Result<Self, String> {
        let builtin_names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name.to_string())
            .collect::<BTreeSet<_>>();
        let mut seen_plugin_names = BTreeSet::new();

        for tool in &plugin_tools {
            let name = tool.definition().name.clone();
            if builtin_names.contains(&name) {
                return Err(format!(
                    "plugin tool `{name}` conflicts with a built-in tool name"
                ));
            }
            if !seen_plugin_names.insert(name.clone()) {
                return Err(format!("duplicate plugin tool name `{name}`"));
            }
        }

        Ok(Self { plugin_tools })
    }

    pub fn normalize_allowed_tools(
        &self,
        values: &[String],
    ) -> Result<Option<BTreeSet<String>>, String> {
        if values.is_empty() {
            return Ok(None);
        }

        let builtin_specs = mvp_tool_specs();
        let canonical_names = builtin_specs
            .iter()
            .map(|spec| spec.name.to_string())
            .chain(
                self.plugin_tools
                    .iter()
                    .map(|tool| tool.definition().name.clone()),
            )
            .collect::<Vec<_>>();
        let mut name_map = canonical_names
            .iter()
            .map(|name| (normalize_tool_name(name), name.clone()))
            .collect::<BTreeMap<_, _>>();

        for (alias, canonical) in [
            ("read", "read_file"),
            ("write", "write_file"),
            ("edit", "edit_file"),
            ("glob", "glob_search"),
            ("grep", "grep_search"),
        ] {
            name_map.insert(alias.to_string(), canonical.to_string());
        }

        let mut allowed = BTreeSet::new();
        for value in values {
            for token in value
                .split(|ch: char| ch == ',' || ch.is_whitespace())
                .filter(|token| !token.is_empty())
            {
                let normalized = normalize_tool_name(token);
                let canonical = name_map.get(&normalized).ok_or_else(|| {
                    format!(
                        "unsupported tool in --allowedTools: {token} (expected one of: {})",
                        canonical_names.join(", ")
                    )
                })?;
                allowed.insert(canonical.clone());
            }
        }

        Ok(Some(allowed))
    }

    #[must_use]
    pub fn definitions(&self, allowed_tools: Option<&BTreeSet<String>>) -> Vec<ToolDefinition> {
        let builtin = mvp_tool_specs()
            .into_iter()
            .filter(|spec| allowed_tools.is_none_or(|allowed| allowed.contains(spec.name)))
            .map(|spec| ToolDefinition {
                name: spec.name.to_string(),
                description: Some(spec.description.to_string()),
                input_schema: spec.input_schema,
            });
        let plugin = self
            .plugin_tools
            .iter()
            .filter(|tool| {
                allowed_tools
                    .is_none_or(|allowed| allowed.contains(tool.definition().name.as_str()))
            })
            .map(|tool| ToolDefinition {
                name: tool.definition().name.clone(),
                description: tool.definition().description.clone(),
                input_schema: tool.definition().input_schema.clone(),
            });
        builtin.chain(plugin).collect()
    }

    pub fn permission_specs(
        &self,
        allowed_tools: Option<&BTreeSet<String>>,
    ) -> Result<Vec<(String, PermissionMode)>, String> {
        let builtin = mvp_tool_specs()
            .into_iter()
            .filter(|spec| allowed_tools.is_none_or(|allowed| allowed.contains(spec.name)))
            .map(|spec| (spec.name.to_string(), spec.required_permission));
        let plugin = self
            .plugin_tools
            .iter()
            .filter(|tool| {
                allowed_tools
                    .is_none_or(|allowed| allowed.contains(tool.definition().name.as_str()))
            })
            .map(|tool| {
                permission_mode_from_plugin(tool.required_permission())
                    .map(|permission| (tool.definition().name.clone(), permission))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(builtin.chain(plugin).collect())
    }

    pub fn execute(&self, name: &str, input: &Value) -> Result<String, String> {
        if mvp_tool_specs().iter().any(|spec| spec.name == name) {
            return execute_tool(name, input);
        }
        self.plugin_tools
            .iter()
            .find(|tool| tool.definition().name == name)
            .ok_or_else(|| format!("unsupported tool: {name}"))?
            .execute(input)
            .map_err(|error| error.to_string())
    }
}

fn normalize_tool_name(value: &str) -> String {
    value.trim().replace('-', "_").to_ascii_lowercase()
}

fn permission_mode_from_plugin(value: &str) -> Result<PermissionMode, String> {
    match value {
        "read-only" => Ok(PermissionMode::ReadOnly),
        "workspace-write" => Ok(PermissionMode::WorkspaceWrite),
        "danger-full-access" => Ok(PermissionMode::DangerFullAccess),
        other => Err(format!("unsupported plugin permission: {other}")),
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn mvp_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "bash",
            description: "Execute a shell command in the current workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" },
                    "dangerouslyDisableSandbox": { "type": "boolean" },
                    "namespaceRestrictions": { "type": "boolean" },
                    "isolateNetwork": { "type": "boolean" },
                    "filesystemMode": { "type": "string", "enum": ["off", "workspace-only", "allow-list"] },
                    "allowedMounts": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "read_file",
            description: "Read a text file from the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "write_file",
            description: "Write a text file in the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "edit_file",
            description: "Replace text in a workspace file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "glob_search",
            description: "Find files by glob pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "grep_search",
            description: "Search file contents with a regex pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "output_mode": { "type": "string" },
                    "-B": { "type": "integer", "minimum": 0 },
                    "-A": { "type": "integer", "minimum": 0 },
                    "-C": { "type": "integer", "minimum": 0 },
                    "context": { "type": "integer", "minimum": 0 },
                    "-n": { "type": "boolean" },
                    "-i": { "type": "boolean" },
                    "type": { "type": "string" },
                    "head_limit": { "type": "integer", "minimum": 1 },
                    "offset": { "type": "integer", "minimum": 0 },
                    "multiline": { "type": "boolean" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WebFetch",
            description:
                "Fetch a URL, convert it into readable text, and answer a prompt about it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "format": "uri" },
                    "prompt": { "type": "string" }
                },
                "required": ["url", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WebSearch",
            description: "Search the web for current information and return cited results.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 2 },
                    "allowed_domains": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "blocked_domains": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TodoWrite",
            description: "Update the structured task list for the current session.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "activeForm": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "activeForm", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["todos"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "Skill",
            description: "Load a local skill definition and its instructions.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "skill": { "type": "string" },
                    "args": { "type": "string" }
                },
                "required": ["skill"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "Agent",
            description: "Launch a background sub-agent to perform a delegated task autonomously. \
The sub-agent runs in an isolated session with its own tools and returns results when done. \
Use this when the task benefits from focused, independent work (e.g., codebase exploration, \
code review, verification, research). Do NOT use for simple lookups — use read_file/grep/glob directly. \
Available subagent_type values: 'Explore' (read-only research), 'Plan', 'Verification', \
'general-purpose' (full tool access). \
小说任务必须使用 novel_task 领域应用工具，不得使用 Agent。 \
The sub-agent inherits your model and API credentials automatically — do NOT research how to launch it, just call this tool.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "Short description of what the agent will do" },
                    "prompt": { "type": "string", "description": "Detailed instructions for the agent" },
                    "subagent_type": { "type": "string", "enum": ["Explore", "Plan", "Verification", "general-purpose"], "description": "Temporary agent type. Novel work is handled by its domain workflow and is not launched through Agent." },
                    "name": { "type": "string", "description": "Optional short name for the agent" },
                    "model": { "type": "string", "description": "Optional model override (leave empty to use default)" },
                    "run_in_background": { "type": "boolean", "description": "Set to true to run this temporary agent in the background.", "default": false }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "ToolSearch",
            description: "Search for deferred or specialized tools by exact name or keywords.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer", "minimum": 1 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "NotebookEdit",
            description: "Replace, insert, or delete a cell in a Jupyter notebook.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "notebook_path": { "type": "string" },
                    "cell_id": { "type": "string" },
                    "new_source": { "type": "string" },
                    "cell_type": { "type": "string", "enum": ["code", "markdown"] },
                    "edit_mode": { "type": "string", "enum": ["replace", "insert", "delete"] }
                },
                "required": ["notebook_path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "Sleep",
            description: "Wait for a specified duration without holding a shell process.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "duration_ms": { "type": "integer", "minimum": 0 }
                },
                "required": ["duration_ms"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "SendUserMessage",
            description: "Send a message to the user.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" },
                    "attachments": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "status": {
                        "type": "string",
                        "enum": ["normal", "proactive"]
                    }
                },
                "required": ["message", "status"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "Config",
            description: "Get or set Claude Code settings.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "setting": { "type": "string" },
                    "value": {
                        "type": ["string", "boolean", "number"]
                    }
                },
                "required": ["setting"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "EnterPlanMode",
            description: "Enable a worktree-local planning mode override and remember the previous local setting for ExitPlanMode.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "ExitPlanMode",
            description: "Restore or clear the worktree-local planning mode override created by EnterPlanMode.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "StructuredOutput",
            description: "Return structured output in the requested format.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": true
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "REPL",
            description: "Execute code in a REPL-like subprocess.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string" },
                    "language": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1 }
                },
                "required": ["code", "language"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "PowerShell",
            description: "Execute a PowerShell command with optional timeout.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "AskUserQuestion",
            description: "Ask the user a question and wait for their response.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["question"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "LSP",
            description: "Query Language Server Protocol for code intelligence (symbols, references, diagnostics).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["symbols", "references", "diagnostics", "definition", "hover"] },
                    "path": { "type": "string" },
                    "line": { "type": "integer", "minimum": 0 },
                    "character": { "type": "integer", "minimum": 0 },
                    "query": { "type": "string" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "ListMcpResources",
            description: "List available resources from connected MCP servers.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "ReadMcpResource",
            description: "Read a specific resource from an MCP server by URI.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" },
                    "uri": { "type": "string" }
                },
                "required": ["uri"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "McpAuth",
            description: "Authenticate with an MCP server that requires OAuth or credentials.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" }
                },
                "required": ["server"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "RemoteTrigger",
            description: "Trigger a remote action or webhook endpoint.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "DELETE"] },
                    "headers": { "type": "object" },
                    "body": { "type": "string" }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "MCP",
            description: "Execute a tool provided by a connected MCP server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" },
                    "tool": { "type": "string" },
                    "arguments": { "type": "object" }
                },
                "required": ["server", "tool"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TestingPermission",
            description: "Test-only tool for verifying permission enforcement behavior.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "search_memory",
            description: "按关键字搜索历史记忆。适合按内容/主题查找，如「DeepSeek 调研」「架构设计」「踩坑」。返回匹配的记忆条目（L1归档、L2会话摘要、潜意识印象）。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "搜索关键词，多个词用空格分隔" },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 20, "default": 5 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "novel_task",
            description: "执行可恢复的小说任务应用命令。start 冻结上下文并运行 Writer；resume、review、decide、publish 和 status 继续或查询同一 durable task。仅在 needs_clarification 后调用 resume，且 input 必须非空。start 遇到 ContextRef hash 变化时更新原调用；resume 遇到变化时必须在 context_refs 中保持原 role/path，仅替换为 actual hash，并只重试一次。",
            input_schema: json!({
                "type": "object",
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "start" },
                            "task_id": { "type": "string" },
                            "project_id": { "type": "string" },
                            "task_type": { "type": "string", "enum": ["outline", "volume_outline", "chapter_plan", "body", "continuation", "review", "polish", "retrospective"] },
                            "task_brief": { "type": "string" },
                            "target_chapter": { "type": "integer", "minimum": 1 },
                            "expected_revision": { "type": "integer", "minimum": 0 },
                            "output_path": { "type": "string" },
                            "context_refs": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "role": { "type": "string", "enum": ["body", "previous_chapter", "chapter_outline", "volume_outline", "character_card", "world_setting", "style_sample", "other"] },
                                        "canonical_path": { "type": "string" },
                                        "sha256": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["role", "canonical_path", "sha256"],
                                    "additionalProperties": false
                                }
                            },
                            "must_happen": { "type": "array", "items": { "type": "string" } },
                            "must_not_change": { "type": "array", "items": { "type": "string" } },
                            "acceptance_criteria": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                            "allow_web_research": { "type": "boolean", "default": false },
                            "publication_policy": { "type": "string", "enum": ["require_user_acceptance", "auto_after_main_review"], "default": "require_user_acceptance" },
                            "parent_task_id": { "type": "string" }
                        },
                        "required": ["action", "task_id", "project_id", "task_type", "task_brief", "expected_revision", "output_path", "acceptance_criteria"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "resume" },
                            "task_id": { "type": "string", "minLength": 1 },
                            "input": { "type": "string", "minLength": 1 },
                            "context_refs": {
                                "type": "array",
                                "description": "仅在应用报告 context hash 变化时提供；必须保持原 role、canonical_path 和顺序，只把 sha256 更新为 actual hash。",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "role": { "type": "string", "enum": ["body", "previous_chapter", "chapter_outline", "volume_outline", "character_card", "world_setting", "style_sample", "other"] },
                                        "canonical_path": { "type": "string" },
                                        "sha256": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["role", "canonical_path", "sha256"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["action", "task_id", "input"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "review" },
                            "task_id": { "type": "string" },
                            "draft_version": { "type": "integer", "minimum": 1 },
                            "reviewed_canon_revision": { "type": "integer", "minimum": 0 },
                            "verdict": { "type": "string", "enum": ["pass", "revise"] },
                            "checks": {
                                "type": "object",
                                "properties": {
                                    "user_requirements": { "type": "string", "enum": ["pass", "fail"] },
                                    "outline_alignment": { "type": "string", "enum": ["pass", "fail"] },
                                    "canon_consistency": { "type": "string", "enum": ["pass", "fail"] },
                                    "character_consistency": { "type": "string", "enum": ["pass", "fail"] },
                                    "timeline_consistency": { "type": "string", "enum": ["pass", "fail"] },
                                    "plot_and_foreshadowing": { "type": "string", "enum": ["pass", "fail"] },
                                    "style_quality": { "type": "string", "enum": ["pass", "fail"] },
                                    "pacing_and_hook": { "type": "string", "enum": ["pass", "fail"] }
                                },
                                "required": ["user_requirements", "outline_alignment", "canon_consistency", "character_consistency", "timeline_consistency", "plot_and_foreshadowing", "style_quality", "pacing_and_hook"],
                                "additionalProperties": false
                            },
                            "issues": { "type": "array", "items": { "oneOf": [{ "type": "string" }, { "type": "object", "properties": { "category": { "type": "string" }, "message": { "type": "string" }, "evidence_refs": { "type": "array", "items": { "type": "string" } } }, "required": ["category", "message"], "additionalProperties": false }] } },
                            "evidence_refs": { "type": "array", "items": { "type": "string" } },
                            "summary": { "type": "string" }
                        },
                        "required": ["action", "task_id", "draft_version", "reviewed_canon_revision", "verdict", "checks", "issues", "evidence_refs", "summary"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": { "action": { "const": "decide" }, "task_id": { "type": "string" }, "draft_version": { "type": "integer", "minimum": 1 }, "decision": { "type": "string", "enum": ["accept", "revise", "reject"] }, "feedback": { "type": "string" }, "decided_at": { "type": "integer" } },
                        "required": ["action", "task_id", "draft_version", "decision"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": { "action": { "const": "publish" }, "task_id": { "type": "string" }, "draft_version": { "type": "integer", "minimum": 1 } },
                        "required": ["action", "task_id", "draft_version"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": { "action": { "const": "status" }, "project_id": { "type": "string" } },
                        "required": ["action"],
                        "additionalProperties": false
                    }
                ]
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "novel_project",
            description: "管理 Novel 领域项目和 Canon 读模型。支持 create、list、recall、consistency 与 resolve_conflict。",
            input_schema: json!({
                "type": "object",
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "action": { "const": "create" }, "project_id": { "type": "string" }, "title": { "type": "string" }, "genres": { "type": "array", "items": { "type": "string" } }, "target_platform": { "type": "string" } },
                        "required": ["action", "project_id", "title"],
                        "additionalProperties": false
                    },
                    { "type": "object", "properties": { "action": { "const": "list" } }, "required": ["action"], "additionalProperties": false },
                    {
                        "type": "object",
                        "properties": { "action": { "const": "recall" }, "project_id": { "type": "string" }, "task_type": { "type": "string", "enum": ["outline", "volume_outline", "chapter_plan", "body", "continuation", "review", "polish", "retrospective"] } },
                        "required": ["action", "project_id", "task_type"],
                        "additionalProperties": false
                    },
                    { "type": "object", "properties": { "action": { "const": "consistency" }, "project_id": { "type": "string" } }, "required": ["action", "project_id"], "additionalProperties": false },
                    {
                        "type": "object",
                        "properties": { "action": { "const": "resolve_conflict" }, "project_id": { "type": "string" }, "conflict_id": { "type": "string" }, "resolution": { "type": "string" } },
                        "required": ["action", "project_id", "conflict_id", "resolution"],
                        "additionalProperties": false
                    }
                ]
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "list_recent_memories",
            description: "按时间列出最近的会话记忆（L2 会话总结）。适合用户说「刚刚」「昨天」「最近」「上次」等时间相关表述时使用。返回每条记忆的文件路径、时间范围、标签和摘要预览。如需查看完整内容，再用 read_file 工具读取对应路径。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20, "default": 5, "description": "返回最近几条会话总结" }
                },
                "required": [],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "graph_search_catalog",
            description: "Search the native knowledge graph catalog with low context cost. Returns node IDs, titles, types, matched keywords, scores, and hints only.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional path to the SQLite graph database file. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "query": { "type": "string", "description": "Keyword query. Multiple words may be separated by spaces." },
                    "graph_type": { "type": "string", "enum": ["Memory", "Code", "Video"], "default": "Memory" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "graph_get_node_detail",
            description: "Load one native knowledge graph node with compact upstream/downstream neighbors and unresolved source refs.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional path to the SQLite graph database file. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "node_id": { "type": "string" }
                },
                "required": ["node_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "graph_trace_memory",
            description: "Trace relationships from one native knowledge graph node using upstream, downstream, or both directions with depth and result limits.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional path to the SQLite graph database file. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "root_id": { "type": "string" },
                    "direction": { "type": "string", "enum": ["upstream", "downstream", "both"], "default": "both" },
                    "max_depth": { "type": "integer", "minimum": 1, "maximum": 10, "default": 2 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                },
                "required": ["root_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "graph_list_domains",
            description: "List active native knowledge graph domains with node counts, edge counts, and last update timestamps.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional path to the SQLite graph database file. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." }
                },
                "required": [],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "graph_add_memory",
            description: "Add or update a catalog-facing memory node in the native knowledge graph. Use for concise summaries and references, not full raw transcript text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional graph DB path. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "title": { "type": "string" },
                    "summary": { "type": "string" },
                    "keywords": { "type": "array", "items": { "type": "string" } },
                    "catalog_type": { "type": "string", "default": "memory" },
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.7 },
                    "source_refs": { "type": "array", "items": { "type": "object" } }
                },
                "required": ["title"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "graph_add_concept",
            description: "Add or update a concept node in the native knowledge graph.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional graph DB path. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "name": { "type": "string" },
                    "summary": { "type": "string" },
                    "graph_type": { "type": "string", "enum": ["Memory", "Code", "Video"], "default": "Memory" },
                    "aliases": { "type": "array", "items": { "type": "string" } },
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.7 }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "graph_add_code_node",
            description: "Add or update a code node in the native knowledge graph for a file, module, function, class, or symbol.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional graph DB path. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "path": { "type": "string" },
                    "symbol": { "type": "string" },
                    "summary": { "type": "string" },
                    "code_kind": { "type": "string", "description": "file, module, function, class, variable, etc." },
                    "importance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.7 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "graph_index_code_workspace",
            description: "Index Rust source files in a workspace into the native Code graph with stable file/module nodes and Contains edges.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional graph DB path. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "root": { "type": "string", "description": "Workspace root to scan. Defaults to the current directory." },
                    "max_files": { "type": "integer", "minimum": 1, "maximum": 5000, "default": 1000 }
                },
                "required": [],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "graph_link_nodes",
            description: "Link two existing native knowledge graph nodes with a typed edge. Both nodes must exist and share the same graph_type.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "db_path": { "type": "string", "description": "Optional graph DB path. Defaults to injected executor path, AI_BRAIN_GRAPH_DB, or ~/.ai-brain/graph/graph.db." },
                    "src": { "type": "string" },
                    "dst": { "type": "string" },
                    "edge_kind": { "type": "string", "enum": ["MentionedIn", "RelatedTo", "SimilarTo", "CausedBy", "DependsOn", "DerivedFrom", "Calls", "Contains", "Imports", "Defines", "Invokes"] },
                    "weight": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.7 },
                    "props": { "type": "object" }
                },
                "required": ["src", "dst", "edge_kind"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
    ]
}

pub fn execute_tool(name: &str, input: &Value) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    execute_tool_in_directory(name, input, &cwd)
}

pub fn execute_tool_in_directory(
    name: &str,
    input: &Value,
    working_directory: &Path,
) -> Result<String, String> {
    match name {
        "bash" => from_value::<BashCommandInput>(input)
            .and_then(|value| run_bash_in_directory(value, working_directory)),
        "read_file" => from_value::<ReadFileInput>(input)
            .and_then(|value| run_read_file_in_directory(value, working_directory)),
        "write_file" => from_value::<WriteFileInput>(input)
            .and_then(|value| run_write_file_in_directory(value, working_directory)),
        "edit_file" => from_value::<EditFileInput>(input)
            .and_then(|value| run_edit_file_in_directory(value, working_directory)),
        "glob_search" => from_value::<GlobSearchInputValue>(input)
            .and_then(|value| run_glob_search_in_directory(value, working_directory)),
        "grep_search" => from_value::<GrepSearchInput>(input)
            .and_then(|value| run_grep_search_in_directory(value, working_directory)),
        "TodoWrite" => from_value::<TodoWriteInput>(input)
            .and_then(|value| run_todo_write_in_directory(value, working_directory)),
        "Agent" => from_value::<AgentInput>(input)
            .and_then(|value| run_agent_in_directory(value, working_directory)),
        "NotebookEdit" => from_value::<NotebookEditInput>(input)
            .and_then(|value| run_notebook_edit_in_directory(value, working_directory)),
        "Config" => from_value::<ConfigInput>(input)
            .and_then(|value| run_config_in_directory(value, working_directory)),
        "EnterPlanMode" => from_value::<EnterPlanModeInput>(input)
            .and_then(|value| run_enter_plan_mode_in_directory(value, working_directory)),
        "ExitPlanMode" => from_value::<ExitPlanModeInput>(input)
            .and_then(|value| run_exit_plan_mode_in_directory(value, working_directory)),
        "REPL" => from_value::<ReplInput>(input)
            .and_then(|value| run_repl_in_directory(value, working_directory)),
        "PowerShell" => from_value::<PowerShellInput>(input)
            .and_then(|value| run_powershell_in_directory(value, working_directory)),
        "graph_search_catalog" => from_value::<GraphSearchCatalogInput>(input)
            .and_then(|value| run_graph_search_catalog_in_directory(value, working_directory)),
        "graph_get_node_detail" => from_value::<GraphGetNodeDetailInput>(input)
            .and_then(|value| run_graph_get_node_detail_in_directory(value, working_directory)),
        "graph_trace_memory" => from_value::<GraphTraceMemoryInput>(input)
            .and_then(|value| run_graph_trace_memory_in_directory(value, working_directory)),
        "graph_list_domains" => from_value::<GraphListDomainsInput>(input)
            .and_then(|value| run_graph_list_domains_in_directory(value, working_directory)),
        "graph_add_memory" => from_value::<GraphAddMemoryInput>(input)
            .and_then(|value| run_graph_add_memory_in_directory(value, working_directory)),
        "graph_add_concept" => from_value::<GraphAddConceptInput>(input)
            .and_then(|value| run_graph_add_concept_in_directory(value, working_directory)),
        "graph_add_code_node" => from_value::<GraphAddCodeNodeInput>(input)
            .and_then(|value| run_graph_add_code_node_in_directory(value, working_directory)),
        "graph_index_code_workspace" => {
            from_value::<GraphIndexCodeWorkspaceInput>(input).and_then(|value| {
                run_graph_index_code_workspace_in_directory(value, working_directory)
            })
        }
        "graph_link_nodes" => from_value::<GraphLinkNodesInput>(input)
            .and_then(|value| run_graph_link_nodes_in_directory(value, working_directory)),
        _ => execute_non_workspace_tool(name, input),
    }
}

fn execute_non_workspace_tool(name: &str, input: &Value) -> Result<String, String> {
    match name {
        "WebFetch" => from_value::<WebFetchInput>(input).and_then(run_web_fetch),
        "WebSearch" => from_value::<WebSearchInput>(input).and_then(run_web_search),
        "Skill" => Err("Skill tool is handled by RealToolExecutor directly".to_string()),
        "ToolSearch" => from_value::<ToolSearchInput>(input).and_then(run_tool_search),
        "Sleep" => from_value::<SleepInput>(input).and_then(run_sleep),
        "SendUserMessage" | "Brief" => from_value::<BriefInput>(input).and_then(run_brief),
        "StructuredOutput" => {
            from_value::<StructuredOutputInput>(input).and_then(run_structured_output)
        }
        "AskUserQuestion" => {
            from_value::<AskUserQuestionInput>(input).and_then(run_ask_user_question)
        }
        "LSP" => from_value::<LspInput>(input).and_then(run_lsp),
        "ListMcpResources" => {
            from_value::<McpResourceInput>(input).and_then(run_list_mcp_resources)
        }
        "ReadMcpResource" => from_value::<McpResourceInput>(input).and_then(run_read_mcp_resource),
        "McpAuth" => from_value::<McpAuthInput>(input).and_then(run_mcp_auth),
        "RemoteTrigger" => from_value::<RemoteTriggerInput>(input).and_then(run_remote_trigger),
        "MCP" => from_value::<McpToolInput>(input).and_then(run_mcp_tool),
        "TestingPermission" => {
            from_value::<TestingPermissionInput>(input).and_then(run_testing_permission)
        }
        "novel_task" | "novel_project" => {
            Err(format!("{name} is handled by RealToolExecutor directly"))
        }
        _ => Err(format!("unsupported tool: {name}")),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_ask_user_question(input: AskUserQuestionInput) -> Result<String, String> {
    let mut result = json!({
        "question": input.question,
        "status": "pending",
        "message": "Waiting for user response"
    });
    if let Some(options) = &input.options {
        result["options"] = json!(options);
    }
    to_pretty_json(result)
}
#[allow(clippy::needless_pass_by_value)]
fn run_lsp(input: LspInput) -> Result<String, String> {
    to_pretty_json(json!({
        "action": input.action,
        "path": input.path,
        "line": input.line,
        "character": input.character,
        "query": input.query,
        "results": [],
        "message": "LSP server not connected"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_list_mcp_resources(input: McpResourceInput) -> Result<String, String> {
    to_pretty_json(json!({
        "server": input.server,
        "resources": [],
        "message": "No MCP resources available"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_read_mcp_resource(input: McpResourceInput) -> Result<String, String> {
    to_pretty_json(json!({
        "server": input.server,
        "uri": input.uri,
        "content": "",
        "message": "Resource not available"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_mcp_auth(input: McpAuthInput) -> Result<String, String> {
    to_pretty_json(json!({
        "server": input.server,
        "status": "auth_required",
        "message": "MCP authentication not yet implemented"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_remote_trigger(input: RemoteTriggerInput) -> Result<String, String> {
    to_pretty_json(json!({
        "url": input.url,
        "method": input.method.unwrap_or_else(|| "GET".to_string()),
        "headers": input.headers,
        "body": input.body,
        "status": "triggered",
        "message": "Remote trigger stub response"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_mcp_tool(input: McpToolInput) -> Result<String, String> {
    to_pretty_json(json!({
        "server": input.server,
        "tool": input.tool,
        "arguments": input.arguments,
        "result": null,
        "message": "MCP tool proxy not yet connected"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_testing_permission(input: TestingPermissionInput) -> Result<String, String> {
    to_pretty_json(json!({
        "action": input.action,
        "permitted": true,
        "message": "Testing permission tool stub"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_search_catalog_in_directory(
    input: GraphSearchCatalogInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    let mut query = CatalogQuery::new(split_keywords(&input.query));
    query.graph_type = input
        .graph_type
        .as_deref()
        .map(parse_graph_type)
        .transpose()?
        .or(Some(GraphType::Memory));
    query.limit = input.limit.unwrap_or(10).min(50);
    to_pretty_json(
        store
            .search_catalog(&query)
            .map_err(|error| error.to_string())?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_get_node_detail_in_directory(
    input: GraphGetNodeDetailInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    to_pretty_json(
        store
            .get_node_detail(&input.node_id)
            .map_err(|error| error.to_string())?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_trace_memory_in_directory(
    input: GraphTraceMemoryInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    let mut query = TraceQuery::new(input.root_id);
    query.direction = input
        .direction
        .as_deref()
        .map(parse_trace_direction)
        .transpose()?
        .unwrap_or(TraceDirection::Both);
    query.max_depth = input.max_depth.unwrap_or(2).min(10);
    query.limit = input.limit.unwrap_or(20).min(100);
    to_pretty_json(
        store
            .trace_memory(&query)
            .map_err(|error| error.to_string())?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_list_domains_in_directory(
    input: GraphListDomainsInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    to_pretty_json(store.list_domains().map_err(|error| error.to_string())?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_add_memory_in_directory(
    input: GraphAddMemoryInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    let title = required_non_empty("title", &input.title)?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut props = HashMap::from([
        ("layer".into(), json!("catalog_entry")),
        ("catalog_title".into(), json!(title)),
        (
            "catalog_type".into(),
            json!(input.catalog_type.unwrap_or_else(|| "memory".to_string())),
        ),
    ]);
    if let Some(summary) = optional_non_empty(input.summary) {
        props.insert("summary".into(), json!(summary));
    }
    if let Some(keywords) = clean_string_list(input.keywords) {
        props.insert("catalog_keywords".into(), json!(keywords));
    }
    if let Some(refs) = input.source_refs.filter(|refs| !refs.is_empty()) {
        props.insert("source_refs".into(), json!(refs));
    }
    let node = Node {
        id: gen_node_id(GraphType::Memory, NodeKind::Memory),
        kind: NodeKind::Memory,
        graph_type: GraphType::Memory,
        props,
        importance: bounded_score(input.importance.unwrap_or(0.7), "importance")?,
        created_at: now,
        last_accessed: now,
        superseded: false,
    };
    store
        .upsert_node(&node)
        .map_err(|error| error.to_string())?;
    to_pretty_json(json!({
        "status": "ok",
        "node_id": node.id,
        "node": node
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_add_concept_in_directory(
    input: GraphAddConceptInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    let name = required_non_empty("name", &input.name)?;
    let graph_type = input
        .graph_type
        .as_deref()
        .map(parse_graph_type)
        .transpose()?
        .unwrap_or(GraphType::Memory);
    let now = chrono::Utc::now().timestamp_millis();
    let mut props = HashMap::from([
        ("name".into(), json!(name)),
        ("catalog_title".into(), json!(name)),
        ("catalog_type".into(), json!("concept")),
    ]);
    if let Some(summary) = optional_non_empty(input.summary) {
        props.insert("summary".into(), json!(summary));
    }
    if let Some(aliases) = clean_string_list(input.aliases) {
        props.insert("aliases".into(), json!(aliases));
    }
    let node = Node {
        id: gen_node_id(graph_type.clone(), NodeKind::Concept),
        kind: NodeKind::Concept,
        graph_type,
        props,
        importance: bounded_score(input.importance.unwrap_or(0.7), "importance")?,
        created_at: now,
        last_accessed: now,
        superseded: false,
    };
    store
        .upsert_node(&node)
        .map_err(|error| error.to_string())?;
    to_pretty_json(json!({
        "status": "ok",
        "node_id": node.id,
        "node": node
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_add_code_node_in_directory(
    input: GraphAddCodeNodeInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    let path = required_non_empty("path", &input.path)?;
    let now = chrono::Utc::now().timestamp_millis();
    let title = input
        .symbol
        .as_deref()
        .filter(|symbol| !symbol.trim().is_empty())
        .map_or_else(|| path.to_string(), |symbol| format!("{path}::{symbol}"));
    let mut props = HashMap::from([
        ("path".into(), json!(path)),
        ("catalog_title".into(), json!(title)),
        ("catalog_type".into(), json!("code")),
    ]);
    if let Some(symbol) = optional_non_empty(input.symbol) {
        props.insert("symbol".into(), json!(symbol));
    }
    if let Some(summary) = optional_non_empty(input.summary) {
        props.insert("summary".into(), json!(summary));
    }
    if let Some(code_kind) = optional_non_empty(input.code_kind) {
        props.insert("code_kind".into(), json!(code_kind));
    }
    let node = Node {
        id: gen_node_id(GraphType::Code, NodeKind::Code),
        kind: NodeKind::Code,
        graph_type: GraphType::Code,
        props,
        importance: bounded_score(input.importance.unwrap_or(0.7), "importance")?,
        created_at: now,
        last_accessed: now,
        superseded: false,
    };
    store
        .upsert_node(&node)
        .map_err(|error| error.to_string())?;
    to_pretty_json(json!({
        "status": "ok",
        "node_id": node.id,
        "node": node
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_index_code_workspace_in_directory(
    input: GraphIndexCodeWorkspaceInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let root = input
        .root
        .as_deref()
        .filter(|root| !root.trim().is_empty())
        .map(Path::new)
        .map(|root| resolve_path_in_directory(working_directory, root))
        .unwrap_or_else(|| working_directory.to_path_buf());
    let root = fs::canonicalize(&root).map_err(|error| {
        format!(
            "cannot resolve code workspace root `{}`: {error}",
            root.display()
        )
    })?;
    if !root.is_dir() {
        return Err(format!(
            "code workspace root is not a directory: {}",
            root.display()
        ));
    }

    let max_files = input.max_files.unwrap_or(1000).min(5000);
    let mut files = Vec::new();
    collect_rust_files(&root, &mut files, max_files)?;
    files.sort();

    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut indexed_modules = BTreeSet::new();
    let mut indexed_edges = BTreeSet::new();
    let mut symbols_indexed = 0usize;

    upsert_code_module_node(&store, "", now)?;
    indexed_modules.insert(String::new());

    for file in &files {
        let rel = relative_slash_path(&root, file)?;
        let module_rel = file
            .parent()
            .and_then(|parent| parent.strip_prefix(&root).ok())
            .map(path_to_slash)
            .unwrap_or_default();
        ensure_code_module_path(
            &store,
            &module_rel,
            now,
            &mut indexed_modules,
            &mut indexed_edges,
        )?;
        upsert_code_file_node(&store, &rel, now)?;

        let module_id = stable_code_module_node_id(&module_rel);
        let file_id = stable_code_file_node_id(&rel);
        insert_code_contains_edge(&store, &module_id, &file_id, now)?;
        indexed_edges.insert((module_id, file_id.clone(), "Contains".to_string()));

        let source = fs::read_to_string(file)
            .map_err(|error| format!("cannot read source file `{}`: {error}", file.display()))?;
        for symbol in parse_rust_symbols(&source) {
            upsert_code_symbol_node(&store, &rel, &symbol, now)?;
            let symbol_id = stable_code_symbol_node_id(&rel, &symbol.kind, &symbol.name);
            insert_code_defines_edge(&store, &file_id, &symbol_id, now)?;
            indexed_edges.insert((file_id.clone(), symbol_id, "Defines".to_string()));
            symbols_indexed += 1;
        }
    }

    to_pretty_json(json!({
        "status": "ok",
        "db_path": db_path,
        "root": root,
        "files_indexed": files.len(),
        "modules_indexed": indexed_modules.len(),
        "symbols_indexed": symbols_indexed,
        "edges_indexed": indexed_edges.len(),
        "truncated": files.len() >= max_files,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RustSymbol {
    kind: String,
    name: String,
    line: usize,
}

fn parse_rust_symbols(source: &str) -> Vec<RustSymbol> {
    source
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            parse_rust_symbol_line(line).map(|mut symbol| {
                symbol.line = idx + 1;
                symbol
            })
        })
        .collect()
}

fn parse_rust_symbol_line(line: &str) -> Option<RustSymbol> {
    let line = line.split("//").next().unwrap_or_default().trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with("use ") {
        return None;
    }

    let mut tokens = line.split_whitespace().collect::<Vec<_>>();
    while tokens.first().is_some_and(|token| {
        token.starts_with("pub") || matches!(*token, "async" | "unsafe" | "const")
    }) {
        tokens.remove(0);
    }
    let first = *tokens.first()?;

    match first {
        "fn" => symbol_after_keyword(&tokens, "function"),
        "struct" => symbol_after_keyword(&tokens, "struct"),
        "enum" => symbol_after_keyword(&tokens, "enum"),
        "trait" => symbol_after_keyword(&tokens, "trait"),
        "mod" => symbol_after_keyword(&tokens, "module"),
        "impl" => parse_impl_symbol(&tokens),
        _ => None,
    }
}

fn symbol_after_keyword(tokens: &[&str], kind: &str) -> Option<RustSymbol> {
    let raw_name = tokens.get(1)?;
    let name = clean_rust_identifier(raw_name)?;
    Some(RustSymbol {
        kind: kind.to_string(),
        name,
        line: 0,
    })
}

fn parse_impl_symbol(tokens: &[&str]) -> Option<RustSymbol> {
    let after_impl = tokens.get(1..)?;
    let name = if let Some(for_idx) = after_impl.iter().position(|token| *token == "for") {
        clean_rust_identifier(after_impl.get(for_idx + 1)?)?
    } else {
        clean_rust_identifier(after_impl.first()?)?
    };
    Some(RustSymbol {
        kind: "impl".to_string(),
        name,
        line: 0,
    })
}

fn clean_rust_identifier(value: &str) -> Option<String> {
    let value = value.trim_start_matches("r#");
    let ident = value
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

fn collect_rust_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    max_files: usize,
) -> Result<(), String> {
    if files.len() >= max_files {
        return Ok(());
    }
    let mut entries = fs::read_dir(root)
        .map_err(|error| format!("cannot read directory `{}`: {error}", root.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        if files.len() >= max_files {
            break;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            if should_skip_code_index_dir(&path) {
                continue;
            }
            collect_rust_files(&path, files, max_files)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn should_skip_code_index_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name == "target" || name == "node_modules" || name == ".git" || name.starts_with('.')
}

fn ensure_code_module_path(
    store: &GraphStore,
    module_rel: &str,
    now: i64,
    indexed_modules: &mut BTreeSet<String>,
    indexed_edges: &mut BTreeSet<(String, String, String)>,
) -> Result<(), String> {
    let mut current = String::new();
    let mut parent = String::new();
    for part in module_rel.split('/').filter(|part| !part.is_empty()) {
        current = if current.is_empty() {
            part.to_string()
        } else {
            format!("{current}/{part}")
        };
        if indexed_modules.insert(current.clone()) {
            upsert_code_module_node(store, &current, now)?;
        }
        let parent_id = stable_code_module_node_id(&parent);
        let child_id = stable_code_module_node_id(&current);
        insert_code_contains_edge(store, &parent_id, &child_id, now)?;
        indexed_edges.insert((parent_id, child_id, "Contains".to_string()));
        parent = current.clone();
    }
    Ok(())
}

fn upsert_code_module_node(store: &GraphStore, module_rel: &str, now: i64) -> Result<(), String> {
    let title = if module_rel.is_empty() {
        "workspace".to_string()
    } else {
        format!("module:{module_rel}")
    };
    let keywords = if module_rel.is_empty() {
        vec![
            "workspace".to_string(),
            "rust".to_string(),
            "module".to_string(),
        ]
    } else {
        vec![
            module_rel.to_string(),
            module_rel
                .rsplit('/')
                .next()
                .unwrap_or(module_rel)
                .to_string(),
            "rust".to_string(),
            "module".to_string(),
        ]
    };
    let node = Node {
        id: stable_code_module_node_id(module_rel),
        kind: NodeKind::Code,
        graph_type: GraphType::Code,
        props: HashMap::from([
            ("path".into(), json!(module_rel)),
            ("catalog_title".into(), json!(title)),
            ("catalog_keywords".into(), json!(keywords)),
            ("catalog_type".into(), json!("code_module")),
            ("code_kind".into(), json!("module")),
        ]),
        importance: if module_rel.is_empty() { 0.8 } else { 0.6 },
        created_at: now,
        last_accessed: now,
        superseded: false,
    };
    store.upsert_node(&node).map_err(|error| error.to_string())
}

fn upsert_code_file_node(store: &GraphStore, rel: &str, now: i64) -> Result<(), String> {
    let file_name = Path::new(rel)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(rel);
    let stem = Path::new(rel)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(file_name);
    let node = Node {
        id: stable_code_file_node_id(rel),
        kind: NodeKind::Code,
        graph_type: GraphType::Code,
        props: HashMap::from([
            ("path".into(), json!(rel)),
            ("catalog_title".into(), json!(rel)),
            (
                "catalog_keywords".into(),
                json!(vec![
                    rel.to_string(),
                    file_name.to_string(),
                    stem.to_string(),
                    "rust".to_string(),
                    "file".to_string(),
                ]),
            ),
            ("catalog_type".into(), json!("code_file")),
            ("code_kind".into(), json!("file")),
        ]),
        importance: 0.7,
        created_at: now,
        last_accessed: now,
        superseded: false,
    };
    store.upsert_node(&node).map_err(|error| error.to_string())
}

fn upsert_code_symbol_node(
    store: &GraphStore,
    rel: &str,
    symbol: &RustSymbol,
    now: i64,
) -> Result<(), String> {
    let title = format!("{rel}::{}", symbol.name);
    let node = Node {
        id: stable_code_symbol_node_id(rel, &symbol.kind, &symbol.name),
        kind: NodeKind::Code,
        graph_type: GraphType::Code,
        props: HashMap::from([
            ("path".into(), json!(rel)),
            ("symbol".into(), json!(&symbol.name)),
            ("line".into(), json!(symbol.line)),
            ("catalog_title".into(), json!(title)),
            (
                "catalog_keywords".into(),
                json!(vec![
                    rel.to_string(),
                    symbol.name.clone(),
                    symbol.kind.clone(),
                    "rust".to_string(),
                    "symbol".to_string(),
                ]),
            ),
            ("catalog_type".into(), json!("code_symbol")),
            ("code_kind".into(), json!(&symbol.kind)),
        ]),
        importance: 0.75,
        created_at: now,
        last_accessed: now,
        superseded: false,
    };
    store.upsert_node(&node).map_err(|error| error.to_string())
}

fn insert_code_contains_edge(
    store: &GraphStore,
    src: &str,
    dst: &str,
    now: i64,
) -> Result<(), String> {
    store
        .insert_edge(&Edge {
            src: src.to_string(),
            dst: dst.to_string(),
            kind: EdgeKind::Contains,
            props: HashMap::from([("reason".into(), json!("code_workspace_index"))]),
            created_at: now,
            weight: 0.8,
        })
        .map_err(|error| error.to_string())
}

fn insert_code_defines_edge(
    store: &GraphStore,
    src: &str,
    dst: &str,
    now: i64,
) -> Result<(), String> {
    store
        .insert_edge(&Edge {
            src: src.to_string(),
            dst: dst.to_string(),
            kind: EdgeKind::Defines,
            props: HashMap::from([("reason".into(), json!("rust_symbol_index"))]),
            created_at: now,
            weight: 0.8,
        })
        .map_err(|error| error.to_string())
}

fn relative_slash_path(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map(path_to_slash)
        .map_err(|error| error.to_string())
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn stable_code_module_node_id(module_rel: &str) -> String {
    stable_code_node_id("module", module_rel)
}

fn stable_code_file_node_id(rel: &str) -> String {
    stable_code_node_id("file", rel)
}

fn stable_code_symbol_node_id(rel: &str, kind: &str, name: &str) -> String {
    stable_code_node_id("symbol", &format!("{rel}_{kind}_{name}"))
}

fn stable_code_node_id(kind: &str, value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let stable = sanitized.trim_matches('_');
    let suffix = if stable.is_empty() { "root" } else { stable };
    format!("code_code_{kind}_{suffix}")
}

#[allow(clippy::needless_pass_by_value)]
fn run_graph_link_nodes_in_directory(
    input: GraphLinkNodesInput,
    working_directory: &Path,
) -> Result<String, String> {
    let db_path = resolve_graph_db_path_in_directory(input.db_path.as_deref(), working_directory)?;
    let store = GraphStore::open(&db_path).map_err(|error| error.to_string())?;
    let src = required_non_empty("src", &input.src)?;
    let dst = required_non_empty("dst", &input.dst)?;
    let src_node = store
        .get_node(src)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("source node not found: {src}"))?;
    let dst_node = store
        .get_node(dst)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("destination node not found: {dst}"))?;
    if src_node.graph_type != dst_node.graph_type {
        return Err(format!(
            "graph_type mismatch: src={:?}, dst={:?}",
            src_node.graph_type, dst_node.graph_type
        ));
    }
    let props = match input.props {
        Some(Value::Object(map)) => map.into_iter().collect(),
        Some(_) => return Err("props must be a JSON object".into()),
        None => HashMap::new(),
    };
    let edge = Edge {
        src: src.to_string(),
        dst: dst.to_string(),
        kind: parse_edge_kind(&input.edge_kind)?,
        props,
        created_at: chrono::Utc::now().timestamp_millis(),
        weight: bounded_score(input.weight.unwrap_or(0.7), "weight")?,
    };
    store
        .insert_edge(&edge)
        .map_err(|error| error.to_string())?;
    to_pretty_json(json!({
        "status": "ok",
        "edge": edge
    }))
}
fn from_value<T: for<'de> Deserialize<'de>>(input: &Value) -> Result<T, String> {
    serde_json::from_value(input.clone()).map_err(|error| error.to_string())
}

fn resolve_path_in_directory(working_directory: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_directory.join(path)
    }
}

fn run_bash_in_directory(
    input: BashCommandInput,
    working_directory: &Path,
) -> Result<String, String> {
    serde_json::to_string_pretty(
        &execute_bash_in_dir(input, working_directory).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn run_read_file_in_directory(
    input: ReadFileInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(
        read_file_in_dir(working_directory, &input.path, input.offset, input.limit)
            .map_err(io_to_string)?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_write_file_in_directory(
    input: WriteFileInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(
        write_file_in_dir(working_directory, &input.path, &input.content).map_err(io_to_string)?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_edit_file_in_directory(
    input: EditFileInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(
        edit_file_in_dir(
            working_directory,
            &input.path,
            &input.old_string,
            &input.new_string,
            input.replace_all.unwrap_or(false),
        )
        .map_err(io_to_string)?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_glob_search_in_directory(
    input: GlobSearchInputValue,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(
        glob_search_in_dir(working_directory, &input.pattern, input.path.as_deref())
            .map_err(io_to_string)?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_grep_search_in_directory(
    input: GrepSearchInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(grep_search_in_dir(working_directory, &input).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_web_fetch(input: WebFetchInput) -> Result<String, String> {
    to_pretty_json(execute_web_fetch(&input)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_web_search(input: WebSearchInput) -> Result<String, String> {
    to_pretty_json(execute_web_search(&input)?)
}

fn run_todo_write_in_directory(
    input: TodoWriteInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(execute_todo_write(input, working_directory)?)
}

fn run_agent_in_directory(input: AgentInput, working_directory: &Path) -> Result<String, String> {
    to_pretty_json(execute_agent_in_directory(input, working_directory)?)
}

pub struct AgentToolLaunch {
    pub output_json: String,
    pub completion_rx: Option<tokio::sync::oneshot::Receiver<AgentCompletion>>,
    pub cancellation: Option<tokio_util::sync::CancellationToken>,
}

#[derive(Debug, Clone)]
pub struct AgentCompletion {
    pub agent_id: String,
    pub name: String,
    pub status: String,
    pub output: String,
    pub error: Option<String>,
    pub duration_ms: u64,
}

pub async fn execute_agent_tool_with_completion(input: &Value) -> Result<AgentToolLaunch, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    execute_agent_tool_with_completion_in_directory(input, &cwd).await
}

async fn execute_agent_tool_with_completion_in_directory(
    input: &Value,
    working_directory: &Path,
) -> Result<AgentToolLaunch, String> {
    let input = from_value::<AgentInput>(input)?;
    let run_in_background = input.run_in_background;
    let launch = start_agent_in_directory(input, working_directory).await?;
    if !run_in_background {
        let done = launch
            .completion_rx
            .await
            .map_err(|_| String::from("sub-agent completion task closed unexpectedly"))?;
        return Ok(AgentToolLaunch {
            output_json: to_pretty_json(done.manifest)?,
            completion_rx: None,
            cancellation: None,
        });
    }

    let output_json = to_pretty_json(launch.manifest.clone())?;
    let cancellation = launch.cancellation.clone();
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let completion = match launch.completion_rx.await {
            Ok(done) => AgentCompletion::from_done(done),
            Err(_) => AgentCompletion {
                agent_id: launch.manifest.agent_id,
                name: launch.manifest.name,
                status: "failed".into(),
                output: String::new(),
                error: Some("sub-agent completion task closed unexpectedly".into()),
                duration_ms: 0,
            },
        };
        let _ = completion_tx.send(completion);
    });

    Ok(AgentToolLaunch {
        output_json,
        completion_rx: Some(completion_rx),
        cancellation: Some(cancellation),
    })
}

fn run_tool_search(input: ToolSearchInput) -> Result<String, String> {
    to_pretty_json(execute_tool_search(input))
}

fn run_notebook_edit_in_directory(
    input: NotebookEditInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(execute_notebook_edit(input, working_directory)?)
}

fn run_sleep(input: SleepInput) -> Result<String, String> {
    to_pretty_json(execute_sleep(input)?)
}

fn run_brief(input: BriefInput) -> Result<String, String> {
    to_pretty_json(execute_brief(input)?)
}

fn run_config_in_directory(input: ConfigInput, working_directory: &Path) -> Result<String, String> {
    to_pretty_json(execute_config(input, working_directory)?)
}

fn run_enter_plan_mode_in_directory(
    input: EnterPlanModeInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(execute_enter_plan_mode(input, working_directory)?)
}

fn run_exit_plan_mode_in_directory(
    input: ExitPlanModeInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(execute_exit_plan_mode(input, working_directory)?)
}

fn run_structured_output(input: StructuredOutputInput) -> Result<String, String> {
    to_pretty_json(execute_structured_output(input)?)
}

fn run_repl_in_directory(input: ReplInput, working_directory: &Path) -> Result<String, String> {
    to_pretty_json(execute_repl(input, working_directory)?)
}

fn run_powershell_in_directory(
    input: PowerShellInput,
    working_directory: &Path,
) -> Result<String, String> {
    to_pretty_json(execute_powershell(input, working_directory).map_err(|error| error.to_string())?)
}

fn to_pretty_json<T: serde::Serialize>(value: T) -> Result<String, String> {
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn io_to_string(error: std::io::Error) -> String {
    error.to_string()
}

#[derive(Debug, Deserialize)]
struct ReadFileInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct WriteFileInput {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct EditFileInput {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GlobSearchInputValue {
    pattern: String,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebFetchInput {
    url: String,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct WebSearchInput {
    query: String,
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TodoWriteInput {
    todos: Vec<TodoItem>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
struct TodoItem {
    content: String,
    #[serde(rename = "activeForm")]
    active_form: String,
    status: TodoStatus,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentInput {
    description: String,
    prompt: String,
    subagent_type: Option<String>,
    name: Option<String>,
    model: Option<String>,
    #[serde(default)]
    run_in_background: bool,
}

#[derive(Debug, Deserialize)]
struct ToolSearchInput {
    query: String,
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct NotebookEditInput {
    notebook_path: String,
    cell_id: Option<String>,
    new_source: Option<String>,
    cell_type: Option<NotebookCellType>,
    edit_mode: Option<NotebookEditMode>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum NotebookCellType {
    Code,
    Markdown,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum NotebookEditMode {
    Replace,
    Insert,
    Delete,
}

#[derive(Debug, Deserialize)]
struct SleepInput {
    duration_ms: u64,
}

#[derive(Debug, Deserialize)]
struct BriefInput {
    message: String,
    attachments: Option<Vec<String>>,
    status: BriefStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BriefStatus {
    Normal,
    Proactive,
}

#[derive(Debug, Deserialize)]
struct ConfigInput {
    setting: String,
    value: Option<ConfigValue>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EnterPlanModeInput {}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExitPlanModeInput {}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfigValue {
    String(String),
    Bool(bool),
    Number(f64),
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct StructuredOutputInput(BTreeMap<String, Value>);

#[derive(Debug, Deserialize)]
struct ReplInput {
    code: String,
    language: String,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PowerShellInput {
    command: String,
    timeout: Option<u64>,
    description: Option<String>,
    run_in_background: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AskUserQuestionInput {
    question: String,
    #[serde(default)]
    options: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct LspInput {
    action: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    character: Option<u32>,
    #[serde(default)]
    query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpResourceInput {
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpAuthInput {
    server: String,
}

#[derive(Debug, Deserialize)]
struct RemoteTriggerInput {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpToolInput {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TestingPermissionInput {
    action: String,
}

#[derive(Debug, Deserialize)]
struct GraphSearchCatalogInput {
    db_path: Option<String>,
    query: String,
    graph_type: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GraphGetNodeDetailInput {
    db_path: Option<String>,
    node_id: String,
}

#[derive(Debug, Deserialize)]
struct GraphTraceMemoryInput {
    db_path: Option<String>,
    root_id: String,
    direction: Option<String>,
    max_depth: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GraphListDomainsInput {
    db_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphAddMemoryInput {
    db_path: Option<String>,
    title: String,
    summary: Option<String>,
    keywords: Option<Vec<String>>,
    catalog_type: Option<String>,
    importance: Option<f64>,
    source_refs: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
struct GraphAddConceptInput {
    db_path: Option<String>,
    name: String,
    summary: Option<String>,
    graph_type: Option<String>,
    aliases: Option<Vec<String>>,
    importance: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct GraphAddCodeNodeInput {
    db_path: Option<String>,
    path: String,
    symbol: Option<String>,
    summary: Option<String>,
    code_kind: Option<String>,
    importance: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct GraphIndexCodeWorkspaceInput {
    db_path: Option<String>,
    root: Option<String>,
    max_files: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GraphLinkNodesInput {
    db_path: Option<String>,
    src: String,
    dst: String,
    edge_kind: String,
    weight: Option<f64>,
    props: Option<Value>,
}

fn resolve_graph_db_path_in_directory(
    input_path: Option<&str>,
    working_directory: &Path,
) -> Result<PathBuf, String> {
    let path = if let Some(path) = input_path.filter(|path| !path.trim().is_empty()) {
        resolve_path_in_directory(working_directory, Path::new(path))
    } else if let Some(path) = std::env::var_os("AI_BRAIN_GRAPH_DB").filter(|path| !path.is_empty())
    {
        resolve_path_in_directory(working_directory, Path::new(&path))
    } else {
        default_graph_db_path()?
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    Ok(path)
}

fn default_graph_db_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| "cannot resolve graph db path: HOME/USERPROFILE not set".to_string())?;
    Ok(PathBuf::from(home)
        .join(".ai-brain")
        .join("graph")
        .join("graph.db"))
}

fn split_keywords(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_graph_type(value: &str) -> Result<GraphType, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "memory" => Ok(GraphType::Memory),
        "code" => Ok(GraphType::Code),
        "video" => Ok(GraphType::Video),
        other => Err(format!("unsupported graph_type: {other}")),
    }
}

fn parse_trace_direction(value: &str) -> Result<TraceDirection, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "upstream" => Ok(TraceDirection::Upstream),
        "downstream" => Ok(TraceDirection::Downstream),
        "both" => Ok(TraceDirection::Both),
        other => Err(format!("unsupported trace direction: {other}")),
    }
}

fn parse_edge_kind(value: &str) -> Result<EdgeKind, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "mentionedin" | "mentioned_in" => Ok(EdgeKind::MentionedIn),
        "relatedto" | "related_to" => Ok(EdgeKind::RelatedTo),
        "similarto" | "similar_to" => Ok(EdgeKind::SimilarTo),
        "causedby" | "caused_by" => Ok(EdgeKind::CausedBy),
        "dependson" | "depends_on" => Ok(EdgeKind::DependsOn),
        "derivedfrom" | "derived_from" => Ok(EdgeKind::DerivedFrom),
        "calls" => Ok(EdgeKind::Calls),
        "contains" => Ok(EdgeKind::Contains),
        "imports" => Ok(EdgeKind::Imports),
        "defines" => Ok(EdgeKind::Defines),
        "invokes" => Ok(EdgeKind::Invokes),
        other => Err(format!("unsupported edge_kind: {other}")),
    }
}

fn required_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(trimmed)
    }
}

fn optional_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn clean_string_list(values: Option<Vec<String>>) -> Option<Vec<String>> {
    let cleaned = values?
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn bounded_score(value: f64, field: &str) -> Result<f64, String> {
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{field} must be between 0.0 and 1.0"))
    }
}

#[derive(Debug, Serialize)]
struct WebFetchOutput {
    bytes: usize,
    code: u16,
    #[serde(rename = "codeText")]
    code_text: String,
    result: String,
    #[serde(rename = "durationMs")]
    duration_ms: u128,
    url: String,
}

#[derive(Debug, Serialize)]
struct WebSearchOutput {
    query: String,
    results: Vec<WebSearchResultItem>,
    #[serde(rename = "durationSeconds")]
    duration_seconds: f64,
}

#[derive(Debug, Serialize)]
struct TodoWriteOutput {
    #[serde(rename = "oldTodos")]
    old_todos: Vec<TodoItem>,
    #[serde(rename = "newTodos")]
    new_todos: Vec<TodoItem>,
    #[serde(rename = "verificationNudgeNeeded")]
    verification_nudge_needed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentOutput {
    #[serde(rename = "agentId")]
    agent_id: String,
    name: String,
    description: String,
    #[serde(rename = "subagentType")]
    subagent_type: Option<String>,
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(rename = "profileId")]
    profile_id: String,
    #[serde(rename = "profileVersion")]
    profile_version: u64,
    #[serde(rename = "contextSnapshotId")]
    context_snapshot_id: String,
    #[serde(rename = "instanceRunId")]
    instance_run_id: String,
    status: String,
    #[serde(rename = "outputFile")]
    output_file: String,
    #[serde(rename = "manifestFile")]
    manifest_file: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    #[serde(rename = "artifactId", skip_serializing_if = "Option::is_none")]
    artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<runtime::TokenUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iterations: Option<usize>,
}

struct AgentDone {
    manifest: AgentOutput,
    duration_ms: u64,
}

impl AgentCompletion {
    fn from_done(done: AgentDone) -> Self {
        Self {
            agent_id: done.manifest.agent_id,
            name: done.manifest.name,
            status: done.manifest.status,
            output: done.manifest.result.unwrap_or_default(),
            error: done.manifest.error,
            duration_ms: done.duration_ms,
        }
    }
}

struct AgentLaunch {
    manifest: AgentOutput,
    completion_rx: tokio::sync::oneshot::Receiver<AgentDone>,
    cancellation: tokio_util::sync::CancellationToken,
}

#[derive(Debug, Clone)]
struct PreparedAgent {
    manifest: AgentOutput,
    prompt: String,
    profile: AgentProfileSnapshot,
}

#[derive(Clone)]
struct FileAgentArtifactSink {
    manifest: AgentOutput,
}

impl ArtifactSink for FileAgentArtifactSink {
    fn store(&self, artifact: &ArtifactEnvelope) -> Result<(), AgentRuntimeError> {
        persist_agent_artifact(&self.manifest, artifact).map_err(AgentRuntimeError::ArtifactSink)
    }
}

#[derive(Debug, Serialize)]
struct ToolSearchOutput {
    matches: Vec<String>,
    query: String,
    normalized_query: String,
    #[serde(rename = "total_deferred_tools")]
    total_deferred_tools: usize,
    #[serde(rename = "pending_mcp_servers")]
    pending_mcp_servers: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct NotebookEditOutput {
    new_source: String,
    cell_id: Option<String>,
    cell_type: Option<NotebookCellType>,
    language: String,
    edit_mode: String,
    error: Option<String>,
    notebook_path: String,
    original_file: String,
    updated_file: String,
}

#[derive(Debug, Serialize)]
struct SleepOutput {
    duration_ms: u64,
    message: String,
}

#[derive(Debug, Serialize)]
struct BriefOutput {
    message: String,
    attachments: Option<Vec<ResolvedAttachment>>,
    #[serde(rename = "sentAt")]
    sent_at: String,
}

#[derive(Debug, Serialize)]
struct ResolvedAttachment {
    path: String,
    size: u64,
    #[serde(rename = "isImage")]
    is_image: bool,
}

#[derive(Debug, Serialize)]
struct ConfigOutput {
    success: bool,
    operation: Option<String>,
    setting: Option<String>,
    value: Option<Value>,
    #[serde(rename = "previousValue")]
    previous_value: Option<Value>,
    #[serde(rename = "newValue")]
    new_value: Option<Value>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanModeState {
    #[serde(rename = "hadLocalOverride")]
    had_local_override: bool,
    #[serde(rename = "previousLocalMode")]
    previous_local_mode: Option<Value>,
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct PlanModeOutput {
    success: bool,
    operation: String,
    changed: bool,
    active: bool,
    managed: bool,
    message: String,
    #[serde(rename = "settingsPath")]
    settings_path: String,
    #[serde(rename = "statePath")]
    state_path: String,
    #[serde(rename = "previousLocalMode")]
    previous_local_mode: Option<Value>,
    #[serde(rename = "currentLocalMode")]
    current_local_mode: Option<Value>,
}

#[derive(Debug, Serialize)]
struct StructuredOutputResult {
    data: String,
    structured_output: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct ReplOutput {
    language: String,
    stdout: String,
    stderr: String,
    #[serde(rename = "exitCode")]
    exit_code: i32,
    #[serde(rename = "durationMs")]
    duration_ms: u128,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum WebSearchResultItem {
    SearchResult {
        tool_use_id: String,
        content: Vec<SearchHit>,
    },
    Commentary(String),
}

#[derive(Debug, Serialize)]
struct SearchHit {
    title: String,
    url: String,
}

fn execute_web_fetch(input: &WebFetchInput) -> Result<WebFetchOutput, String> {
    let started = Instant::now();
    let client = build_http_client()?;
    let request_url = normalize_fetch_url(&input.url)?;
    let response = client
        .get(request_url.clone())
        .send()
        .map_err(|error| error.to_string())?;

    let status = response.status();
    let final_url = response.url().to_string();
    let code = status.as_u16();
    let code_text = status.canonical_reason().unwrap_or("Unknown").to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.text().map_err(|error| error.to_string())?;
    let bytes = body.len();
    let normalized = normalize_fetched_content(&body, &content_type);
    let result = summarize_web_fetch(&final_url, &input.prompt, &normalized, &body, &content_type);

    Ok(WebFetchOutput {
        bytes,
        code,
        code_text,
        result,
        duration_ms: started.elapsed().as_millis(),
        url: final_url,
    })
}

fn execute_web_search(input: &WebSearchInput) -> Result<WebSearchOutput, String> {
    let started = Instant::now();
    let client = build_http_client()?;
    let search_url = build_search_url(&input.query)?;
    let response = client
        .get(search_url)
        .send()
        .map_err(|error| error.to_string())?;

    let final_url = response.url().clone();
    let html = response.text().map_err(|error| error.to_string())?;
    let mut hits = extract_search_hits(&html);

    if hits.is_empty() && final_url.host_str().is_some() {
        hits = extract_search_hits_from_generic_links(&html);
    }

    if let Some(allowed) = input.allowed_domains.as_ref() {
        hits.retain(|hit| host_matches_list(&hit.url, allowed));
    }
    if let Some(blocked) = input.blocked_domains.as_ref() {
        hits.retain(|hit| !host_matches_list(&hit.url, blocked));
    }

    dedupe_hits(&mut hits);
    hits.truncate(8);

    let summary = if hits.is_empty() {
        format!("No web search results matched the query {:?}.", input.query)
    } else {
        let rendered_hits = hits
            .iter()
            .map(|hit| format!("- [{}]({})", hit.title, hit.url))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Search results for {:?}. Include a Sources section in the final answer.\n{}",
            input.query, rendered_hits
        )
    };

    Ok(WebSearchOutput {
        query: input.query.clone(),
        results: vec![
            WebSearchResultItem::Commentary(summary),
            WebSearchResultItem::SearchResult {
                tool_use_id: String::from("web_search_1"),
                content: hits,
            },
        ],
        duration_seconds: started.elapsed().as_secs_f64(),
    })
}

fn build_http_client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("clawd-rust-tools/0.1")
        .build()
        .map_err(|error| error.to_string())
}

fn normalize_fetch_url(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed.scheme() == "http" {
        let host = parsed.host_str().unwrap_or_default();
        if host != "localhost" && host != "127.0.0.1" && host != "::1" {
            let mut upgraded = parsed;
            upgraded
                .set_scheme("https")
                .map_err(|()| String::from("failed to upgrade URL to https"))?;
            return Ok(upgraded.to_string());
        }
    }
    Ok(parsed.to_string())
}

fn build_search_url(query: &str) -> Result<reqwest::Url, String> {
    if let Ok(base) = std::env::var("CLAWD_WEB_SEARCH_BASE_URL") {
        let mut url = reqwest::Url::parse(&base).map_err(|error| error.to_string())?;
        url.query_pairs_mut().append_pair("q", query);
        return Ok(url);
    }

    let mut url = reqwest::Url::parse("https://html.duckduckgo.com/html/")
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut().append_pair("q", query);
    Ok(url)
}

fn normalize_fetched_content(body: &str, content_type: &str) -> String {
    if content_type.contains("html") {
        html_to_text(body)
    } else {
        body.trim().to_string()
    }
}

fn summarize_web_fetch(
    url: &str,
    prompt: &str,
    content: &str,
    raw_body: &str,
    content_type: &str,
) -> String {
    let lower_prompt = prompt.to_lowercase();
    let compact = collapse_whitespace(content);

    let detail = if lower_prompt.contains("title") {
        extract_title(content, raw_body, content_type).map_or_else(
            || preview_text(&compact, 600),
            |title| format!("Title: {title}"),
        )
    } else if lower_prompt.contains("summary") || lower_prompt.contains("summarize") {
        preview_text(&compact, 900)
    } else {
        let preview = preview_text(&compact, 900);
        format!("Prompt: {prompt}\nContent preview:\n{preview}")
    };

    format!("Fetched {url}\n{detail}")
}

fn extract_title(content: &str, raw_body: &str, content_type: &str) -> Option<String> {
    if content_type.contains("html") {
        let lowered = raw_body.to_lowercase();
        if let Some(start) = lowered.find("<title>") {
            let after = start + "<title>".len();
            if let Some(end_rel) = lowered[after..].find("</title>") {
                let title =
                    collapse_whitespace(&decode_html_entities(&raw_body[after..after + end_rel]));
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
    }

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut previous_was_space = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            '&' => {
                text.push('&');
                previous_was_space = false;
            }
            ch if ch.is_whitespace() => {
                if !previous_was_space {
                    text.push(' ');
                    previous_was_space = true;
                }
            }
            _ => {
                text.push(ch);
                previous_was_space = false;
            }
        }
    }

    collapse_whitespace(&decode_html_entities(&text))
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn preview_text(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let shortened = input.chars().take(max_chars).collect::<String>();
    format!("{}…", shortened.trim_end())
}

fn extract_search_hits(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut remaining = html;

    while let Some(anchor_start) = remaining.find("result__a") {
        let after_class = &remaining[anchor_start..];
        let Some(href_idx) = after_class.find("href=") else {
            remaining = &after_class[1..];
            continue;
        };
        let href_slice = &after_class[href_idx + 5..];
        let Some((url, rest)) = extract_quoted_value(href_slice) else {
            remaining = &after_class[1..];
            continue;
        };
        let Some(close_tag_idx) = rest.find('>') else {
            remaining = &after_class[1..];
            continue;
        };
        let after_tag = &rest[close_tag_idx + 1..];
        let Some(end_anchor_idx) = after_tag.find("</a>") else {
            remaining = &after_tag[1..];
            continue;
        };
        let title = html_to_text(&after_tag[..end_anchor_idx]);
        if let Some(decoded_url) = decode_duckduckgo_redirect(&url) {
            hits.push(SearchHit {
                title: title.trim().to_string(),
                url: decoded_url,
            });
        }
        remaining = &after_tag[end_anchor_idx + 4..];
    }

    hits
}

fn extract_search_hits_from_generic_links(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut remaining = html;

    while let Some(anchor_start) = remaining.find("<a") {
        let after_anchor = &remaining[anchor_start..];
        let Some(href_idx) = after_anchor.find("href=") else {
            remaining = &after_anchor[2..];
            continue;
        };
        let href_slice = &after_anchor[href_idx + 5..];
        let Some((url, rest)) = extract_quoted_value(href_slice) else {
            remaining = &after_anchor[2..];
            continue;
        };
        let Some(close_tag_idx) = rest.find('>') else {
            remaining = &after_anchor[2..];
            continue;
        };
        let after_tag = &rest[close_tag_idx + 1..];
        let Some(end_anchor_idx) = after_tag.find("</a>") else {
            remaining = &after_anchor[2..];
            continue;
        };
        let title = html_to_text(&after_tag[..end_anchor_idx]);
        if title.trim().is_empty() {
            remaining = &after_tag[end_anchor_idx + 4..];
            continue;
        }
        let decoded_url = decode_duckduckgo_redirect(&url).unwrap_or(url);
        if decoded_url.starts_with("http://") || decoded_url.starts_with("https://") {
            hits.push(SearchHit {
                title: title.trim().to_string(),
                url: decoded_url,
            });
        }
        remaining = &after_tag[end_anchor_idx + 4..];
    }

    hits
}

fn extract_quoted_value(input: &str) -> Option<(String, &str)> {
    let quote = input.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &input[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some((rest[..end].to_string(), &rest[end + quote.len_utf8()..]))
}

fn decode_duckduckgo_redirect(url: &str) -> Option<String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(html_entity_decode_url(url));
    }

    let joined = if url.starts_with("//") {
        format!("https:{url}")
    } else if url.starts_with('/') {
        format!("https://duckduckgo.com{url}")
    } else {
        return None;
    };

    let parsed = reqwest::Url::parse(&joined).ok()?;
    if parsed.path() == "/l/" || parsed.path() == "/l" {
        for (key, value) in parsed.query_pairs() {
            if key == "uddg" {
                return Some(html_entity_decode_url(value.as_ref()));
            }
        }
    }
    Some(joined)
}

fn html_entity_decode_url(url: &str) -> String {
    decode_html_entities(url)
}

fn host_matches_list(url: &str, domains: &[String]) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    domains.iter().any(|domain| {
        let normalized = normalize_domain_filter(domain);
        !normalized.is_empty() && (host == normalized || host.ends_with(&format!(".{normalized}")))
    })
}

fn normalize_domain_filter(domain: &str) -> String {
    let trimmed = domain.trim();
    let candidate = reqwest::Url::parse(trimmed)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| trimmed.to_string());
    candidate
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn dedupe_hits(hits: &mut Vec<SearchHit>) {
    let mut seen = BTreeSet::new();
    hits.retain(|hit| seen.insert(hit.url.clone()));
}

fn execute_todo_write(
    input: TodoWriteInput,
    working_directory: &Path,
) -> Result<TodoWriteOutput, String> {
    validate_todos(&input.todos)?;
    let store_path = todo_store_path(working_directory);
    let old_todos = if store_path.exists() {
        serde_json::from_str::<Vec<TodoItem>>(
            &std::fs::read_to_string(&store_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };

    let all_done = input
        .todos
        .iter()
        .all(|todo| matches!(todo.status, TodoStatus::Completed));
    let persisted = if all_done {
        Vec::new()
    } else {
        input.todos.clone()
    };

    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        &store_path,
        serde_json::to_string_pretty(&persisted).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let verification_nudge_needed = (all_done
        && input.todos.len() >= 3
        && !input
            .todos
            .iter()
            .any(|todo| todo.content.to_lowercase().contains("verif")))
    .then_some(true);

    Ok(TodoWriteOutput {
        old_todos,
        new_todos: input.todos,
        verification_nudge_needed,
    })
}

fn validate_todos(todos: &[TodoItem]) -> Result<(), String> {
    if todos.is_empty() {
        return Err(String::from("todos must not be empty"));
    }
    // Allow multiple in_progress items for parallel workflows
    if todos.iter().any(|todo| todo.content.trim().is_empty()) {
        return Err(String::from("todo content must not be empty"));
    }
    if todos.iter().any(|todo| todo.active_form.trim().is_empty()) {
        return Err(String::from("todo activeForm must not be empty"));
    }
    Ok(())
}

fn todo_store_path(working_directory: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("CLAWD_TODO_STORE") {
        let path = PathBuf::from(path);
        return if path.is_absolute() {
            path
        } else {
            working_directory.join(path)
        };
    }
    working_directory.join(".clawd-todos.json")
}

const DEFAULT_AGENT_MAX_ITERATIONS: usize = 64;
const DEFAULT_AGENT_MAX_WORKERS: usize = 4;

/// 动态获取当前日期字符串（YYYY-MM-DD）
fn current_date_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn execute_agent_in_directory(
    input: AgentInput,
    working_directory: &Path,
) -> Result<AgentOutput, String> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(String::from(
            "synchronous Agent compatibility entry cannot run inside a Tokio task; use execute_agent_tool_with_completion",
        ));
    }
    agent_compat_runtime()?.block_on(async move {
        let run_in_background = input.run_in_background;
        let launch = start_agent_in_directory(input, working_directory).await?;
        if run_in_background {
            return Ok(launch.manifest);
        }
        launch
            .completion_rx
            .await
            .map(|done| done.manifest)
            .map_err(|_| String::from("sub-agent completion task closed unexpectedly"))
    })
}

fn agent_compat_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().map_err(|error| error.to_string()))
    {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(error.clone()),
    }
}

fn agent_worker_pool() -> &'static AgentWorkerPool {
    static POOL: OnceLock<AgentWorkerPool> = OnceLock::new();
    POOL.get_or_init(|| {
        AgentWorkerPool::new(DEFAULT_AGENT_MAX_WORKERS)
            .expect("default agent worker count must be non-zero")
    })
}

fn prepare_agent_in_directory(
    input: AgentInput,
    working_directory: &Path,
) -> Result<PreparedAgent, String> {
    if input.description.trim().is_empty() {
        return Err(String::from("description must not be empty"));
    }
    if input.prompt.trim().is_empty() {
        return Err(String::from("prompt must not be empty"));
    }
    let normalized_subagent_type = normalize_subagent_type(input.subagent_type.as_deref());
    let profile = build_agent_profile_in_directory(&normalized_subagent_type, working_directory)?;

    let agent_id = make_agent_id();
    let output_dir = agent_store_dir(working_directory);
    std::fs::create_dir_all(&output_dir).map_err(|error| error.to_string())?;
    let output_file = output_dir.join(format!("{agent_id}.md"));
    let manifest_file = output_dir.join(format!("{agent_id}.json"));
    let model = resolve_agent_model(
        input.model.as_deref(),
        &normalized_subagent_type,
        working_directory,
    );
    let agent_name = input
        .name
        .as_deref()
        .map(slugify_agent_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| slugify_agent_name(&input.description));
    let created_at = iso8601_now();
    let agent_prompt = input.prompt.clone();
    let context_snapshot_id = format!("context-{agent_id}");
    let instance_run_id = format!("run-{agent_id}");

    let output_contents = format!(
        "# Agent Task

- id: {}
- name: {}
- description: {}
- subagent_type: {}
- created_at: {}

## Prompt

{}
",
        agent_id, agent_name, input.description, normalized_subagent_type, created_at, agent_prompt
    );
    std::fs::write(&output_file, output_contents).map_err(|error| error.to_string())?;

    let manifest = AgentOutput {
        agent_id,
        name: agent_name,
        description: input.description,
        subagent_type: Some(normalized_subagent_type),
        model: Some(model),
        provider: None,
        profile_id: profile.profile_id.clone(),
        profile_version: profile.version,
        context_snapshot_id,
        instance_run_id,
        status: String::from("running"),
        output_file: output_file.display().to_string(),
        manifest_file: manifest_file.display().to_string(),
        created_at: created_at.clone(),
        started_at: Some(created_at),
        completed_at: None,
        error: None,
        result: None,
        artifact_id: None,
        usage: None,
        iterations: None,
    };
    write_agent_manifest(&manifest)?;

    Ok(PreparedAgent {
        manifest,
        prompt: agent_prompt,
        profile,
    })
}

async fn start_agent_in_directory(
    input: AgentInput,
    working_directory: &Path,
) -> Result<AgentLaunch, String> {
    let mut prepared = prepare_agent_in_directory(input, working_directory)?;
    let allowed_tools = prepared
        .profile
        .tool_grant
        .iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let client = ProviderRuntimeClient::new_in_directory(
        prepared.manifest.model.clone().unwrap_or_default(),
        allowed_tools.clone(),
        working_directory,
    )
    .map_err(|error| {
        let message = format!("failed to initialize sub-agent provider: {error}");
        let _ =
            persist_agent_terminal_state(&prepared.manifest, "failed", None, Some(message.clone()));
        message
    })?;
    let model = client.resolved_model_policy();
    prepared.manifest.model = Some(model.model.clone());
    prepared.manifest.provider = Some(model.provider.clone());
    write_agent_manifest(&prepared.manifest)?;

    let context_snapshot = ContextSnapshot::from_text(
        prepared.manifest.context_snapshot_id.clone(),
        prepared.prompt,
    )
    .map_err(|error| error.to_string())?;
    let spec = AgentRunSpec {
        agent_instance_id: prepared.manifest.agent_id.clone(),
        instance_run_id: prepared.manifest.instance_run_id.clone(),
        member_id: None,
        inbox_item_id: None,
        task_run_id: format!("legacy-task-{}", prepared.manifest.agent_id),
        node_id: "delegated-agent".into(),
        profile: prepared.profile,
        context_snapshot,
        input_artifacts: Vec::new(),
        model: model.clone(),
        reasoning: ReasoningPolicy::medium(),
        budget_reservation: BudgetReservation {
            reservation_id: format!("budget-{}", prepared.manifest.instance_run_id),
            max_input_tokens: u32::MAX,
            max_output_tokens: model.max_output_tokens,
        },
        deadline_unix_ms: None,
    };
    let cancellation = tokio_util::sync::CancellationToken::new();
    let lease = agent_worker_pool()
        .acquire(&cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let runtime = AgentRuntime::new(
        client,
        SubagentToolExecutor::new(allowed_tools, working_directory.to_path_buf()),
        agent_permission_policy(),
        FileAgentArtifactSink {
            manifest: prepared.manifest.clone(),
        },
    );
    let handle = runtime.spawn(spec, lease, cancellation.clone());
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    let initial_manifest = prepared.manifest.clone();
    tokio::spawn(async move {
        let outcome = handle.wait().await;
        let final_manifest = agent_manifest_from_outcome(&initial_manifest, &outcome);
        if outcome.status != AgentRunStatus::Completed {
            let _ = persist_agent_outcome(&initial_manifest, &outcome);
        }
        let _ = completion_tx.send(AgentDone {
            manifest: final_manifest,
            duration_ms: outcome.duration_ms,
        });
    });

    Ok(AgentLaunch {
        manifest: prepared.manifest,
        completion_rx,
        cancellation,
    })
}

fn build_agent_profile_in_directory(
    subagent_type: &str,
    working_directory: &Path,
) -> Result<AgentProfileSnapshot, String> {
    if !matches!(
        subagent_type,
        "general-purpose" | "Explore" | "Plan" | "Verification" | "claw-guide" | "statusline-setup"
    ) {
        return Err(format!("unsupported built-in agent role: {subagent_type}"));
    }
    let profile_slug = canonical_tool_token(subagent_type);
    AgentProfileSnapshot::new(
        format!(
            "builtin.{}",
            if profile_slug.is_empty() {
                "general"
            } else {
                &profile_slug
            }
        ),
        1,
        subagent_type,
        build_agent_system_prompt(subagent_type, working_directory)?,
        ToolGrant::new(allowed_tools_for_subagent(subagent_type)),
        OutputContract::Text,
        load_subagent_config(working_directory)
            .and_then(|config| config.max_iterations)
            .unwrap_or(DEFAULT_AGENT_MAX_ITERATIONS),
    )
    .map_err(|error| error.to_string())
}

fn build_agent_system_prompt(
    subagent_type: &str,
    working_directory: &Path,
) -> Result<Vec<String>, String> {
    // 子代理用轻量级系统提示词，不加载完整主脑 prompt（避免 60 万+ 字符撑爆弱模型上下文）
    let os = std::env::consts::OS;
    let today = current_date_str();
    let prompt = format!(
        "You are a background sub-agent of type `{subagent_type}`.\n\
         Current date: {today}\n\
         Operating system: {os}\n\
         Working directory: {}\n\n\
         Instructions:\n\
         - Work only on the delegated task described below.\n\
         - Use only the tools available to you.\n\
         - Do not ask the user questions.\n\
         - Finish with a concise result.",
        working_directory.display()
    );
    Ok(vec![prompt])
}

fn resolve_agent_model(
    model: Option<&str>,
    _subagent_type: &str,
    working_directory: &Path,
) -> String {
    // 优先使用调用者指定的模型，否则从子代理配置或主脑配置中获取
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        return m.to_string();
    }
    // 尝试从子代理配置获取
    if let Some(cfg) = load_subagent_config(working_directory) {
        if let Some(m) = cfg.model.filter(|m| !m.is_empty()) {
            return m;
        }
    }
    // 尝试从主脑配置获取 subagent brain model
    if let Ok(llm_config) = brain_llm::config::LlmConfig::load_default() {
        let m = llm_config.model_for_brain("subagent").to_string();
        if !m.is_empty() {
            return m;
        }
    }
    // 最终 fallback：使用通用默认值
    String::from("claude-sonnet-4-6")
}

fn allowed_tools_for_subagent(subagent_type: &str) -> BTreeSet<String> {
    let tools = match subagent_type {
        "Explore" => vec![
            "read_file",
            "write_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "StructuredOutput",
        ],
        "Plan" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "TodoWrite",
            "StructuredOutput",
            "SendUserMessage",
        ],
        "Verification" => vec![
            "bash",
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "TodoWrite",
            "StructuredOutput",
            "SendUserMessage",
            "PowerShell",
        ],
        "claw-guide" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "StructuredOutput",
            "SendUserMessage",
        ],
        "statusline-setup" => vec![
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "glob_search",
            "grep_search",
            "ToolSearch",
        ],
        _ => vec![
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "TodoWrite",
            "Skill",
            "ToolSearch",
            "NotebookEdit",
            "Sleep",
            "SendUserMessage",
            "Config",
            "StructuredOutput",
            "REPL",
            "PowerShell",
        ],
    };
    tools.into_iter().map(str::to_string).collect()
}

fn agent_permission_policy() -> PermissionPolicy {
    mvp_tool_specs().into_iter().fold(
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
    )
}

fn write_agent_manifest(manifest: &AgentOutput) -> Result<(), String> {
    std::fs::write(
        &manifest.manifest_file,
        serde_json::to_string_pretty(manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn persist_agent_terminal_state(
    manifest: &AgentOutput,
    status: &str,
    result: Option<&str>,
    error: Option<String>,
) -> Result<(), String> {
    append_agent_output(
        &manifest.output_file,
        &format_agent_terminal_output(status, result, error.as_deref()),
    )?;
    let mut next_manifest = manifest.clone();
    next_manifest.status = status.to_string();
    next_manifest.completed_at = Some(iso8601_now());
    next_manifest.error = error;
    next_manifest.result = result.map(str::to_string);
    write_agent_manifest(&next_manifest)
}

fn agent_manifest_from_outcome(manifest: &AgentOutput, outcome: &AgentRunOutcome) -> AgentOutput {
    let status = match outcome.status {
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        AgentRunStatus::Cancelled => "cancelled",
        AgentRunStatus::DeadlineExceeded => "deadline_exceeded",
    };
    let artifact = outcome.artifact.as_ref();
    AgentOutput {
        status: status.into(),
        completed_at: Some(iso8601_now()),
        error: outcome.error.clone(),
        result: artifact.map(|value| value.content.clone()),
        artifact_id: artifact.map(|value| value.artifact_id.clone()),
        usage: Some(outcome.usage),
        iterations: Some(outcome.iterations),
        ..manifest.clone()
    }
}

fn persist_agent_outcome(manifest: &AgentOutput, outcome: &AgentRunOutcome) -> Result<(), String> {
    let next_manifest = agent_manifest_from_outcome(manifest, outcome);
    append_agent_output(
        &manifest.output_file,
        &format_agent_terminal_output(
            &next_manifest.status,
            next_manifest.result.as_deref(),
            next_manifest.error.as_deref(),
        ),
    )?;
    write_agent_manifest(&next_manifest)
}

fn persist_agent_artifact(
    manifest: &AgentOutput,
    artifact: &ArtifactEnvelope,
) -> Result<(), String> {
    let next_manifest = AgentOutput {
        status: "completed".into(),
        completed_at: Some(iso8601_now()),
        error: None,
        result: Some(artifact.content.clone()),
        artifact_id: Some(artifact.artifact_id.clone()),
        usage: Some(artifact.usage),
        iterations: Some(artifact.iterations),
        ..manifest.clone()
    };
    append_agent_output(
        &manifest.output_file,
        &format_agent_terminal_output("completed", Some(&artifact.content), None),
    )?;
    write_agent_manifest(&next_manifest)
}

fn append_agent_output(path: &str, suffix: &str) -> Result<(), String> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(suffix.as_bytes())
        .map_err(|error| error.to_string())
}

fn format_agent_terminal_output(status: &str, result: Option<&str>, error: Option<&str>) -> String {
    let mut sections = vec![format!("\n## Result\n\n- status: {status}\n")];
    if let Some(result) = result.filter(|value| !value.trim().is_empty()) {
        sections.push(format!("\n### Final response\n\n{}\n", result.trim()));
    }
    if let Some(error) = error.filter(|value| !value.trim().is_empty()) {
        sections.push(format!("\n### Error\n\n{}\n", error.trim()));
    }
    sections.join("")
}

/// 子代理配置文件结构（从 .ai-brain/subagent.json 加载）
#[derive(Debug, Clone, Default, Deserialize)]
struct SubagentConfig {
    /// 可选的模型覆盖
    model: Option<String>,
    /// 可选的API地址覆盖
    base_url: Option<String>,
    /// 可选的最大迭代次数覆盖
    max_iterations: Option<usize>,
}

/// 加载子代理配置文件
fn load_subagent_config(working_directory: &Path) -> Option<SubagentConfig> {
    let config_path = working_directory.join(".ai-brain").join("subagent.json");

    let contents = fs::read_to_string(&config_path).ok()?;
    if contents.trim().is_empty() {
        return None;
    }

    serde_json::from_str(&contents).ok()
}

use brain_llm::config::LlmConfig;

struct ProviderRuntimeClient {
    runtime: tokio::runtime::Runtime,
    client: ProviderClient,
    provider: String,
    model: String,
    max_output_tokens: u32,
    temperature: f64,
    allowed_tools: BTreeSet<String>,
}

impl ProviderRuntimeClient {
    #[allow(clippy::needless_pass_by_value)]
    fn new_in_directory(
        model: String,
        allowed_tools: BTreeSet<String>,
        working_directory: &Path,
    ) -> Result<Self, String> {
        // 1. 加载主脑 LLM 配置（失败时使用默认配置，不阻断子代理启动）
        let llm_config = LlmConfig::load_default().unwrap_or_else(|e| {
            tracing::warn!("[子代理] 主脑配置加载失败，使用默认配置: {e}");
            LlmConfig::default_config()
        });
        let default_provider = &llm_config.llm.default_provider;
        tracing::debug!("[子代理] 主脑配置: provider={default_provider}");

        // 2. 加载子代理配置（可选覆盖）
        let subagent_config = load_subagent_config(working_directory).unwrap_or_default();

        // 3. 模型优先级：subagent.json 配置 > brain_models.subagent > 主脑默认
        //    子代理需要 tool calling 支持，默认模型不一定兼容
        let model = subagent_config
            .model
            .as_deref()
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .or_else(|| (!model.trim().is_empty()).then(|| model.trim().to_string()))
            .unwrap_or_else(|| llm_config.model_for_brain("subagent").to_string());
        tracing::debug!("[子代理] 使用模型: {model}");

        // 4. 提供商选择：多策略匹配
        //    策略1: 模型名以提供商名开头（如 deepseek-chat → deepseek）
        //    策略2: 已知模型→提供商映射（如 glm-* → zhipu, grok-* → xai）
        //    找不到时回退到 default_provider
        let provider_name = llm_config
            .llm
            .providers
            .keys()
            .find(|name| model.starts_with(name.as_str()))
            .or_else(|| {
                // 已知模型前缀→提供商映射
                let known_mappings: &[(&str, &str)] = &[
                    ("glm", "zhipu"),
                    ("grok", "xai"),
                    ("gpt", "openai"),
                    ("claude", "anthropic"),
                    ("deepseek", "deepseek"),
                    ("qwen", "alibaba"),
                    ("gemini", "google"),
                ];
                known_mappings
                    .iter()
                    .find(|(prefix, _)| model.starts_with(prefix))
                    .and_then(|(_, provider)| {
                        llm_config
                            .llm
                            .providers
                            .keys()
                            .find(|k| k.as_str() == *provider)
                    })
            })
            .map(String::as_str)
            .unwrap_or(default_provider);
        tracing::debug!("[子代理] 选择提供商: {provider_name}");

        let provider_config = llm_config
            .llm
            .providers
            .get(provider_name)
            .ok_or_else(|| format!("提供商配置不存在: {provider_name}"))?;

        let api_key = llm_config
            .resolve_api_key(provider_name)
            .map_err(|e| format!("无法获取 API Key (provider={provider_name}): {e}"))?;

        // 5. 创建 OpenAI 兼容客户端
        let openai_config = api::OpenAiCompatConfig {
            provider_name: "subagent",
            api_key_env: "",
            base_url_env: "",
            default_base_url: "",
        };
        let mut api_base = provider_config.api_base.clone();
        // 子代理配置可覆盖 base_url
        if let Some(base_url) = &subagent_config.base_url {
            api_base = base_url.clone();
        }
        let openai_client =
            api::OpenAiCompatClient::new(api_key, openai_config).with_base_url(api_base.clone());
        let client = api::ProviderClient::OpenAi(openai_client);
        tracing::debug!("[子代理] 客户端就绪: model={model}, api_base={api_base}");
        let max_output_tokens = max_tokens_for_model(&model);
        let (_, temperature) = llm_config.params_for_brain("subagent");

        Ok(Self {
            runtime: tokio::runtime::Runtime::new().map_err(|error| error.to_string())?,
            client,
            provider: provider_name.to_string(),
            model,
            max_output_tokens,
            temperature,
            allowed_tools,
        })
    }

    fn resolved_model_policy(&self) -> ResolvedModelPolicy {
        ResolvedModelPolicy {
            policy_id: "subagent".into(),
            label: "subagent".into(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            max_output_tokens: self.max_output_tokens,
            temperature: self.temperature,
        }
    }
}

impl ApiClient for ProviderRuntimeClient {
    #[allow(clippy::too_many_lines)]
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let tools = tool_specs_for_allowed_tools(Some(&self.allowed_tools))
            .into_iter()
            .map(|spec| ToolDefinition {
                name: spec.name.to_string(),
                description: Some(spec.description.to_string()),
                input_schema: spec.input_schema,
            })
            .collect::<Vec<_>>();
        let message_request = MessageRequest {
            model: self.model.clone(),
            max_tokens: self.max_output_tokens,
            messages: convert_messages(&request.messages),
            system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n")),
            tools: (!tools.is_empty()).then_some(tools),
            tool_choice: (!self.allowed_tools.is_empty()).then_some(ToolChoice::Auto),
            stream: true,
        };

        // 诊断：打印请求摘要
        tracing::info!(
            "[子代理] API 请求: model={}, max_tokens={}, msgs={}, tools={}, system={}",
            message_request.model,
            message_request.max_tokens,
            message_request.messages.len(),
            message_request.tools.as_ref().map(|t| t.len()).unwrap_or(0),
            message_request
                .system
                .as_ref()
                .map(|s| s.len())
                .unwrap_or(0),
        );
        for (i, msg) in message_request.messages.iter().enumerate() {
            tracing::info!(
                "[子代理]   msg[{i}] role={}, blocks={}",
                msg.role,
                msg.content.len()
            );
        }
        // 诊断：多轮调用时打印详细消息结构（排查 400 错误）
        if message_request.messages.len() > 1 {
            for (i, msg) in message_request.messages.iter().enumerate() {
                for (j, block) in msg.content.iter().enumerate() {
                    match block {
                        InputContentBlock::Text { text } => {
                            tracing::info!(
                                "[子代理]     msg[{i}].block[{j}] Text ({}chars)",
                                text.len()
                            );
                        }
                        InputContentBlock::Thinking { thinking } => {
                            tracing::info!(
                                "[子代理]     msg[{i}].block[{j}] Thinking ({}chars)",
                                thinking.len()
                            );
                        }
                        InputContentBlock::ToolUse { id, name, input } => {
                            tracing::info!("[子代理]     msg[{i}].block[{j}] ToolUse id={}, name={}, input={}chars", id, name, input.to_string().len());
                        }
                        InputContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            tracing::info!("[子代理]     msg[{i}].block[{j}] ToolResult tool_use_id={}, content_items={}, is_error={}", tool_use_id, content.len(), is_error);
                        }
                    }
                }
            }
        }

        self.runtime.block_on(async {
            // 重试机制：最多 3 次，指数退避（1s, 2s, 4s）
            let max_retries = 3u32;
            let mut stream = None;
            for attempt in 0..max_retries {
                match self.client.stream_message(&message_request).await {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(error) => {
                        let err_msg = error.to_string();
                        let is_retryable = err_msg.contains("429")
                            || err_msg.contains("500")
                            || err_msg.contains("502")
                            || err_msg.contains("503")
                            || err_msg.contains("timeout")
                            || err_msg.contains("connection")
                            || err_msg.contains("network");
                        if is_retryable && attempt + 1 < max_retries {
                            let wait_ms = 1000u64 * 2u64.pow(attempt);
                            tracing::warn!(
                                "[子代理] API 调用失败 (attempt {}/{}): {}，{}ms 后重试",
                                attempt + 1,
                                max_retries,
                                err_msg,
                                wait_ms
                            );
                            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
                            continue;
                        }
                        return Err(RuntimeError::new(err_msg));
                    }
                }
            }
            let mut stream = stream.ok_or_else(|| RuntimeError::new("重试次数已耗尽"))?;
            let mut events = Vec::new();
            let mut pending_tools: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
            let mut saw_stop = false;

            while let Some(event) = stream
                .next_event()
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?
            {
                match event {
                    ApiStreamEvent::MessageStart(start) => {
                        for block in start.message.content {
                            push_output_block(block, 0, &mut events, &mut pending_tools, true);
                        }
                    }
                    ApiStreamEvent::ContentBlockStart(start) => {
                        push_output_block(
                            start.content_block,
                            start.index,
                            &mut events,
                            &mut pending_tools,
                            true,
                        );
                    }
                    ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                        ContentBlockDelta::TextDelta { text } => {
                            if !text.is_empty() {
                                events.push(AssistantEvent::TextDelta(text));
                            }
                        }
                        ContentBlockDelta::InputJsonDelta { partial_json } => {
                            if let Some((_, _, input)) = pending_tools.get_mut(&delta.index) {
                                input.push_str(&partial_json);
                            }
                        }
                        ContentBlockDelta::ThinkingDelta { thinking } => {
                            if !thinking.is_empty() {
                                events.push(AssistantEvent::ThinkingDelta(thinking));
                            }
                        }
                        ContentBlockDelta::SignatureDelta { .. } => {}
                    },
                    ApiStreamEvent::ContentBlockStop(stop) => {
                        if let Some((id, name, input)) = pending_tools.remove(&stop.index) {
                            events.push(AssistantEvent::ToolUse { id, name, input });
                        }
                    }
                    ApiStreamEvent::MessageDelta(delta) => {
                        events.push(AssistantEvent::Usage(delta.usage.token_usage()));
                    }
                    ApiStreamEvent::MessageStop(_) => {
                        saw_stop = true;
                        events.push(AssistantEvent::MessageStop);
                    }
                }
            }

            push_prompt_cache_record(&self.client, &mut events);

            if !saw_stop
                && events.iter().any(|event| {
                    matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                        || matches!(event, AssistantEvent::ToolUse { .. })
                })
            {
                events.push(AssistantEvent::MessageStop);
            }

            if events
                .iter()
                .any(|event| matches!(event, AssistantEvent::MessageStop))
            {
                return Ok(events);
            }

            let response = self
                .client
                .send_message(&MessageRequest {
                    stream: false,
                    ..message_request.clone()
                })
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))?;
            let mut events = response_to_events(response);
            push_prompt_cache_record(&self.client, &mut events);
            Ok(events)
        })
    }
}

struct SubagentToolExecutor {
    allowed_tools: BTreeSet<String>,
    working_directory: PathBuf,
}

impl SubagentToolExecutor {
    fn new(allowed_tools: BTreeSet<String>, working_directory: PathBuf) -> Self {
        Self {
            allowed_tools,
            working_directory,
        }
    }
}

impl ToolExecutor for SubagentToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if !self.allowed_tools.contains(tool_name) {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled for this sub-agent"
            )));
        }
        let value: Value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        execute_tool_in_directory(tool_name, &value, &self.working_directory)
            .map_err(ToolError::new)
    }
}

fn tool_specs_for_allowed_tools(allowed_tools: Option<&BTreeSet<String>>) -> Vec<ToolSpec> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| allowed_tools.is_none_or(|allowed| allowed.contains(spec.name)))
        .collect()
}

fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => {
                        Some(InputContentBlock::Text { text: text.clone() })
                    }
                    ContentBlock::Thinking { thinking } => Some(InputContentBlock::Thinking {
                        thinking: thinking.clone(),
                    }),
                    ContentBlock::ToolUse { id, name, input } => Some(InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    }),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => Some(InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    }),
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

fn push_output_block(
    block: OutputContentBlock,
    block_index: u32,
    events: &mut Vec<AssistantEvent>,
    pending_tools: &mut BTreeMap<u32, (String, String, String)>,
    streaming_tool_input: bool,
) {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            pending_tools.insert(block_index, (id, name, initial_input));
        }
        OutputContentBlock::Thinking { thinking, .. } => {
            if !thinking.is_empty() {
                events.push(AssistantEvent::ThinkingDelta(thinking));
            }
        }
        OutputContentBlock::RedactedThinking { .. } => {}
    }
}

fn response_to_events(response: MessageResponse) -> Vec<AssistantEvent> {
    let mut events = Vec::new();
    let mut pending_tools = BTreeMap::new();

    for (index, block) in response.content.into_iter().enumerate() {
        let index = u32::try_from(index).expect("response block index overflow");
        push_output_block(block, index, &mut events, &mut pending_tools, false);
        if let Some((id, name, input)) = pending_tools.remove(&index) {
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    events.push(AssistantEvent::Usage(response.usage.token_usage()));
    events.push(AssistantEvent::MessageStop);
    events
}

fn push_prompt_cache_record(client: &ProviderClient, events: &mut Vec<AssistantEvent>) {
    if let Some(record) = client.take_last_prompt_cache_record() {
        if let Some(event) = prompt_cache_record_to_runtime_event(record) {
            events.push(AssistantEvent::PromptCache(event));
        }
    }
}

fn prompt_cache_record_to_runtime_event(
    record: api::PromptCacheRecord,
) -> Option<PromptCacheEvent> {
    let cache_break = record.cache_break?;
    Some(PromptCacheEvent {
        unexpected: cache_break.unexpected,
        reason: cache_break.reason,
        previous_cache_read_input_tokens: cache_break.previous_cache_read_input_tokens,
        current_cache_read_input_tokens: cache_break.current_cache_read_input_tokens,
        token_drop: cache_break.token_drop,
    })
}

#[cfg(test)]
fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    summary
        .assistant_messages
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[allow(clippy::needless_pass_by_value)]
fn execute_tool_search(input: ToolSearchInput) -> ToolSearchOutput {
    let deferred = deferred_tool_specs();
    let max_results = input.max_results.unwrap_or(5).max(1);
    let query = input.query.trim().to_string();
    let normalized_query = normalize_tool_search_query(&query);
    let matches = search_tool_specs(&query, max_results, &deferred);

    ToolSearchOutput {
        matches,
        query,
        normalized_query,
        total_deferred_tools: deferred.len(),
        pending_mcp_servers: None,
    }
}

/// 基础工具 — 始终注册，不过载模型决策空间
///
/// 只包含 6 个核心工具：bash, read_file, write_file, edit_file, glob_search, grep_search
/// 其余工具通过 ToolSearch 按需加载（仿 Claude Code 的分层工具加载机制）。
#[must_use]
pub fn base_tool_specs() -> Vec<ToolSpec> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| {
            matches!(
                spec.name,
                "bash"
                    | "read_file"
                    | "write_file"
                    | "edit_file"
                    | "glob_search"
                    | "grep_search"
                    | "Agent"
            )
        })
        .collect()
}

/// 延迟加载工具 — 不默认注册，通过 ToolSearch 按需发现
#[must_use]
pub fn deferred_tool_specs() -> Vec<ToolSpec> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| {
            !matches!(
                spec.name,
                "bash"
                    | "read_file"
                    | "write_file"
                    | "edit_file"
                    | "glob_search"
                    | "grep_search"
                    | "Agent"
            )
        })
        .collect()
}

fn search_tool_specs(query: &str, max_results: usize, specs: &[ToolSpec]) -> Vec<String> {
    let lowered = query.to_lowercase();
    if let Some(selection) = lowered.strip_prefix("select:") {
        return selection
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .filter_map(|wanted| {
                let wanted = canonical_tool_token(wanted);
                specs
                    .iter()
                    .find(|spec| canonical_tool_token(spec.name) == wanted)
                    .map(|spec| spec.name.to_string())
            })
            .take(max_results)
            .collect();
    }

    let mut required = Vec::new();
    let mut optional = Vec::new();
    for term in lowered.split_whitespace() {
        if let Some(rest) = term.strip_prefix('+') {
            if !rest.is_empty() {
                required.push(rest);
            }
        } else {
            optional.push(term);
        }
    }
    let terms = if required.is_empty() {
        optional.clone()
    } else {
        required.iter().chain(optional.iter()).copied().collect()
    };

    let mut scored = specs
        .iter()
        .filter_map(|spec| {
            let name = spec.name.to_lowercase();
            let canonical_name = canonical_tool_token(spec.name);
            let normalized_description = normalize_tool_search_query(spec.description);
            let haystack = format!(
                "{name} {} {canonical_name}",
                spec.description.to_lowercase()
            );
            let normalized_haystack = format!("{canonical_name} {normalized_description}");
            if required.iter().any(|term| !haystack.contains(term)) {
                return None;
            }

            let mut score = 0_i32;
            for term in &terms {
                let canonical_term = canonical_tool_token(term);
                if haystack.contains(term) {
                    score += 2;
                }
                if name == *term {
                    score += 8;
                }
                if name.contains(term) {
                    score += 4;
                }
                if canonical_name == canonical_term {
                    score += 12;
                }
                if normalized_haystack.contains(&canonical_term) {
                    score += 3;
                }
            }

            if score == 0 && !lowered.is_empty() {
                return None;
            }
            Some((score, spec.name.to_string()))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored
        .into_iter()
        .map(|(_, name)| name)
        .take(max_results)
        .collect()
}

fn normalize_tool_search_query(query: &str) -> String {
    query
        .trim()
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|term| !term.is_empty())
        .map(canonical_tool_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonical_tool_token(value: &str) -> String {
    let mut canonical = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if let Some(stripped) = canonical.strip_suffix("tool") {
        canonical = stripped.to_string();
    }
    canonical
}

fn agent_store_dir(working_directory: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("CLAWD_AGENT_STORE") {
        return resolve_path_in_directory(working_directory, Path::new(&path));
    }
    working_directory.join(".clawd-agents")
}

fn make_agent_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("agent-{nanos}")
}

fn slugify_agent_name(description: &str) -> String {
    let mut out = description
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(32).collect()
}

fn normalize_subagent_type(subagent_type: Option<&str>) -> String {
    let trimmed = subagent_type.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return String::from("general-purpose");
    }
    match canonical_tool_token(trimmed).as_str() {
        "general" | "generalpurpose" | "generalpurposeagent" => String::from("general-purpose"),
        "explore" | "explorer" | "exploreagent" => String::from("Explore"),
        "plan" | "planagent" => String::from("Plan"),
        "verification" | "verificationagent" | "verify" | "verifier" => {
            String::from("Verification")
        }
        "clawguide" | "clawguideagent" | "guide" => String::from("claw-guide"),
        "statusline" | "statuslinesetup" => String::from("statusline-setup"),
        _ => trimmed.to_string(),
    }
}

fn iso8601_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[allow(clippy::too_many_lines)]
fn execute_notebook_edit(
    input: NotebookEditInput,
    working_directory: &Path,
) -> Result<NotebookEditOutput, String> {
    let path = resolve_path_in_directory(working_directory, Path::new(&input.notebook_path));
    if path.extension().and_then(|ext| ext.to_str()) != Some("ipynb") {
        return Err(String::from(
            "File must be a Jupyter notebook (.ipynb file).",
        ));
    }

    let original_file = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let mut notebook: serde_json::Value =
        serde_json::from_str(&original_file).map_err(|error| error.to_string())?;
    let language = notebook
        .get("metadata")
        .and_then(|metadata| metadata.get("kernelspec"))
        .and_then(|kernelspec| kernelspec.get("language"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("python")
        .to_string();
    let cells = notebook
        .get_mut("cells")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| String::from("Notebook cells array not found"))?;

    let edit_mode = input.edit_mode.unwrap_or(NotebookEditMode::Replace);
    let target_index = match input.cell_id.as_deref() {
        Some(cell_id) => Some(resolve_cell_index(cells, Some(cell_id), edit_mode)?),
        None if matches!(
            edit_mode,
            NotebookEditMode::Replace | NotebookEditMode::Delete
        ) =>
        {
            Some(resolve_cell_index(cells, None, edit_mode)?)
        }
        None => None,
    };
    let resolved_cell_type = match edit_mode {
        NotebookEditMode::Delete => None,
        NotebookEditMode::Insert => Some(input.cell_type.unwrap_or(NotebookCellType::Code)),
        NotebookEditMode::Replace => Some(input.cell_type.unwrap_or_else(|| {
            target_index
                .and_then(|index| cells.get(index))
                .and_then(cell_kind)
                .unwrap_or(NotebookCellType::Code)
        })),
    };
    let new_source = require_notebook_source(input.new_source, edit_mode)?;

    let cell_id = match edit_mode {
        NotebookEditMode::Insert => {
            let resolved_cell_type = resolved_cell_type
                .ok_or_else(|| String::from("insert mode requires a cell type"))?;
            let new_id = make_cell_id(cells.len());
            let new_cell = build_notebook_cell(&new_id, resolved_cell_type, &new_source);
            let insert_at = target_index.map_or(cells.len(), |index| index + 1);
            cells.insert(insert_at, new_cell);
            cells
                .get(insert_at)
                .and_then(|cell| cell.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
        NotebookEditMode::Delete => {
            let idx = target_index
                .ok_or_else(|| String::from("delete mode requires a target cell index"))?;
            let removed = cells.remove(idx);
            removed
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
        NotebookEditMode::Replace => {
            let resolved_cell_type = resolved_cell_type
                .ok_or_else(|| String::from("replace mode requires a cell type"))?;
            let idx = target_index
                .ok_or_else(|| String::from("replace mode requires a target cell index"))?;
            let cell = cells
                .get_mut(idx)
                .ok_or_else(|| String::from("Cell index out of range"))?;
            cell["source"] = serde_json::Value::Array(source_lines(&new_source));
            cell["cell_type"] = serde_json::Value::String(match resolved_cell_type {
                NotebookCellType::Code => String::from("code"),
                NotebookCellType::Markdown => String::from("markdown"),
            });
            match resolved_cell_type {
                NotebookCellType::Code => {
                    if !cell.get("outputs").is_some_and(serde_json::Value::is_array) {
                        cell["outputs"] = json!([]);
                    }
                    if cell.get("execution_count").is_none() {
                        cell["execution_count"] = serde_json::Value::Null;
                    }
                }
                NotebookCellType::Markdown => {
                    if let Some(object) = cell.as_object_mut() {
                        object.remove("outputs");
                        object.remove("execution_count");
                    }
                }
            }
            cell.get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
    };

    let updated_file =
        serde_json::to_string_pretty(&notebook).map_err(|error| error.to_string())?;
    std::fs::write(&path, &updated_file).map_err(|error| error.to_string())?;

    Ok(NotebookEditOutput {
        new_source,
        cell_id,
        cell_type: resolved_cell_type,
        language,
        edit_mode: format_notebook_edit_mode(edit_mode),
        error: None,
        notebook_path: path.display().to_string(),
        original_file,
        updated_file,
    })
}

fn require_notebook_source(
    source: Option<String>,
    edit_mode: NotebookEditMode,
) -> Result<String, String> {
    match edit_mode {
        NotebookEditMode::Delete => Ok(source.unwrap_or_default()),
        NotebookEditMode::Insert | NotebookEditMode::Replace => source
            .ok_or_else(|| String::from("new_source is required for insert and replace edits")),
    }
}

fn build_notebook_cell(cell_id: &str, cell_type: NotebookCellType, source: &str) -> Value {
    let mut cell = json!({
        "cell_type": match cell_type {
            NotebookCellType::Code => "code",
            NotebookCellType::Markdown => "markdown",
        },
        "id": cell_id,
        "metadata": {},
        "source": source_lines(source),
    });
    if let Some(object) = cell.as_object_mut() {
        match cell_type {
            NotebookCellType::Code => {
                object.insert(String::from("outputs"), json!([]));
                object.insert(String::from("execution_count"), Value::Null);
            }
            NotebookCellType::Markdown => {}
        }
    }
    cell
}

fn cell_kind(cell: &serde_json::Value) -> Option<NotebookCellType> {
    cell.get("cell_type")
        .and_then(serde_json::Value::as_str)
        .map(|kind| {
            if kind == "markdown" {
                NotebookCellType::Markdown
            } else {
                NotebookCellType::Code
            }
        })
}

const MAX_SLEEP_DURATION_MS: u64 = 300_000;

#[allow(clippy::needless_pass_by_value)]
fn execute_sleep(input: SleepInput) -> Result<SleepOutput, String> {
    if input.duration_ms > MAX_SLEEP_DURATION_MS {
        return Err(format!(
            "duration_ms {} exceeds maximum allowed sleep of {MAX_SLEEP_DURATION_MS}ms",
            input.duration_ms,
        ));
    }
    std::thread::sleep(Duration::from_millis(input.duration_ms));
    Ok(SleepOutput {
        duration_ms: input.duration_ms,
        message: format!("Slept for {}ms", input.duration_ms),
    })
}

fn execute_brief(input: BriefInput) -> Result<BriefOutput, String> {
    if input.message.trim().is_empty() {
        return Err(String::from("message must not be empty"));
    }

    let attachments = input
        .attachments
        .as_ref()
        .map(|paths| {
            paths
                .iter()
                .map(|path| resolve_attachment(path))
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;

    let message = match input.status {
        BriefStatus::Normal | BriefStatus::Proactive => input.message,
    };

    Ok(BriefOutput {
        message,
        attachments,
        sent_at: iso8601_timestamp(),
    })
}

fn resolve_attachment(path: &str) -> Result<ResolvedAttachment, String> {
    let resolved = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&resolved).map_err(|error| error.to_string())?;
    Ok(ResolvedAttachment {
        path: resolved.display().to_string(),
        size: metadata.len(),
        is_image: is_image_path(&resolved),
    })
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    )
}

fn execute_config(input: ConfigInput, working_directory: &Path) -> Result<ConfigOutput, String> {
    let setting = input.setting.trim();
    if setting.is_empty() {
        return Err(String::from("setting must not be empty"));
    }
    let Some(spec) = supported_config_setting(setting) else {
        return Ok(ConfigOutput {
            success: false,
            operation: None,
            setting: None,
            value: None,
            previous_value: None,
            new_value: None,
            error: Some(format!("Unknown setting: \"{setting}\"")),
        });
    };

    let path = config_file_for_scope(spec.scope, working_directory)?;
    let mut document = read_json_object(&path)?;

    if let Some(value) = input.value {
        let normalized = normalize_config_value(spec, value)?;
        let previous_value = get_nested_value(&document, spec.path).cloned();
        set_nested_value(&mut document, spec.path, normalized.clone());
        write_json_object(&path, &document)?;
        Ok(ConfigOutput {
            success: true,
            operation: Some(String::from("set")),
            setting: Some(setting.to_string()),
            value: Some(normalized.clone()),
            previous_value,
            new_value: Some(normalized),
            error: None,
        })
    } else {
        Ok(ConfigOutput {
            success: true,
            operation: Some(String::from("get")),
            setting: Some(setting.to_string()),
            value: get_nested_value(&document, spec.path).cloned(),
            previous_value: None,
            new_value: None,
            error: None,
        })
    }
}

const PERMISSION_DEFAULT_MODE_PATH: &[&str] = &["permissions", "defaultMode"];

fn execute_enter_plan_mode(
    _input: EnterPlanModeInput,
    working_directory: &Path,
) -> Result<PlanModeOutput, String> {
    let settings_path = config_file_for_scope(ConfigScope::Settings, working_directory)?;
    let state_path = plan_mode_state_file(working_directory)?;
    let mut document = read_json_object(&settings_path)?;
    let current_local_mode = get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned();
    let current_is_plan =
        matches!(current_local_mode.as_ref(), Some(Value::String(value)) if value == "plan");

    if let Some(state) = read_plan_mode_state(&state_path)? {
        if current_is_plan {
            return Ok(PlanModeOutput {
                success: true,
                operation: String::from("enter"),
                changed: false,
                active: true,
                managed: true,
                message: String::from("Plan mode override is already active for this worktree."),
                settings_path: settings_path.display().to_string(),
                state_path: state_path.display().to_string(),
                previous_local_mode: state.previous_local_mode,
                current_local_mode,
            });
        }
        clear_plan_mode_state(&state_path)?;
    }

    if current_is_plan {
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("enter"),
            changed: false,
            active: true,
            managed: false,
            message: String::from(
                "Worktree-local plan mode is already enabled outside EnterPlanMode; leaving it unchanged.",
            ),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: None,
            current_local_mode,
        });
    }

    let state = PlanModeState {
        had_local_override: current_local_mode.is_some(),
        previous_local_mode: current_local_mode.clone(),
    };
    write_plan_mode_state(&state_path, &state)?;
    set_nested_value(
        &mut document,
        PERMISSION_DEFAULT_MODE_PATH,
        Value::String(String::from("plan")),
    );
    write_json_object(&settings_path, &document)?;

    Ok(PlanModeOutput {
        success: true,
        operation: String::from("enter"),
        changed: true,
        active: true,
        managed: true,
        message: String::from("Enabled worktree-local plan mode override."),
        settings_path: settings_path.display().to_string(),
        state_path: state_path.display().to_string(),
        previous_local_mode: state.previous_local_mode,
        current_local_mode: get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned(),
    })
}

fn execute_exit_plan_mode(
    _input: ExitPlanModeInput,
    working_directory: &Path,
) -> Result<PlanModeOutput, String> {
    let settings_path = config_file_for_scope(ConfigScope::Settings, working_directory)?;
    let state_path = plan_mode_state_file(working_directory)?;
    let mut document = read_json_object(&settings_path)?;
    let current_local_mode = get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned();
    let current_is_plan =
        matches!(current_local_mode.as_ref(), Some(Value::String(value)) if value == "plan");

    let Some(state) = read_plan_mode_state(&state_path)? else {
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("exit"),
            changed: false,
            active: current_is_plan,
            managed: false,
            message: String::from("No EnterPlanMode override is active for this worktree."),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: None,
            current_local_mode,
        });
    };

    if !current_is_plan {
        clear_plan_mode_state(&state_path)?;
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("exit"),
            changed: false,
            active: false,
            managed: false,
            message: String::from(
                "Cleared stale EnterPlanMode state because plan mode was already changed outside the tool.",
            ),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: state.previous_local_mode,
            current_local_mode,
        });
    }

    if state.had_local_override {
        if let Some(previous_local_mode) = state.previous_local_mode.clone() {
            set_nested_value(
                &mut document,
                PERMISSION_DEFAULT_MODE_PATH,
                previous_local_mode,
            );
        } else {
            remove_nested_value(&mut document, PERMISSION_DEFAULT_MODE_PATH);
        }
    } else {
        remove_nested_value(&mut document, PERMISSION_DEFAULT_MODE_PATH);
    }
    write_json_object(&settings_path, &document)?;
    clear_plan_mode_state(&state_path)?;

    Ok(PlanModeOutput {
        success: true,
        operation: String::from("exit"),
        changed: true,
        active: false,
        managed: false,
        message: String::from("Restored the prior worktree-local plan mode setting."),
        settings_path: settings_path.display().to_string(),
        state_path: state_path.display().to_string(),
        previous_local_mode: state.previous_local_mode,
        current_local_mode: get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned(),
    })
}

fn execute_structured_output(
    input: StructuredOutputInput,
) -> Result<StructuredOutputResult, String> {
    if input.0.is_empty() {
        return Err(String::from("structured output payload must not be empty"));
    }
    Ok(StructuredOutputResult {
        data: String::from("Structured output provided successfully"),
        structured_output: input.0,
    })
}

fn execute_repl(input: ReplInput, working_directory: &Path) -> Result<ReplOutput, String> {
    if input.code.trim().is_empty() {
        return Err(String::from("code must not be empty"));
    }
    let runtime = resolve_repl_runtime(&input.language)?;
    let started = Instant::now();
    let mut process = build_repl_command(
        runtime.program,
        runtime.args,
        &input.code,
        working_directory,
    );
    process
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = if let Some(timeout_ms) = input.timeout_ms {
        let mut child = process.spawn().map_err(|error| error.to_string())?;
        loop {
            if child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                break child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?;
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                child.kill().map_err(|error| error.to_string())?;
                child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "REPL execution exceeded timeout of {timeout_ms} ms"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    } else {
        process
            .spawn()
            .map_err(|error| error.to_string())?
            .wait_with_output()
            .map_err(|error| error.to_string())?
    };

    Ok(ReplOutput {
        language: input.language,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(1),
        duration_ms: started.elapsed().as_millis(),
    })
}

fn build_repl_command(
    program: &str,
    args: &[&str],
    code: &str,
    working_directory: &Path,
) -> Command {
    let mut process = Command::new(program);
    process.args(args).arg(code).current_dir(working_directory);
    process
}

struct ReplRuntime {
    program: &'static str,
    args: &'static [&'static str],
}

fn resolve_repl_runtime(language: &str) -> Result<ReplRuntime, String> {
    match language.trim().to_ascii_lowercase().as_str() {
        "python" | "py" => Ok(ReplRuntime {
            program: detect_first_command(&["python3", "python"])
                .ok_or_else(|| String::from("python runtime not found"))?,
            args: &["-c"],
        }),
        "javascript" | "js" | "node" => Ok(ReplRuntime {
            program: detect_first_command(&["node"])
                .ok_or_else(|| String::from("node runtime not found"))?,
            args: &["-e"],
        }),
        "sh" | "shell" | "bash" => Ok(ReplRuntime {
            program: detect_first_command(&["bash", "sh"])
                .ok_or_else(|| String::from("shell runtime not found"))?,
            args: &["-lc"],
        }),
        other => Err(format!("unsupported REPL language: {other}")),
    }
}

fn detect_first_command(commands: &[&'static str]) -> Option<&'static str> {
    commands
        .iter()
        .copied()
        .find(|command| command_exists(command))
}

#[derive(Clone, Copy)]
enum ConfigScope {
    Global,
    Settings,
}

#[derive(Clone, Copy)]
struct ConfigSettingSpec {
    scope: ConfigScope,
    kind: ConfigKind,
    path: &'static [&'static str],
    options: Option<&'static [&'static str]>,
}

#[derive(Clone, Copy)]
enum ConfigKind {
    Boolean,
    String,
}

fn supported_config_setting(setting: &str) -> Option<ConfigSettingSpec> {
    Some(match setting {
        "theme" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["theme"],
            options: None,
        },
        "editorMode" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["editorMode"],
            options: Some(&["default", "vim", "emacs"]),
        },
        "verbose" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["verbose"],
            options: None,
        },
        "preferredNotifChannel" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["preferredNotifChannel"],
            options: None,
        },
        "autoCompactEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["autoCompactEnabled"],
            options: None,
        },
        "autoMemoryEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["autoMemoryEnabled"],
            options: None,
        },
        "autoDreamEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["autoDreamEnabled"],
            options: None,
        },
        "fileCheckpointingEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["fileCheckpointingEnabled"],
            options: None,
        },
        "showTurnDuration" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["showTurnDuration"],
            options: None,
        },
        "terminalProgressBarEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["terminalProgressBarEnabled"],
            options: None,
        },
        "todoFeatureEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["todoFeatureEnabled"],
            options: None,
        },
        "model" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["model"],
            options: None,
        },
        "alwaysThinkingEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["alwaysThinkingEnabled"],
            options: None,
        },
        "permissions.defaultMode" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["permissions", "defaultMode"],
            options: Some(&["default", "plan", "acceptEdits", "dontAsk", "auto"]),
        },
        "language" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["language"],
            options: None,
        },
        "teammateMode" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["teammateMode"],
            options: Some(&["tmux", "in-process", "auto"]),
        },
        _ => return None,
    })
}

fn normalize_config_value(spec: ConfigSettingSpec, value: ConfigValue) -> Result<Value, String> {
    let normalized = match (spec.kind, value) {
        (ConfigKind::Boolean, ConfigValue::Bool(value)) => Value::Bool(value),
        (ConfigKind::Boolean, ConfigValue::String(value)) => {
            match value.trim().to_ascii_lowercase().as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err(String::from("setting requires true or false")),
            }
        }
        (ConfigKind::Boolean, ConfigValue::Number(_)) => {
            return Err(String::from("setting requires true or false"))
        }
        (ConfigKind::String, ConfigValue::String(value)) => Value::String(value),
        (ConfigKind::String, ConfigValue::Bool(value)) => Value::String(value.to_string()),
        (ConfigKind::String, ConfigValue::Number(value)) => json!(value),
    };

    if let Some(options) = spec.options {
        let Some(as_str) = normalized.as_str() else {
            return Err(String::from("setting requires a string value"));
        };
        if !options.iter().any(|option| option == &as_str) {
            return Err(format!(
                "Invalid value \"{as_str}\". Options: {}",
                options.join(", ")
            ));
        }
    }

    Ok(normalized)
}

fn config_file_for_scope(scope: ConfigScope, working_directory: &Path) -> Result<PathBuf, String> {
    Ok(match scope {
        ConfigScope::Global => config_home_dir(working_directory)?.join("settings.json"),
        ConfigScope::Settings => working_directory.join(".claw").join("settings.local.json"),
    })
}

fn config_home_dir(working_directory: &Path) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("CLAW_CONFIG_HOME") {
        return Ok(resolve_path_in_directory(
            working_directory,
            Path::new(&path),
        ));
    }
    let home = std::env::var("HOME").map_err(|_| String::from("HOME is not set"))?;
    Ok(resolve_path_in_directory(working_directory, Path::new(&home)).join(".claw"))
}

fn read_json_object(path: &Path) -> Result<serde_json::Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(serde_json::Map::new());
            }
            serde_json::from_str::<Value>(&contents)
                .map_err(|error| error.to_string())?
                .as_object()
                .cloned()
                .ok_or_else(|| String::from("config file must contain a JSON object"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_json_object(path: &Path, value: &serde_json::Map<String, Value>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn get_nested_value<'a>(
    value: &'a serde_json::Map<String, Value>,
    path: &[&str],
) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut current = value.get(*first)?;
    for key in rest {
        current = current.as_object()?.get(*key)?;
    }
    Some(current)
}

fn set_nested_value(root: &mut serde_json::Map<String, Value>, path: &[&str], new_value: Value) {
    let (first, rest) = path.split_first().expect("config path must not be empty");
    if rest.is_empty() {
        root.insert((*first).to_string(), new_value);
        return;
    }

    let entry = root
        .entry((*first).to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let map = entry.as_object_mut().expect("object inserted");
    set_nested_value(map, rest, new_value);
}

fn remove_nested_value(root: &mut serde_json::Map<String, Value>, path: &[&str]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return root.remove(*first).is_some();
    }

    let mut should_remove_parent = false;
    let removed = root.get_mut(*first).is_some_and(|entry| {
        entry.as_object_mut().is_some_and(|map| {
            let removed = remove_nested_value(map, rest);
            should_remove_parent = removed && map.is_empty();
            removed
        })
    });

    if should_remove_parent {
        root.remove(*first);
    }

    removed
}

fn plan_mode_state_file(working_directory: &Path) -> Result<PathBuf, String> {
    Ok(
        config_file_for_scope(ConfigScope::Settings, working_directory)?
            .parent()
            .ok_or_else(|| String::from("settings.local.json has no parent directory"))?
            .join("tool-state")
            .join("plan-mode.json"),
    )
}

fn read_plan_mode_state(path: &Path) -> Result<Option<PlanModeState>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(None);
            }
            serde_json::from_str(&contents)
                .map(Some)
                .map_err(|error| error.to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_plan_mode_state(path: &Path, state: &PlanModeState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn clear_plan_mode_state(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn iso8601_timestamp() -> String {
    if let Ok(output) = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
    {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }
    iso8601_now()
}

#[allow(clippy::needless_pass_by_value)]
fn execute_powershell(
    input: PowerShellInput,
    working_directory: &Path,
) -> std::io::Result<runtime::BashCommandOutput> {
    let _ = &input.description;
    let shell = detect_powershell_shell()?;
    execute_shell_command(
        shell,
        &input.command,
        input.timeout,
        input.run_in_background,
        working_directory,
    )
}

fn detect_powershell_shell() -> std::io::Result<&'static str> {
    #[cfg(windows)]
    {
        if windows_command_exists("pwsh.exe") {
            return Ok("pwsh.exe");
        }
        if windows_command_exists("powershell.exe") {
            return Ok("powershell.exe");
        }
    }
    if command_exists("pwsh") {
        Ok("pwsh")
    } else if command_exists("powershell") {
        Ok("powershell")
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "PowerShell executable not found (expected `pwsh` or `powershell` in PATH)",
        ))
    }
}

#[cfg(windows)]
fn windows_command_exists(command: &str) -> bool {
    Command::new("where.exe")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn command_exists(command: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[allow(clippy::too_many_lines)]
fn execute_shell_command(
    shell: &str,
    command: &str,
    timeout: Option<u64>,
    run_in_background: Option<bool>,
    working_directory: &Path,
) -> std::io::Result<runtime::BashCommandOutput> {
    let mut process = build_powershell_command(shell, command, working_directory);
    if run_in_background.unwrap_or(false) {
        let child = process
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        return Ok(runtime::BashCommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            raw_output_path: None,
            interrupted: false,
            is_image: None,
            background_task_id: Some(child.id().to_string()),
            backgrounded_by_user: Some(true),
            assistant_auto_backgrounded: Some(false),
            dangerously_disable_sandbox: None,
            return_code_interpretation: None,
            no_output_expected: Some(true),
            structured_content: None,
            persisted_output_path: None,
            persisted_output_size: None,
            sandbox_status: None,
        });
    }

    process
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(timeout_ms) = timeout {
        let mut child = process.spawn()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                let output = child.wait_with_output()?;
                return Ok(runtime::BashCommandOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    raw_output_path: None,
                    interrupted: false,
                    is_image: None,
                    background_task_id: None,
                    backgrounded_by_user: None,
                    assistant_auto_backgrounded: None,
                    dangerously_disable_sandbox: None,
                    return_code_interpretation: status
                        .code()
                        .filter(|code| *code != 0)
                        .map(|code| format!("exit_code:{code}")),
                    no_output_expected: Some(output.stdout.is_empty() && output.stderr.is_empty()),
                    structured_content: None,
                    persisted_output_path: None,
                    persisted_output_size: None,
                    sandbox_status: None,
                });
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                let _ = child.kill();
                let output = child.wait_with_output()?;
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let stderr = if stderr.trim().is_empty() {
                    format!("Command exceeded timeout of {timeout_ms} ms")
                } else {
                    format!(
                        "{}
Command exceeded timeout of {timeout_ms} ms",
                        stderr.trim_end()
                    )
                };
                return Ok(runtime::BashCommandOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr,
                    raw_output_path: None,
                    interrupted: true,
                    is_image: None,
                    background_task_id: None,
                    backgrounded_by_user: None,
                    assistant_auto_backgrounded: None,
                    dangerously_disable_sandbox: None,
                    return_code_interpretation: Some(String::from("timeout")),
                    no_output_expected: Some(false),
                    structured_content: None,
                    persisted_output_path: None,
                    persisted_output_size: None,
                    sandbox_status: None,
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let output = process.output()?;
    Ok(runtime::BashCommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        raw_output_path: None,
        interrupted: false,
        is_image: None,
        background_task_id: None,
        backgrounded_by_user: None,
        assistant_auto_backgrounded: None,
        dangerously_disable_sandbox: None,
        return_code_interpretation: output
            .status
            .code()
            .filter(|code| *code != 0)
            .map(|code| format!("exit_code:{code}")),
        no_output_expected: Some(output.stdout.is_empty() && output.stderr.is_empty()),
        structured_content: None,
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: None,
    })
}

fn build_powershell_command(shell: &str, command: &str, working_directory: &Path) -> Command {
    let mut process = Command::new(shell);
    process
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(command)
        .current_dir(working_directory);
    process
}

fn resolve_cell_index(
    cells: &[serde_json::Value],
    cell_id: Option<&str>,
    edit_mode: NotebookEditMode,
) -> Result<usize, String> {
    if cells.is_empty()
        && matches!(
            edit_mode,
            NotebookEditMode::Replace | NotebookEditMode::Delete
        )
    {
        return Err(String::from("Notebook has no cells to edit"));
    }
    if let Some(cell_id) = cell_id {
        cells
            .iter()
            .position(|cell| cell.get("id").and_then(serde_json::Value::as_str) == Some(cell_id))
            .ok_or_else(|| format!("Cell id not found: {cell_id}"))
    } else {
        Ok(cells.len().saturating_sub(1))
    }
}

fn source_lines(source: &str) -> Vec<serde_json::Value> {
    if source.is_empty() {
        return vec![serde_json::Value::String(String::new())];
    }
    source
        .split_inclusive('\n')
        .map(|line| serde_json::Value::String(line.to_string()))
        .collect()
}

fn format_notebook_edit_mode(mode: NotebookEditMode) -> String {
    match mode {
        NotebookEditMode::Replace => String::from("replace"),
        NotebookEditMode::Insert => String::from("insert"),
        NotebookEditMode::Delete => String::from("delete"),
    }
}

fn make_cell_id(index: usize) -> String {
    format!("cell-{}", index + 1)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;

    use super::{
        agent_permission_policy, allowed_tools_for_subagent, build_agent_profile_in_directory,
        execute_tool, final_assistant_text, mvp_tool_specs, permission_mode_from_plugin,
        persist_agent_artifact, persist_agent_terminal_state, prepare_agent_in_directory,
        push_output_block, AgentInput, ProviderRuntimeClient, SubagentToolExecutor,
    };
    use agent_runtime::{
        AgentRunSpec, AgentRunStatus, AgentRuntime, AgentRuntimeError, AgentWorkerPool,
        ArtifactEnvelope, ArtifactSink, BudgetReservation, ContextSnapshot, ReasoningPolicy,
        ResolvedModelPolicy,
    };
    use api::OutputContentBlock;
    use brain_graph::{
        id::gen_node_id,
        schema::{Edge, EdgeKind, GraphType, Node, NodeKind},
        store::GraphStore,
    };
    use runtime::{
        ApiClient, ApiRequest, AssistantEvent, ConversationMessage, ConversationRuntime,
        RuntimeError, Session,
    };
    use serde_json::json;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("clawd-tools-{unique}-{name}"))
    }

    fn test_working_directory() -> PathBuf {
        std::env::current_dir().expect("cwd")
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }

        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    struct DirectoryCleanup(PathBuf);

    impl DirectoryCleanup {
        fn new(path: PathBuf) -> Self {
            Self(path)
        }
    }

    impl Drop for DirectoryCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn explicit_working_directory_scopes_workspace_tools() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _todo_store = EnvVarGuard::remove("CLAWD_TODO_STORE");
        let root = temp_path("explicit-working-directory");
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        fs::create_dir_all(&workspace_a).expect("create workspace A");
        fs::create_dir_all(&workspace_b).expect("create workspace B");
        fs::write(workspace_a.join("same.txt"), "from-a\nneedle-a\n").expect("seed workspace A");
        fs::write(workspace_b.join("same.txt"), "from-b\nneedle-b\n").expect("seed workspace B");

        let read_a = super::execute_tool_in_directory(
            "read_file",
            &json!({"path": "same.txt"}),
            &workspace_a,
        )
        .expect("read workspace A");
        let read_b = super::execute_tool_in_directory(
            "read_file",
            &json!({"path": "same.txt"}),
            &workspace_b,
        )
        .expect("read workspace B");
        assert!(read_a.contains("from-a"));
        assert!(read_b.contains("from-b"));

        for (workspace, marker) in [(&workspace_a, "written-a"), (&workspace_b, "written-b")] {
            super::execute_tool_in_directory(
                "write_file",
                &json!({"path": "written.txt", "content": marker}),
                workspace,
            )
            .expect("write scoped file");
            fs::write(workspace.join("editable.txt"), "before").expect("seed editable file");
            super::execute_tool_in_directory(
                "edit_file",
                &json!({
                    "path": "editable.txt",
                    "old_string": "before",
                    "new_string": marker
                }),
                workspace,
            )
            .expect("edit scoped file");
            assert_eq!(
                fs::read_to_string(workspace.join("written.txt")).expect("read written file"),
                marker
            );
            assert_eq!(
                fs::read_to_string(workspace.join("editable.txt")).expect("read edited file"),
                marker
            );
        }

        let glob_a = super::execute_tool_in_directory(
            "glob_search",
            &json!({"pattern": "*.txt"}),
            &workspace_a,
        )
        .expect("glob workspace A");
        let glob_b = super::execute_tool_in_directory(
            "glob_search",
            &json!({"pattern": "*.txt"}),
            &workspace_b,
        )
        .expect("glob workspace B");
        assert!(glob_a.contains("workspace-a"));
        assert!(!glob_a.contains("workspace-b"));
        assert!(glob_b.contains("workspace-b"));
        assert!(!glob_b.contains("workspace-a"));

        let grep_a = super::execute_tool_in_directory(
            "grep_search",
            &json!({"pattern": "needle-a", "path": ".", "output_mode": "content"}),
            &workspace_a,
        )
        .expect("grep workspace A");
        let grep_b = super::execute_tool_in_directory(
            "grep_search",
            &json!({"pattern": "needle-b", "path": ".", "output_mode": "content"}),
            &workspace_b,
        )
        .expect("grep workspace B");
        assert!(grep_a.contains("needle-a"));
        assert!(!grep_a.contains("needle-b"));
        assert!(grep_b.contains("needle-b"));
        assert!(!grep_b.contains("needle-a"));

        for (workspace, marker) in [(&workspace_a, "todo-a"), (&workspace_b, "todo-b")] {
            super::execute_tool_in_directory(
                "TodoWrite",
                &json!({
                    "todos": [{
                        "content": marker,
                        "status": "pending",
                        "activeForm": marker
                    }]
                }),
                workspace,
            )
            .expect("write scoped todo state");
            let stored = fs::read_to_string(workspace.join(".clawd-todos.json"))
                .expect("read scoped todo state");
            assert!(stored.contains(marker));
        }

        for (workspace, language) in [(&workspace_a, "lang-a"), (&workspace_b, "lang-b")] {
            let config_dir = workspace.join(".claw");
            fs::create_dir_all(&config_dir).expect("create local config directory");
            fs::write(
                config_dir.join("settings.local.json"),
                serde_json::to_vec(&json!({
                    "language": language,
                    "permissions": {"defaultMode": "acceptEdits"}
                }))
                .expect("serialize local config"),
            )
            .expect("write local config");
            let config = super::execute_tool_in_directory(
                "Config",
                &json!({"setting": "language"}),
                workspace,
            )
            .expect("read scoped config");
            let config: serde_json::Value = serde_json::from_str(&config).expect("config json");
            assert_eq!(config["value"], language);
        }

        super::execute_tool_in_directory("EnterPlanMode", &json!({}), &workspace_a)
            .expect("enter plan mode in workspace A");
        assert!(workspace_a.join(".claw/tool-state/plan-mode.json").exists());
        assert!(!workspace_b.join(".claw/tool-state/plan-mode.json").exists());
        super::execute_tool_in_directory("EnterPlanMode", &json!({}), &workspace_b)
            .expect("enter plan mode in workspace B");
        super::execute_tool_in_directory("ExitPlanMode", &json!({}), &workspace_a)
            .expect("exit plan mode in workspace A");
        assert!(!workspace_a.join(".claw/tool-state/plan-mode.json").exists());
        assert!(workspace_b.join(".claw/tool-state/plan-mode.json").exists());
        super::execute_tool_in_directory("ExitPlanMode", &json!({}), &workspace_b)
            .expect("exit plan mode in workspace B");

        for (workspace, marker) in [(&workspace_a, "notebook-a"), (&workspace_b, "notebook-b")] {
            fs::write(
                workspace.join("sample.ipynb"),
                serde_json::to_vec(&json!({
                    "cells": [{
                        "cell_type": "code",
                        "execution_count": null,
                        "id": "cell-1",
                        "metadata": {},
                        "outputs": [],
                        "source": ["before"]
                    }],
                    "metadata": {"kernelspec": {"language": "python"}},
                    "nbformat": 4,
                    "nbformat_minor": 5
                }))
                .expect("serialize notebook"),
            )
            .expect("write notebook");
            super::execute_tool_in_directory(
                "NotebookEdit",
                &json!({
                    "notebook_path": "sample.ipynb",
                    "cell_id": "cell-1",
                    "new_source": marker,
                    "edit_mode": "replace"
                }),
                workspace,
            )
            .expect("edit scoped notebook");
            assert!(fs::read_to_string(workspace.join("sample.ipynb"))
                .expect("read edited notebook")
                .contains(marker));
        }

        for (workspace, marker) in [(&workspace_a, "workspace-a"), (&workspace_b, "workspace-b")] {
            let repl = super::execute_tool_in_directory(
                "REPL",
                &json!({
                    "language": "python",
                    "code": "import os; print(os.getcwd())",
                    "timeout_ms": 1000
                }),
                workspace,
            );
            match repl {
                Ok(repl) => {
                    let repl: serde_json::Value = serde_json::from_str(&repl).expect("REPL json");
                    assert!(repl["stdout"]
                        .as_str()
                        .expect("REPL stdout")
                        .contains(marker));
                }
                #[cfg(windows)]
                Err(error) if error.contains("python runtime not found") => {
                    let output = super::build_repl_command("cmd.exe", &["/C"], "cd", workspace)
                        .output()
                        .expect("run Windows REPL fallback subprocess");
                    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
                }
                Err(error) => panic!("run scoped REPL: {error}"),
            }

            let bash =
                super::execute_tool_in_directory("bash", &json!({"command": "pwd"}), workspace);
            match bash {
                Ok(bash) => {
                    let bash: serde_json::Value = serde_json::from_str(&bash).expect("bash json");
                    assert!(bash["stdout"]
                        .as_str()
                        .expect("bash stdout")
                        .contains(marker));
                }
                #[cfg(windows)]
                Err(error) if error.contains("program not found") => {}
                Err(error) => panic!("run scoped bash: {error}"),
            }
        }

        fs::write(workspace_a.join("a.rs"), "pub fn from_a() {}\n").expect("write Rust A");
        let relative_root_b = workspace_b.join("relative-root");
        fs::create_dir_all(&relative_root_b).expect("create relative graph root");
        fs::write(relative_root_b.join("b.rs"), "pub fn from_b() {}\n").expect("write Rust B");
        let graph_a = super::execute_tool_in_directory(
            "graph_index_code_workspace",
            &json!({
                "db_path": workspace_a.join("graph.db"),
                "max_files": 10
            }),
            &workspace_a,
        )
        .expect("index default scoped graph root");
        let graph_b = super::execute_tool_in_directory(
            "graph_index_code_workspace",
            &json!({
                "db_path": workspace_b.join("graph.db"),
                "root": "relative-root",
                "max_files": 10
            }),
            &workspace_b,
        )
        .expect("index relative scoped graph root");
        let graph_a: serde_json::Value = serde_json::from_str(&graph_a).expect("graph A json");
        let graph_b: serde_json::Value = serde_json::from_str(&graph_b).expect("graph B json");
        assert_eq!(graph_a["files_indexed"], 1);
        assert_eq!(graph_b["files_indexed"], 1);
        assert!(graph_a["root"]
            .as_str()
            .expect("graph A root")
            .contains("workspace-a"));
        assert!(graph_b["root"]
            .as_str()
            .expect("graph B root")
            .contains("relative-root"));

        #[cfg(windows)]
        for (workspace, marker) in [(&workspace_a, "workspace-a"), (&workspace_b, "workspace-b")] {
            let powershell = super::execute_tool_in_directory(
                "PowerShell",
                &json!({"command": "(Get-Location).Path", "timeout": 1000}),
                workspace,
            )
            .expect("run scoped PowerShell");
            let powershell: serde_json::Value =
                serde_json::from_str(&powershell).expect("PowerShell json");
            assert!(powershell["stdout"]
                .as_str()
                .expect("PowerShell stdout")
                .contains(marker));
        }

        #[cfg(not(windows))]
        {
            let command = super::build_powershell_command("pwsh", "Get-Location", &workspace_a);
            assert_eq!(command.get_current_dir(), Some(workspace_a.as_path()));
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_working_directory_scopes_agent_preparation_and_executor() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _agent_store = EnvVarGuard::remove("CLAWD_AGENT_STORE");
        let root = temp_path("explicit-agent-working-directory");
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        for (workspace, model, max_iterations, marker) in [
            (&workspace_a, "agent-model-a", 7, "from-agent-a"),
            (&workspace_b, "agent-model-b", 9, "from-agent-b"),
        ] {
            fs::create_dir_all(workspace.join(".ai-brain")).expect("create agent config dir");
            fs::write(
                workspace.join(".ai-brain/subagent.json"),
                serde_json::to_vec(&json!({
                    "model": model,
                    "max_iterations": max_iterations
                }))
                .expect("serialize agent config"),
            )
            .expect("write agent config");
            fs::write(workspace.join("same.txt"), marker).expect("write agent fixture");

            let prepared = super::prepare_agent_in_directory(
                AgentInput {
                    description: format!("agent in {marker}"),
                    prompt: format!("inspect {marker}"),
                    subagent_type: Some(String::from("Explore")),
                    name: None,
                    model: None,
                    run_in_background: false,
                },
                workspace,
            )
            .expect("prepare scoped agent");
            assert_eq!(prepared.manifest.model.as_deref(), Some(model));
            assert_eq!(prepared.profile.max_iterations, max_iterations);
            assert!(prepared
                .profile
                .system_prompt
                .join("\n")
                .contains(&workspace.display().to_string()));
            assert!(PathBuf::from(&prepared.manifest.output_file)
                .starts_with(workspace.join(".clawd-agents")));
            assert!(PathBuf::from(&prepared.manifest.manifest_file)
                .starts_with(workspace.join(".clawd-agents")));

            let mut executor = SubagentToolExecutor::new(
                BTreeSet::from([String::from("read_file")]),
                workspace.to_path_buf(),
            );
            let read = runtime::ToolExecutor::execute(
                &mut executor,
                "read_file",
                r#"{"path":"same.txt"}"#,
            )
            .expect("sub-agent executor reads scoped file");
            assert!(read.contains(marker));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_working_directory_isolates_concurrent_requests() {
        let root = temp_path("concurrent-working-directories");
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        fs::create_dir_all(&workspace_a).expect("create workspace A");
        fs::create_dir_all(&workspace_b).expect("create workspace B");
        fs::write(workspace_a.join("same.txt"), "from-a").expect("seed workspace A");
        fs::write(workspace_b.join("same.txt"), "from-b").expect("seed workspace B");

        thread::scope(|scope| {
            for (workspace, marker) in [(&workspace_a, "from-a"), (&workspace_b, "from-b")] {
                scope.spawn(move || {
                    for iteration in 0..20 {
                        let read = super::execute_tool_in_directory(
                            "read_file",
                            &json!({"path": "same.txt"}),
                            workspace,
                        )
                        .expect("concurrent scoped read");
                        assert!(read.contains(marker));
                        super::execute_tool_in_directory(
                            "write_file",
                            &json!({
                                "path": "concurrent.txt",
                                "content": format!("{marker}-{iteration}")
                            }),
                            workspace,
                        )
                        .expect("concurrent scoped write");
                    }
                });
            }
        });

        assert_eq!(
            fs::read_to_string(workspace_a.join("concurrent.txt"))
                .expect("read concurrent output A"),
            "from-a-19"
        );
        assert_eq!(
            fs::read_to_string(workspace_b.join("concurrent.txt"))
                .expect("read concurrent output B"),
            "from-b-19"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_working_directory_resolves_relative_environment_overrides() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _todo_store = EnvVarGuard::set("CLAWD_TODO_STORE", "state/todos.json");
        let _config_home = EnvVarGuard::set("CLAW_CONFIG_HOME", "config-home");
        let _graph_db = EnvVarGuard::set("AI_BRAIN_GRAPH_DB", "graph/graph.db");
        let _agent_store = EnvVarGuard::set("CLAWD_AGENT_STORE", "agent-output");
        let workspace = temp_path("relative-environment-overrides");
        fs::create_dir_all(&workspace).expect("create override workspace");
        fs::write(workspace.join("lib.rs"), "pub fn scoped() {}\n").expect("write graph fixture");

        super::execute_tool_in_directory(
            "TodoWrite",
            &json!({
                "todos": [{
                    "content": "relative todo",
                    "status": "pending",
                    "activeForm": "relative todo"
                }]
            }),
            &workspace,
        )
        .expect("write relative todo override");
        assert!(workspace.join("state/todos.json").exists());

        super::execute_tool_in_directory(
            "Config",
            &json!({"setting": "verbose", "value": true}),
            &workspace,
        )
        .expect("write relative config override");
        assert!(workspace.join("config-home/settings.json").exists());

        super::execute_tool_in_directory(
            "graph_index_code_workspace",
            &json!({"max_files": 10}),
            &workspace,
        )
        .expect("write relative graph override");
        assert!(workspace.join("graph/graph.db").exists());

        let prepared = super::prepare_agent_in_directory(
            AgentInput {
                description: String::from("relative agent store"),
                prompt: String::from("verify relative agent store"),
                subagent_type: Some(String::from("Explore")),
                name: None,
                model: Some(String::from("explicit-test-model")),
                run_in_background: false,
            },
            &workspace,
        )
        .expect("prepare relative agent override");
        assert!(PathBuf::from(prepared.manifest.output_file)
            .starts_with(workspace.join("agent-output")));

        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn explicit_working_directory_scopes_relative_graph_input_paths() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ignored_env_dir = PathBuf::from(
            temp_path("ignored-graph-env")
                .file_name()
                .expect("relative env directory"),
        );
        let _graph_db = EnvVarGuard::set("AI_BRAIN_GRAPH_DB", ignored_env_dir.join("ignored.db"));
        let relative_db_dir = PathBuf::from(
            temp_path("relative-graph-input")
                .file_name()
                .expect("relative DB directory"),
        );
        let relative_db_path = relative_db_dir.join("graph.db");
        let process_cwd = test_working_directory();
        let _process_db_cleanup = DirectoryCleanup::new(process_cwd.join(&relative_db_dir));
        let _process_env_cleanup = DirectoryCleanup::new(process_cwd.join(&ignored_env_dir));
        let root = temp_path("relative-graph-input-workspaces");
        let _root_cleanup = DirectoryCleanup::new(root.clone());
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        fs::create_dir_all(&workspace_a).expect("create graph workspace A");
        fs::create_dir_all(&workspace_b).expect("create graph workspace B");

        for workspace in [&workspace_a, &workspace_b] {
            super::execute_tool_in_directory(
                "graph_list_domains",
                &json!({"db_path": relative_db_path}),
                workspace,
            )
            .expect("list domains in scoped relative graph DB");
            assert!(workspace.join(&relative_db_path).exists());
            assert!(!workspace.join(&ignored_env_dir).join("ignored.db").exists());
        }
        assert!(!process_cwd.join(relative_db_path).exists());
    }

    #[test]
    fn explicit_working_directory_scopes_relative_graph_environment_paths() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let relative_db_dir = PathBuf::from(
            temp_path("relative-graph-env")
                .file_name()
                .expect("relative env DB directory"),
        );
        let relative_db_path = relative_db_dir.join("graph.db");
        let _graph_db = EnvVarGuard::set("AI_BRAIN_GRAPH_DB", &relative_db_path);
        let process_cwd = test_working_directory();
        let _process_db_cleanup = DirectoryCleanup::new(process_cwd.join(&relative_db_dir));
        let root = temp_path("relative-graph-env-workspaces");
        let _root_cleanup = DirectoryCleanup::new(root.clone());
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        fs::create_dir_all(&workspace_a).expect("create graph workspace A");
        fs::create_dir_all(&workspace_b).expect("create graph workspace B");

        for workspace in [&workspace_a, &workspace_b] {
            super::execute_tool_in_directory("graph_list_domains", &json!({}), workspace)
                .expect("list domains in scoped env graph DB");
            assert!(workspace.join(&relative_db_path).exists());
        }
        assert!(!process_cwd.join(relative_db_path).exists());
    }

    #[test]
    fn legacy_execute_tool_keeps_current_directory_behavior() {
        let cwd = std::env::current_dir().expect("current directory");
        let relative_dir = PathBuf::from(format!(
            ".clawd-tools-legacy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let fixture_dir = cwd.join(&relative_dir);
        fs::create_dir_all(&fixture_dir).expect("create legacy fixture dir");
        fs::write(fixture_dir.join("same.txt"), "legacy-current-directory")
            .expect("write legacy fixture");

        let output = execute_tool("read_file", &json!({"path": relative_dir.join("same.txt")}))
            .expect("legacy execute_tool reads from process cwd");
        assert!(output.contains("legacy-current-directory"));

        let _ = fs::remove_dir_all(fixture_dir);
    }

    fn graph_node(title: &str, keywords: &[&str], importance: f64) -> Node {
        let now = chrono::Utc::now().timestamp_millis();
        Node {
            id: gen_node_id(GraphType::Memory, NodeKind::Memory),
            kind: NodeKind::Memory,
            graph_type: GraphType::Memory,
            props: BTreeMap::from([
                ("catalog_title".to_string(), json!(title)),
                ("catalog_keywords".to_string(), json!(keywords)),
                ("catalog_type".to_string(), json!("business_logic")),
            ])
            .into_iter()
            .collect(),
            importance,
            created_at: now,
            last_accessed: now,
            superseded: false,
        }
    }

    fn seed_graph_db(path: &PathBuf) -> (Node, Node) {
        let store = GraphStore::open(path).expect("open graph store");
        let root = graph_node("红冲资费推送 BMS 异步调用", &["红冲", "BMS"], 0.9);
        let upstream = graph_node("红冲资费生成逻辑解释", &["红冲", "资费"], 0.8);
        store.upsert_node(&root).expect("insert root");
        store.upsert_node(&upstream).expect("insert upstream");
        store
            .insert_edge(&Edge {
                src: upstream.id.clone(),
                dst: root.id.clone(),
                kind: EdgeKind::DependsOn,
                props: Default::default(),
                created_at: chrono::Utc::now().timestamp_millis(),
                weight: 0.8,
            })
            .expect("insert edge");
        (root, upstream)
    }

    #[test]
    fn exposes_mvp_tools() {
        let names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"WebFetch"));
        assert!(names.contains(&"WebSearch"));
        assert!(names.contains(&"TodoWrite"));
        assert!(names.contains(&"Skill"));
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"ToolSearch"));
        assert!(names.contains(&"NotebookEdit"));
        assert!(names.contains(&"Sleep"));
        assert!(names.contains(&"SendUserMessage"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"EnterPlanMode"));
        assert!(names.contains(&"ExitPlanMode"));
        assert!(names.contains(&"StructuredOutput"));
        assert!(names.contains(&"REPL"));
        assert!(names.contains(&"PowerShell"));
        assert!(names.contains(&"novel_task"));
        assert!(names.contains(&"novel_project"));
        assert!(!names.contains(&"novel_commit_delta"));
        assert!(names.contains(&"graph_search_catalog"));
        assert!(names.contains(&"graph_get_node_detail"));
        assert!(names.contains(&"graph_trace_memory"));
        assert!(names.contains(&"graph_list_domains"));
        assert!(names.contains(&"graph_add_memory"));
        assert!(names.contains(&"graph_add_concept"));
        assert!(names.contains(&"graph_add_code_node"));
        assert!(names.contains(&"graph_index_code_workspace"));
        assert!(names.contains(&"graph_link_nodes"));
    }

    #[test]
    fn rejects_unknown_tool_names() {
        let error = execute_tool("nope", &json!({})).expect_err("tool should be rejected");
        assert!(error.contains("unsupported tool"));
    }

    #[test]
    fn graph_tools_execute_against_sqlite_store() {
        let db_path = temp_path("graph-tools.db");
        let (root, _upstream) = seed_graph_db(&db_path);
        let root_id = root.id.clone();
        let db_path_string = db_path.display().to_string();

        let catalog = execute_tool(
            "graph_search_catalog",
            &json!({
                "db_path": db_path_string,
                "query": "红冲 BMS",
                "graph_type": "Memory",
                "limit": 5
            }),
        )
        .expect("graph_search_catalog should succeed");
        let catalog_json: serde_json::Value =
            serde_json::from_str(&catalog).expect("catalog output json");
        assert_eq!(catalog_json["status"], "ok");
        assert_eq!(
            catalog_json["data"]["entries"][0]["title"],
            "红冲资费推送 BMS 异步调用"
        );

        let detail = execute_tool(
            "graph_get_node_detail",
            &json!({
                "db_path": db_path_string,
                "node_id": root_id
            }),
        )
        .expect("graph_get_node_detail should succeed");
        let detail_json: serde_json::Value =
            serde_json::from_str(&detail).expect("detail output json");
        assert_eq!(detail_json["status"], "ok");
        assert_eq!(detail_json["data"]["upstream"].as_array().unwrap().len(), 1);

        let trace = execute_tool(
            "graph_trace_memory",
            &json!({
                "db_path": db_path_string,
                "root_id": root_id,
                "direction": "upstream",
                "max_depth": 1,
                "limit": 5
            }),
        )
        .expect("graph_trace_memory should succeed");
        let trace_json: serde_json::Value =
            serde_json::from_str(&trace).expect("trace output json");
        assert_eq!(trace_json["status"], "ok");
        assert_eq!(trace_json["data"]["steps"].as_array().unwrap().len(), 1);

        let domains = execute_tool(
            "graph_list_domains",
            &json!({
                "db_path": db_path_string
            }),
        )
        .expect("graph_list_domains should succeed");
        let domains_json: serde_json::Value =
            serde_json::from_str(&domains).expect("domains output json");
        assert_eq!(domains_json.as_array().unwrap()[0]["node_count"], 2);

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn graph_write_tools_create_and_link_nodes() {
        let db_path = temp_path("graph-write-tools.db");
        let db_path_string = db_path.display().to_string();

        let memory = execute_tool(
            "graph_add_memory",
            &json!({
                "db_path": db_path_string,
                "title": "红冲资费生成逻辑解释",
                "summary": "解释红冲资费如何生成。",
                "keywords": ["红冲", "资费"],
                "catalog_type": "business_logic",
                "importance": 0.9
            }),
        )
        .expect("graph_add_memory should succeed");
        let memory_json: serde_json::Value =
            serde_json::from_str(&memory).expect("memory output json");
        let memory_id = memory_json["node_id"].as_str().unwrap().to_string();

        let concept = execute_tool(
            "graph_add_concept",
            &json!({
                "db_path": db_path_string,
                "name": "红冲",
                "summary": "退款冲正相关业务概念",
                "aliases": ["冲正"],
                "importance": 0.8
            }),
        )
        .expect("graph_add_concept should succeed");
        let concept_json: serde_json::Value =
            serde_json::from_str(&concept).expect("concept output json");
        let concept_id = concept_json["node_id"].as_str().unwrap().to_string();

        let code = execute_tool(
            "graph_add_code_node",
            &json!({
                "db_path": db_path_string,
                "path": "src/billing/reversal.rs",
                "symbol": "build_reversal_fee",
                "summary": "生成红冲资费",
                "code_kind": "function"
            }),
        )
        .expect("graph_add_code_node should succeed");
        let code_json: serde_json::Value = serde_json::from_str(&code).expect("code output json");
        assert!(code_json["node_id"]
            .as_str()
            .unwrap()
            .starts_with("code_code_"));

        let link = execute_tool(
            "graph_link_nodes",
            &json!({
                "db_path": db_path_string,
                "src": concept_id,
                "dst": memory_id,
                "edge_kind": "MentionedIn",
                "weight": 0.75
            }),
        )
        .expect("graph_link_nodes should succeed");
        let link_json: serde_json::Value = serde_json::from_str(&link).expect("link output json");
        assert_eq!(link_json["status"], "ok");

        let detail = execute_tool(
            "graph_get_node_detail",
            &json!({
                "db_path": db_path_string,
                "node_id": memory_id
            }),
        )
        .expect("graph_get_node_detail should succeed");
        let detail_json: serde_json::Value =
            serde_json::from_str(&detail).expect("detail output json");
        assert_eq!(detail_json["status"], "ok");
        assert_eq!(detail_json["data"]["upstream"].as_array().unwrap().len(), 1);

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn graph_index_code_workspace_indexes_rust_files() {
        let root = temp_path("code-workspace");
        let src_dir = root.join("src");
        let foo_dir = src_dir.join("foo");
        fs::create_dir_all(&foo_dir).expect("create test workspace");
        fs::write(src_dir.join("lib.rs"), "pub mod foo;\n").expect("write lib");
        fs::write(foo_dir.join("mod.rs"), "pub fn run() {}\n").expect("write module");
        fs::create_dir_all(root.join("target")).expect("create target");
        fs::write(root.join("target").join("ignored.rs"), "fn ignored() {}\n")
            .expect("write ignored");

        let db_path = temp_path("graph-code-index.db");
        let db_path_string = db_path.display().to_string();
        let root_string = root.display().to_string();

        let indexed = execute_tool(
            "graph_index_code_workspace",
            &json!({
                "db_path": db_path_string,
                "root": root_string,
                "max_files": 10
            }),
        )
        .expect("graph_index_code_workspace should succeed");
        let indexed_json: serde_json::Value =
            serde_json::from_str(&indexed).expect("index output json");
        assert_eq!(indexed_json["status"], "ok");
        assert_eq!(indexed_json["files_indexed"], 2);
        assert_eq!(indexed_json["symbols_indexed"], 2);
        assert_eq!(indexed_json["truncated"], false);

        let catalog = execute_tool(
            "graph_search_catalog",
            &json!({
                "db_path": db_path_string,
                "query": "foo",
                "graph_type": "Code",
                "limit": 10
            }),
        )
        .expect("graph_search_catalog should succeed");
        let catalog_json: serde_json::Value =
            serde_json::from_str(&catalog).expect("catalog output json");
        assert_eq!(catalog_json["status"], "ok");
        assert!(catalog_json["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["node_id"] == "code_code_module_src_foo"));

        let detail = execute_tool(
            "graph_get_node_detail",
            &json!({
                "db_path": db_path_string,
                "node_id": "code_code_module_src"
            }),
        )
        .expect("graph_get_node_detail should succeed");
        let detail_json: serde_json::Value =
            serde_json::from_str(&detail).expect("detail output json");
        assert_eq!(detail_json["status"], "ok");
        assert!(detail_json["data"]["downstream"]
            .as_array()
            .unwrap()
            .iter()
            .any(|neighbor| neighbor["node_id"] == "code_code_file_src_lib_rs"));

        let run_catalog = execute_tool(
            "graph_search_catalog",
            &json!({
                "db_path": db_path_string,
                "query": "run",
                "graph_type": "Code",
                "limit": 10
            }),
        )
        .expect("graph_search_catalog should succeed");
        let run_catalog_json: serde_json::Value =
            serde_json::from_str(&run_catalog).expect("catalog output json");
        assert_eq!(run_catalog_json["status"], "ok");
        assert!(run_catalog_json["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["node_id"] == "code_code_symbol_src_foo_mod_rs_function_run"));

        let file_detail = execute_tool(
            "graph_get_node_detail",
            &json!({
                "db_path": db_path_string,
                "node_id": "code_code_file_src_foo_mod_rs"
            }),
        )
        .expect("graph_get_node_detail should succeed");
        let file_detail_json: serde_json::Value =
            serde_json::from_str(&file_detail).expect("detail output json");
        assert_eq!(file_detail_json["status"], "ok");
        assert!(file_detail_json["data"]["downstream"]
            .as_array()
            .unwrap()
            .iter()
            .any(|neighbor| neighbor["node_id"] == "code_code_symbol_src_foo_mod_rs_function_run"));

        let _ = fs::remove_file(db_path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn permission_mode_from_plugin_rejects_invalid_inputs() {
        let unknown_permission = permission_mode_from_plugin("admin")
            .expect_err("unknown plugin permission should fail");
        assert!(unknown_permission.contains("unsupported plugin permission: admin"));

        let empty_permission =
            permission_mode_from_plugin("").expect_err("empty plugin permission should fail");
        assert!(empty_permission.contains("unsupported plugin permission: "));
    }

    #[test]
    fn web_fetch_returns_prompt_aware_summary() {
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.starts_with("GET /page "));
            HttpResponse::html(
                200,
                "OK",
                "<html><head><title>Ignored</title></head><body><h1>Test Page</h1><p>Hello <b>world</b> from local server.</p></body></html>",
            )
        }));

        let result = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/page", server.addr()),
                "prompt": "Summarize this page"
            }),
        )
        .expect("WebFetch should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["code"], 200);
        let summary = output["result"].as_str().expect("result string");
        assert!(summary.contains("Fetched"));
        assert!(summary.contains("Test Page"));
        assert!(summary.contains("Hello world from local server"));

        let titled = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/page", server.addr()),
                "prompt": "What is the page title?"
            }),
        )
        .expect("WebFetch title query should succeed");
        let titled_output: serde_json::Value = serde_json::from_str(&titled).expect("valid json");
        let titled_summary = titled_output["result"].as_str().expect("result string");
        assert!(titled_summary.contains("Title: Ignored"));
    }

    #[test]
    fn web_fetch_supports_plain_text_and_rejects_invalid_url() {
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.starts_with("GET /plain "));
            HttpResponse::text(200, "OK", "plain text response")
        }));

        let result = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/plain", server.addr()),
                "prompt": "Show me the content"
            }),
        )
        .expect("WebFetch should succeed for text content");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["url"], format!("http://{}/plain", server.addr()));
        assert!(output["result"]
            .as_str()
            .expect("result")
            .contains("plain text response"));

        let error = execute_tool(
            "WebFetch",
            &json!({
                "url": "not a url",
                "prompt": "Summarize"
            }),
        )
        .expect_err("invalid URL should fail");
        assert!(error.contains("relative URL without a base") || error.contains("invalid"));
    }

    #[test]
    fn web_search_extracts_and_filters_results() {
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.contains("GET /search?q=rust+web+search "));
            HttpResponse::html(
                200,
                "OK",
                r#"
                <html><body>
                  <a class="result__a" href="https://docs.rs/reqwest">Reqwest docs</a>
                  <a class="result__a" href="https://example.com/blocked">Blocked result</a>
                </body></html>
                "#,
            )
        }));

        std::env::set_var(
            "CLAWD_WEB_SEARCH_BASE_URL",
            format!("http://{}/search", server.addr()),
        );
        let result = execute_tool(
            "WebSearch",
            &json!({
                "query": "rust web search",
                "allowed_domains": ["https://DOCS.rs/"],
                "blocked_domains": ["HTTPS://EXAMPLE.COM"]
            }),
        )
        .expect("WebSearch should succeed");
        std::env::remove_var("CLAWD_WEB_SEARCH_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["query"], "rust web search");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["title"], "Reqwest docs");
        assert_eq!(content[0]["url"], "https://docs.rs/reqwest");
    }

    #[test]
    fn web_search_handles_generic_links_and_invalid_base_url() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str| {
            assert!(request_line.contains("GET /fallback?q=generic+links "));
            HttpResponse::html(
                200,
                "OK",
                r#"
                <html><body>
                  <a href="https://example.com/one">Example One</a>
                  <a href="https://example.com/one">Duplicate Example One</a>
                  <a href="https://docs.rs/tokio">Tokio Docs</a>
                </body></html>
                "#,
            )
        }));

        std::env::set_var(
            "CLAWD_WEB_SEARCH_BASE_URL",
            format!("http://{}/fallback", server.addr()),
        );
        let result = execute_tool(
            "WebSearch",
            &json!({
                "query": "generic links"
            }),
        )
        .expect("WebSearch fallback parsing should succeed");
        std::env::remove_var("CLAWD_WEB_SEARCH_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["url"], "https://example.com/one");
        assert_eq!(content[1]["url"], "https://docs.rs/tokio");

        std::env::set_var("CLAWD_WEB_SEARCH_BASE_URL", "://bad-base-url");
        let error = execute_tool("WebSearch", &json!({ "query": "generic links" }))
            .expect_err("invalid base URL should fail");
        std::env::remove_var("CLAWD_WEB_SEARCH_BASE_URL");
        assert!(error.contains("relative URL without a base") || error.contains("empty host"));
    }

    #[test]
    fn pending_tools_preserve_multiple_streaming_tool_calls_by_index() {
        let mut events = Vec::new();
        let mut pending_tools = BTreeMap::new();

        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({}),
            },
            1,
            &mut events,
            &mut pending_tools,
            true,
        );
        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-2".to_string(),
                name: "grep_search".to_string(),
                input: json!({}),
            },
            2,
            &mut events,
            &mut pending_tools,
            true,
        );

        pending_tools
            .get_mut(&1)
            .expect("first tool pending")
            .2
            .push_str("{\"path\":\"src/main.rs\"}");
        pending_tools
            .get_mut(&2)
            .expect("second tool pending")
            .2
            .push_str("{\"pattern\":\"TODO\"}");

        assert_eq!(
            pending_tools.remove(&1),
            Some((
                "tool-1".to_string(),
                "read_file".to_string(),
                "{\"path\":\"src/main.rs\"}".to_string(),
            ))
        );
        assert_eq!(
            pending_tools.remove(&2),
            Some((
                "tool-2".to_string(),
                "grep_search".to_string(),
                "{\"pattern\":\"TODO\"}".to_string(),
            ))
        );
    }

    #[test]
    fn todo_write_persists_and_returns_previous_state() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = temp_path("todos.json");
        std::env::set_var("CLAWD_TODO_STORE", &path);

        let first = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Add tool", "activeForm": "Adding tool", "status": "in_progress"},
                    {"content": "Run tests", "activeForm": "Running tests", "status": "pending"}
                ]
            }),
        )
        .expect("TodoWrite should succeed");
        let first_output: serde_json::Value = serde_json::from_str(&first).expect("valid json");
        assert_eq!(first_output["oldTodos"].as_array().expect("array").len(), 0);

        let second = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Add tool", "activeForm": "Adding tool", "status": "completed"},
                    {"content": "Run tests", "activeForm": "Running tests", "status": "completed"},
                    {"content": "Verify", "activeForm": "Verifying", "status": "completed"}
                ]
            }),
        )
        .expect("TodoWrite should succeed");
        std::env::remove_var("CLAWD_TODO_STORE");
        let _ = std::fs::remove_file(path);

        let second_output: serde_json::Value = serde_json::from_str(&second).expect("valid json");
        assert_eq!(
            second_output["oldTodos"].as_array().expect("array").len(),
            2
        );
        assert_eq!(
            second_output["newTodos"].as_array().expect("array").len(),
            3
        );
        assert!(second_output["verificationNudgeNeeded"].is_null());
    }

    #[test]
    fn todo_write_rejects_invalid_payloads_and_sets_verification_nudge() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = temp_path("todos-errors.json");
        std::env::set_var("CLAWD_TODO_STORE", &path);

        let empty = execute_tool("TodoWrite", &json!({ "todos": [] }))
            .expect_err("empty todos should fail");
        assert!(empty.contains("todos must not be empty"));

        // Multiple in_progress items are now allowed for parallel workflows
        let _multi_active = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "One", "activeForm": "Doing one", "status": "in_progress"},
                    {"content": "Two", "activeForm": "Doing two", "status": "in_progress"}
                ]
            }),
        )
        .expect("multiple in-progress todos should succeed");

        let blank_content = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "   ", "activeForm": "Doing it", "status": "pending"}
                ]
            }),
        )
        .expect_err("blank content should fail");
        assert!(blank_content.contains("todo content must not be empty"));

        let nudge = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Write tests", "activeForm": "Writing tests", "status": "completed"},
                    {"content": "Fix errors", "activeForm": "Fixing errors", "status": "completed"},
                    {"content": "Ship branch", "activeForm": "Shipping branch", "status": "completed"}
                ]
            }),
        )
        .expect("completed todos should succeed");
        std::env::remove_var("CLAWD_TODO_STORE");
        let _ = fs::remove_file(path);

        let output: serde_json::Value = serde_json::from_str(&nudge).expect("valid json");
        assert_eq!(output["verificationNudgeNeeded"], true);
    }

    #[test]
    fn skill_dispatch_is_reserved_for_real_tool_executor() {
        let error = execute_tool(
            "Skill",
            &json!({
                "skill": "help",
                "args": "overview"
            }),
        )
        .expect_err("Skill should be dispatched by RealToolExecutor");

        assert_eq!(error, "Skill tool is handled by RealToolExecutor directly");
    }

    #[test]
    fn tool_search_supports_keyword_and_select_queries() {
        let keyword = execute_tool(
            "ToolSearch",
            &json!({"query": "web current", "max_results": 3}),
        )
        .expect("ToolSearch should succeed");
        let keyword_output: serde_json::Value = serde_json::from_str(&keyword).expect("valid json");
        let matches = keyword_output["matches"].as_array().expect("matches");
        assert!(matches.iter().any(|value| value == "WebSearch"));

        let selected = execute_tool("ToolSearch", &json!({"query": "select:WebSearch,Skill"}))
            .expect("ToolSearch should succeed");
        let selected_output: serde_json::Value =
            serde_json::from_str(&selected).expect("valid json");
        assert_eq!(selected_output["matches"][0], "WebSearch");
        assert_eq!(selected_output["matches"][1], "Skill");

        let aliased = execute_tool("ToolSearch", &json!({"query": "WebSearchTool"}))
            .expect("ToolSearch should support tool aliases");
        let aliased_output: serde_json::Value = serde_json::from_str(&aliased).expect("valid json");
        assert_eq!(aliased_output["matches"][0], "WebSearch");
        assert_eq!(aliased_output["normalized_query"], "websearch");

        let selected_with_alias = execute_tool(
            "ToolSearch",
            &json!({"query": "select:WebSearchTool,Skill"}),
        )
        .expect("ToolSearch alias select should succeed");
        let selected_with_alias_output: serde_json::Value =
            serde_json::from_str(&selected_with_alias).expect("valid json");
        assert_eq!(selected_with_alias_output["matches"][0], "WebSearch");
        assert_eq!(selected_with_alias_output["matches"][1], "Skill");
    }

    #[test]
    fn agent_persists_handoff_metadata() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = temp_path("agent-store");
        std::env::set_var("CLAWD_AGENT_STORE", &dir);
        let prepared = prepare_agent_in_directory(
            AgentInput {
                description: "Audit the branch".to_string(),
                prompt: "Check tests and outstanding work.".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("ship-audit".to_string()),
                model: None,
                run_in_background: false,
            },
            &test_working_directory(),
        )
        .expect("Agent should be prepared");
        std::env::remove_var("CLAWD_AGENT_STORE");

        let manifest = prepared.manifest;
        assert_eq!(manifest.name, "ship-audit");
        assert_eq!(manifest.subagent_type.as_deref(), Some("Explore"));
        assert_eq!(manifest.status, "running");
        assert_eq!(manifest.profile_id, "builtin.explore");
        assert!(manifest.context_snapshot_id.starts_with("context-agent-"));
        assert!(manifest.instance_run_id.starts_with("run-agent-"));
        assert!(!manifest.created_at.is_empty());
        assert!(manifest.started_at.is_some());
        assert!(manifest.completed_at.is_none());
        let contents = std::fs::read_to_string(&manifest.output_file).expect("agent file exists");
        let manifest_contents =
            std::fs::read_to_string(&manifest.manifest_file).expect("manifest file exists");
        assert!(contents.contains("Audit the branch"));
        assert!(contents.contains("Check tests and outstanding work."));
        assert!(manifest_contents.contains("\"subagentType\": \"Explore\""));
        assert!(manifest_contents.contains("\"status\": \"running\""));
        assert_eq!(prepared.prompt, "Check tests and outstanding work.");
        assert!(prepared.profile.tool_grant.contains("read_file"));
        assert!(!prepared.profile.tool_grant.contains("Agent"));

        assert_eq!(super::normalize_subagent_type(Some("explorer")), "Explore");
        assert_eq!(super::slugify_agent_name("Ship Audit!!!"), "ship-audit");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_artifact_and_failure_states_are_persisted() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = temp_path("agent-runner");
        std::env::set_var("CLAWD_AGENT_STORE", &dir);

        let completed = prepare_agent_in_directory(
            AgentInput {
                description: "Complete the task".to_string(),
                prompt: "Do the work".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("complete-task".to_string()),
                model: Some("claude-sonnet-4-6".to_string()),
                run_in_background: false,
            },
            &test_working_directory(),
        )
        .expect("completed agent should prepare")
        .manifest;
        let artifact = ArtifactEnvelope {
            artifact_id: "artifact-test".into(),
            content: "Finished successfully".into(),
            content_hash: "test-hash".into(),
            output_contract: agent_runtime::OutputContract::Text,
            context_snapshot_id: completed.context_snapshot_id.clone(),
            producer_instance_id: completed.agent_id.clone(),
            producer_instance_run_id: completed.instance_run_id.clone(),
            producer_member_id: None,
            task_run_id: "task-test".into(),
            node_id: "node-test".into(),
            profile_id: completed.profile_id.clone(),
            profile_version: completed.profile_version,
            model: ResolvedModelPolicy {
                policy_id: "subagent".into(),
                label: "subagent".into(),
                provider: "test".into(),
                model: "claude-sonnet-4-6".into(),
                max_output_tokens: 1_024,
                temperature: 0.0,
            },
            reasoning: ReasoningPolicy::medium(),
            usage: runtime::TokenUsage {
                input_tokens: 12,
                output_tokens: 4,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            iterations: 1,
            created_at_unix_ms: 1,
        };
        persist_agent_artifact(&completed, &artifact).expect("persist completed artifact");

        let completed_manifest = std::fs::read_to_string(&completed.manifest_file)
            .expect("completed manifest should exist");
        let completed_output =
            std::fs::read_to_string(&completed.output_file).expect("completed output should exist");
        assert!(completed_manifest.contains("\"status\": \"completed\""));
        assert!(completed_manifest.contains("\"artifactId\": \"artifact-test\""));
        assert!(completed_manifest.contains("\"input_tokens\": 12"));
        assert!(completed_manifest.contains("Finished successfully"));
        assert!(completed_output.contains("Finished successfully"));

        let failed = prepare_agent_in_directory(
            AgentInput {
                description: "Fail the task".to_string(),
                prompt: "Do the failing work".to_string(),
                subagent_type: Some("Verification".to_string()),
                name: Some("fail-task".to_string()),
                model: None,
                run_in_background: false,
            },
            &test_working_directory(),
        )
        .expect("failed agent should prepare")
        .manifest;
        persist_agent_terminal_state(
            &failed,
            "failed",
            None,
            Some(String::from("simulated failure")),
        )
        .expect("persist failed state");

        let failed_manifest =
            std::fs::read_to_string(&failed.manifest_file).expect("failed manifest should exist");
        let failed_output =
            std::fs::read_to_string(&failed.output_file).expect("failed output should exist");
        assert!(failed_manifest.contains("\"status\": \"failed\""));
        assert!(failed_manifest.contains("simulated failure"));
        assert!(failed_output.contains("simulated failure"));

        std::env::remove_var("CLAWD_AGENT_STORE");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_profiles_own_prompt_tool_grant_and_output_contract() {
        for role in ["Explore", "Plan", "Verification"] {
            let profile = build_agent_profile_in_directory(role, &test_working_directory())
                .expect("built-in profile");
            assert_eq!(profile.role, role);
            assert!(profile.system_prompt.join("\n").contains(role));
            assert!(profile.tool_grant.contains("read_file"));
            assert!(!profile.tool_grant.contains("Agent"));
            assert_eq!(profile.output_contract, agent_runtime::OutputContract::Text);
        }
    }

    #[test]
    fn agent_tool_subset_mapping_is_expected() {
        let general = allowed_tools_for_subagent("general-purpose");
        assert!(general.contains("bash"));
        assert!(general.contains("write_file"));
        assert!(!general.contains("Agent"));

        let explore = allowed_tools_for_subagent("Explore");
        assert!(explore.contains("read_file"));
        assert!(explore.contains("grep_search"));
        assert!(!explore.contains("bash"));

        let plan = allowed_tools_for_subagent("Plan");
        assert!(plan.contains("TodoWrite"));
        assert!(plan.contains("StructuredOutput"));
        assert!(!plan.contains("Agent"));

        let verification = allowed_tools_for_subagent("Verification");
        assert!(verification.contains("bash"));
        assert!(verification.contains("PowerShell"));
        assert!(!verification.contains("write_file"));
    }

    #[test]
    fn novel_workflow_tools_replace_ephemeral_agent_schema() {
        let specs = mvp_tool_specs();
        let agent = specs
            .iter()
            .find(|spec| spec.name == "Agent")
            .expect("Agent spec");
        let agent_properties = agent.input_schema["properties"].as_object().unwrap();
        assert!(!agent_properties.contains_key("novel_context"));
        let agent_types = agent_properties["subagent_type"]["enum"]
            .as_array()
            .unwrap();
        assert!(!agent_types.iter().any(|kind| kind == "Novel"));

        let mut novel_names = specs
            .iter()
            .map(|spec| spec.name)
            .filter(|name| name.starts_with("novel_"))
            .collect::<Vec<_>>();
        novel_names.sort_unstable();
        assert_eq!(novel_names, vec!["novel_project", "novel_task"]);
        let task = specs.iter().find(|spec| spec.name == "novel_task").unwrap();
        for guidance in [
            "仅在 needs_clarification 后调用 resume",
            "input 必须非空",
            "start 遇到 ContextRef hash 变化",
            "resume 遇到变化",
            "actual hash",
            "原 role/path",
            "只重试一次",
        ] {
            assert!(
                task.description.contains(guidance),
                "novel_task description missing {guidance}"
            );
        }
        let resume = task.input_schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| branch["properties"]["action"]["const"] == "resume")
            .unwrap();
        assert_eq!(resume["properties"]["task_id"]["minLength"], 1);
        assert_eq!(resume["properties"]["input"]["minLength"], 1);
        assert_eq!(resume["properties"]["context_refs"]["type"], "array");
        assert!(resume["properties"]["context_refs"]["description"]
            .as_str()
            .unwrap()
            .contains("只把 sha256 更新为 actual hash"));
        let publish = task.input_schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| branch["properties"]["action"]["const"] == "publish")
            .unwrap();
        let publish_properties = publish["properties"].as_object().unwrap();
        assert_eq!(publish_properties.len(), 3);
        assert!(publish_properties.contains_key("action"));
        assert!(publish_properties.contains_key("task_id"));
        assert!(publish_properties.contains_key("draft_version"));
        assert!(!publish_properties.contains_key("content"));
        assert!(!specs.iter().any(|spec| spec.name == "novel_commit_delta"));
    }

    #[test]
    fn novel_application_tools_have_object_root_schemas() {
        let specs = mvp_tool_specs();

        for name in ["novel_task", "novel_project"] {
            let spec = specs.iter().find(|spec| spec.name == name).unwrap();
            assert_eq!(
                spec.input_schema["type"], "object",
                "{name} 顶层 schema 必须声明为 object"
            );
        }
    }

    #[test]
    fn ephemeral_novel_agent_is_rejected_before_spawn() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("novel-scope");
        let agent_store = root.join("agents");
        fs::create_dir_all(&root).expect("create temp root");
        std::env::set_var("CLAWD_AGENT_STORE", &agent_store);

        let result = prepare_agent_in_directory(
            AgentInput {
                description: "续写第四章".into(),
                prompt: "承接第三章并完成第四章正文。".into(),
                subagent_type: Some("Novel".into()),
                name: Some("chapter-four".into()),
                model: None,
                run_in_background: false,
            },
            &test_working_directory(),
        );
        let Err(error) = result else {
            panic!("ephemeral Novel launch must be rejected");
        };
        assert!(error.contains("unsupported built-in agent role"));
        assert!(!agent_store.exists());

        std::env::remove_var("CLAWD_AGENT_STORE");
        let _ = fs::remove_dir_all(root);
    }

    #[derive(Debug)]
    struct MockSubagentApiClient {
        calls: usize,
        input_path: String,
    }

    impl runtime::ApiClient for MockSubagentApiClient {
        fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.calls += 1;
            match self.calls {
                1 => {
                    assert_eq!(request.messages.len(), 1);
                    Ok(vec![
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "read_file".to_string(),
                            input: json!({ "path": self.input_path }).to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ])
                }
                2 => {
                    assert!(request.messages.len() >= 3);
                    assert!(request
                        .messages
                        .iter()
                        .flat_map(|message| message.blocks.iter())
                        .any(|block| matches!(
                            block,
                            runtime::ContentBlock::ToolResult { output, .. }
                                if output.contains("hello from child")
                        )));
                    Ok(vec![
                        AssistantEvent::TextDelta("Scope: completed mock review".to_string()),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => panic!("unexpected mock stream call"),
            }
        }
    }

    #[derive(Clone, Default)]
    struct TestArtifactSink {
        artifacts: Arc<Mutex<Vec<ArtifactEnvelope>>>,
    }

    impl ArtifactSink for TestArtifactSink {
        fn store(&self, artifact: &ArtifactEnvelope) -> Result<(), AgentRuntimeError> {
            self.artifacts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(artifact.clone());
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn built_in_agent_roles_execute_through_agent_runtime() {
        let path = temp_path("subagent-input.txt");
        std::fs::write(&path, "hello from child").expect("write input file");
        let pool = AgentWorkerPool::new(1).expect("worker pool");

        for (index, role) in ["Explore", "Plan", "Verification"].into_iter().enumerate() {
            let cancellation = tokio_util::sync::CancellationToken::new();
            let lease = pool.acquire(&cancellation).await.expect("worker lease");
            let profile = build_agent_profile_in_directory(role, &test_working_directory())
                .expect("built-in profile");
            let sink = TestArtifactSink::default();
            let runtime = AgentRuntime::new(
                MockSubagentApiClient {
                    calls: 0,
                    input_path: path.display().to_string(),
                },
                SubagentToolExecutor::new(
                    profile.tool_grant.iter().map(str::to_string).collect(),
                    std::env::current_dir().expect("cwd"),
                ),
                agent_permission_policy(),
                sink.clone(),
            );
            let spec = AgentRunSpec {
                agent_instance_id: format!("agent-{index}"),
                instance_run_id: format!("run-{index}"),
                member_id: None,
                inbox_item_id: None,
                task_run_id: "task-test".into(),
                node_id: format!("node-{index}"),
                profile,
                context_snapshot: ContextSnapshot::from_text(
                    format!("context-{index}"),
                    "Inspect the delegated file",
                )
                .expect("context snapshot"),
                input_artifacts: Vec::new(),
                model: ResolvedModelPolicy {
                    policy_id: "test".into(),
                    label: "test".into(),
                    provider: "mock".into(),
                    model: "mock-model".into(),
                    max_output_tokens: 1_024,
                    temperature: 0.0,
                },
                reasoning: ReasoningPolicy::medium(),
                budget_reservation: BudgetReservation {
                    reservation_id: format!("budget-{index}"),
                    max_input_tokens: 1_024,
                    max_output_tokens: 1_024,
                },
                deadline_unix_ms: None,
            };

            let outcome = runtime.spawn(spec, lease, cancellation).wait().await;

            assert_eq!(outcome.status, AgentRunStatus::Completed, "role={role}");
            assert_eq!(
                outcome
                    .artifact
                    .as_ref()
                    .map(|artifact| artifact.content.as_str()),
                Some("Scope: completed mock review")
            );
            assert_eq!(sink.artifacts.lock().unwrap().len(), 1);
        }

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_rejects_blank_required_fields() {
        let missing_description = execute_tool(
            "Agent",
            &json!({
                "description": "  ",
                "prompt": "Inspect"
            }),
        )
        .expect_err("blank description should fail");
        assert!(missing_description.contains("description must not be empty"));

        let missing_prompt = execute_tool(
            "Agent",
            &json!({
                "description": "Inspect branch",
                "prompt": " "
            }),
        )
        .expect_err("blank prompt should fail");
        assert!(missing_prompt.contains("prompt must not be empty"));
    }

    #[test]
    fn notebook_edit_replaces_inserts_and_deletes_cells() {
        let path = temp_path("notebook.ipynb");
        std::fs::write(
            &path,
            r#"{
  "cells": [
    {"cell_type": "code", "id": "cell-a", "metadata": {}, "source": ["print(1)\n"], "outputs": [], "execution_count": null}
  ],
  "metadata": {"kernelspec": {"language": "python"}},
  "nbformat": 4,
  "nbformat_minor": 5
}"#,
        )
        .expect("write notebook");

        let replaced = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "print(2)\n",
                "edit_mode": "replace"
            }),
        )
        .expect("NotebookEdit replace should succeed");
        let replaced_output: serde_json::Value = serde_json::from_str(&replaced).expect("json");
        assert_eq!(replaced_output["cell_id"], "cell-a");
        assert_eq!(replaced_output["cell_type"], "code");

        let inserted = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "# heading\n",
                "cell_type": "markdown",
                "edit_mode": "insert"
            }),
        )
        .expect("NotebookEdit insert should succeed");
        let inserted_output: serde_json::Value = serde_json::from_str(&inserted).expect("json");
        assert_eq!(inserted_output["cell_type"], "markdown");
        let appended = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "new_source": "print(3)\n",
                "edit_mode": "insert"
            }),
        )
        .expect("NotebookEdit append should succeed");
        let appended_output: serde_json::Value = serde_json::from_str(&appended).expect("json");
        assert_eq!(appended_output["cell_type"], "code");

        let deleted = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "edit_mode": "delete"
            }),
        )
        .expect("NotebookEdit delete should succeed without new_source");
        let deleted_output: serde_json::Value = serde_json::from_str(&deleted).expect("json");
        assert!(deleted_output["cell_type"].is_null());
        assert_eq!(deleted_output["new_source"], "");

        let final_notebook: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read notebook"))
                .expect("valid notebook json");
        let cells = final_notebook["cells"].as_array().expect("cells array");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["cell_type"], "markdown");
        assert!(cells[0].get("outputs").is_none());
        assert_eq!(cells[1]["cell_type"], "code");
        assert_eq!(cells[1]["source"][0], "print(3)\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notebook_edit_rejects_invalid_inputs() {
        let text_path = temp_path("notebook.txt");
        fs::write(&text_path, "not a notebook").expect("write text file");
        let wrong_extension = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": text_path.display().to_string(),
                "new_source": "print(1)\n"
            }),
        )
        .expect_err("non-ipynb file should fail");
        assert!(wrong_extension.contains("Jupyter notebook"));
        let _ = fs::remove_file(&text_path);

        let empty_notebook = temp_path("empty.ipynb");
        fs::write(
            &empty_notebook,
            r#"{"cells":[],"metadata":{"kernelspec":{"language":"python"}},"nbformat":4,"nbformat_minor":5}"#,
        )
        .expect("write empty notebook");

        let missing_source = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": empty_notebook.display().to_string(),
                "edit_mode": "insert"
            }),
        )
        .expect_err("insert without source should fail");
        assert!(missing_source.contains("new_source is required"));

        let missing_cell = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": empty_notebook.display().to_string(),
                "edit_mode": "delete"
            }),
        )
        .expect_err("delete on empty notebook should fail");
        assert!(missing_cell.contains("Notebook has no cells to edit"));
        let _ = fs::remove_file(empty_notebook);
    }

    #[test]
    fn bash_tool_reports_success_exit_failure_timeout_and_background() {
        let success = execute_tool("bash", &json!({ "command": "printf 'hello'" }))
            .expect("bash should succeed");
        let success_output: serde_json::Value = serde_json::from_str(&success).expect("json");
        assert_eq!(success_output["stdout"], "hello");
        assert_eq!(success_output["interrupted"], false);

        let failure = execute_tool("bash", &json!({ "command": "printf 'oops' >&2; exit 7" }))
            .expect("bash failure should still return structured output");
        let failure_output: serde_json::Value = serde_json::from_str(&failure).expect("json");
        assert_eq!(failure_output["returnCodeInterpretation"], "exit_code:7");
        assert!(failure_output["stderr"]
            .as_str()
            .expect("stderr")
            .contains("oops"));

        let timeout = execute_tool("bash", &json!({ "command": "sleep 1", "timeout": 10 }))
            .expect("bash timeout should return output");
        let timeout_output: serde_json::Value = serde_json::from_str(&timeout).expect("json");
        assert_eq!(timeout_output["interrupted"], true);
        assert_eq!(timeout_output["returnCodeInterpretation"], "timeout");
        assert!(timeout_output["stderr"]
            .as_str()
            .expect("stderr")
            .contains("Command exceeded timeout"));

        let background = execute_tool(
            "bash",
            &json!({ "command": "sleep 1", "run_in_background": true }),
        )
        .expect("bash background should succeed");
        let background_output: serde_json::Value = serde_json::from_str(&background).expect("json");
        assert!(background_output["backgroundTaskId"].as_str().is_some());
        assert_eq!(background_output["noOutputExpected"], true);
    }

    #[test]
    fn file_tools_cover_read_write_and_edit_behaviors() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("fs-suite");
        fs::create_dir_all(&root).expect("create root");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        let write_create = execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\nalpha\n" }),
        )
        .expect("write create should succeed");
        let write_create_output: serde_json::Value =
            serde_json::from_str(&write_create).expect("json");
        assert_eq!(write_create_output["type"], "create");
        assert!(root.join("nested/demo.txt").exists());

        let write_update = execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\ngamma\n" }),
        )
        .expect("write update should succeed");
        let write_update_output: serde_json::Value =
            serde_json::from_str(&write_update).expect("json");
        assert_eq!(write_update_output["type"], "update");
        assert_eq!(write_update_output["originalFile"], "alpha\nbeta\nalpha\n");

        let read_full = execute_tool("read_file", &json!({ "path": "nested/demo.txt" }))
            .expect("read full should succeed");
        let read_full_output: serde_json::Value = serde_json::from_str(&read_full).expect("json");
        assert_eq!(read_full_output["file"]["content"], "alpha\nbeta\ngamma");
        assert_eq!(read_full_output["file"]["startLine"], 1);

        let read_slice = execute_tool(
            "read_file",
            &json!({ "path": "nested/demo.txt", "offset": 1, "limit": 1 }),
        )
        .expect("read slice should succeed");
        let read_slice_output: serde_json::Value = serde_json::from_str(&read_slice).expect("json");
        assert_eq!(read_slice_output["file"]["content"], "beta");
        assert_eq!(read_slice_output["file"]["startLine"], 2);

        let read_past_end = execute_tool(
            "read_file",
            &json!({ "path": "nested/demo.txt", "offset": 50 }),
        )
        .expect("read past EOF should succeed");
        let read_past_end_output: serde_json::Value =
            serde_json::from_str(&read_past_end).expect("json");
        assert_eq!(read_past_end_output["file"]["content"], "");
        assert_eq!(read_past_end_output["file"]["startLine"], 4);

        let read_error = execute_tool("read_file", &json!({ "path": "missing.txt" }))
            .expect_err("missing file should fail");
        assert!(!read_error.is_empty());

        let edit_once = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "alpha", "new_string": "omega" }),
        )
        .expect("single edit should succeed");
        let edit_once_output: serde_json::Value = serde_json::from_str(&edit_once).expect("json");
        assert_eq!(edit_once_output["replaceAll"], false);
        assert_eq!(
            fs::read_to_string(root.join("nested/demo.txt")).expect("read file"),
            "omega\nbeta\ngamma\n"
        );

        execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\nalpha\n" }),
        )
        .expect("reset file");
        let edit_all = execute_tool(
            "edit_file",
            &json!({
                "path": "nested/demo.txt",
                "old_string": "alpha",
                "new_string": "omega",
                "replace_all": true
            }),
        )
        .expect("replace all should succeed");
        let edit_all_output: serde_json::Value = serde_json::from_str(&edit_all).expect("json");
        assert_eq!(edit_all_output["replaceAll"], true);
        assert_eq!(
            fs::read_to_string(root.join("nested/demo.txt")).expect("read file"),
            "omega\nbeta\nomega\n"
        );

        let edit_same = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "omega", "new_string": "omega" }),
        )
        .expect("identical old/new should be an idempotent no-op");
        let edit_same_output: serde_json::Value = serde_json::from_str(&edit_same).expect("json");
        assert_eq!(edit_same_output["structuredPatch"], json!([]));
        assert_eq!(
            fs::read_to_string(root.join("nested/demo.txt")).expect("read file"),
            "omega\nbeta\nomega\n"
        );

        let edit_missing = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "missing", "new_string": "omega" }),
        )
        .expect_err("missing substring should fail");
        assert!(edit_missing.contains("old_string not found"));

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn glob_and_grep_tools_cover_success_and_errors() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("search-suite");
        fs::create_dir_all(root.join("nested")).expect("create root");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        fs::write(
            root.join("nested/lib.rs"),
            "fn main() {}\nlet alpha = 1;\nlet alpha = 2;\n",
        )
        .expect("write rust file");
        fs::write(root.join("nested/notes.txt"), "alpha\nbeta\n").expect("write txt file");

        let globbed = execute_tool("glob_search", &json!({ "pattern": "nested/*.rs" }))
            .expect("glob should succeed");
        let globbed_output: serde_json::Value = serde_json::from_str(&globbed).expect("json");
        assert_eq!(globbed_output["numFiles"], 1);
        assert!(globbed_output["filenames"][0]
            .as_str()
            .expect("filename")
            .ends_with("nested/lib.rs"));

        let glob_error = execute_tool("glob_search", &json!({ "pattern": "[" }))
            .expect_err("invalid glob should fail");
        assert!(!glob_error.is_empty());

        let grep_content = execute_tool(
            "grep_search",
            &json!({
                "pattern": "alpha",
                "path": "nested",
                "glob": "*.rs",
                "output_mode": "content",
                "-n": true,
                "head_limit": 1,
                "offset": 1
            }),
        )
        .expect("grep content should succeed");
        let grep_content_output: serde_json::Value =
            serde_json::from_str(&grep_content).expect("json");
        assert_eq!(grep_content_output["numFiles"], 0);
        assert!(grep_content_output["appliedLimit"].is_null());
        assert_eq!(grep_content_output["appliedOffset"], 1);
        assert!(grep_content_output["content"]
            .as_str()
            .expect("content")
            .contains("let alpha = 2;"));

        let grep_count = execute_tool(
            "grep_search",
            &json!({ "pattern": "alpha", "path": "nested", "output_mode": "count" }),
        )
        .expect("grep count should succeed");
        let grep_count_output: serde_json::Value = serde_json::from_str(&grep_count).expect("json");
        assert_eq!(grep_count_output["numMatches"], 3);

        let grep_error = execute_tool(
            "grep_search",
            &json!({ "pattern": "(alpha", "path": "nested" }),
        )
        .expect_err("invalid regex should fail");
        assert!(!grep_error.is_empty());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sleep_waits_and_reports_duration() {
        let started = std::time::Instant::now();
        let result =
            execute_tool("Sleep", &json!({"duration_ms": 20})).expect("Sleep should succeed");
        let elapsed = started.elapsed();
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["duration_ms"], 20);
        assert!(output["message"]
            .as_str()
            .expect("message")
            .contains("Slept for 20ms"));
        assert!(elapsed >= Duration::from_millis(15));
    }

    #[test]
    fn given_excessive_duration_when_sleep_then_rejects_with_error() {
        let result = execute_tool("Sleep", &json!({"duration_ms": 999_999_999_u64}));
        let error = result.expect_err("excessive sleep should fail");
        assert!(error.contains("exceeds maximum allowed sleep"));
    }

    #[test]
    fn given_zero_duration_when_sleep_then_succeeds() {
        let result =
            execute_tool("Sleep", &json!({"duration_ms": 0})).expect("0ms sleep should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["duration_ms"], 0);
    }

    #[test]
    fn brief_returns_sent_message_and_attachment_metadata() {
        let attachment = std::env::temp_dir().join(format!(
            "clawd-brief-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&attachment, b"png-data").expect("write attachment");

        let result = execute_tool(
            "SendUserMessage",
            &json!({
                "message": "hello user",
                "attachments": [attachment.display().to_string()],
                "status": "normal"
            }),
        )
        .expect("SendUserMessage should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["message"], "hello user");
        assert!(output["sentAt"].as_str().is_some());
        assert_eq!(output["attachments"][0]["isImage"], true);
        let _ = std::fs::remove_file(attachment);
    }

    #[test]
    fn config_reads_and_writes_supported_values() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "clawd-config-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".claw")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".claw")).expect("cwd dir");
        std::fs::write(
            home.join(".claw").join("settings.json"),
            r#"{"verbose":false}"#,
        )
        .expect("write global settings");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let get = execute_tool("Config", &json!({"setting": "verbose"})).expect("get config");
        let get_output: serde_json::Value = serde_json::from_str(&get).expect("json");
        assert_eq!(get_output["value"], false);

        let set = execute_tool(
            "Config",
            &json!({"setting": "permissions.defaultMode", "value": "plan"}),
        )
        .expect("set config");
        let set_output: serde_json::Value = serde_json::from_str(&set).expect("json");
        assert_eq!(set_output["operation"], "set");
        assert_eq!(set_output["newValue"], "plan");

        let invalid = execute_tool(
            "Config",
            &json!({"setting": "permissions.defaultMode", "value": "bogus"}),
        )
        .expect_err("invalid config value should error");
        assert!(invalid.contains("Invalid value"));

        let unknown =
            execute_tool("Config", &json!({"setting": "nope"})).expect("unknown setting result");
        let unknown_output: serde_json::Value = serde_json::from_str(&unknown).expect("json");
        assert_eq!(unknown_output["success"], false);

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn enter_and_exit_plan_mode_round_trip_existing_local_override() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "clawd-plan-mode-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".claw")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".claw")).expect("cwd dir");
        std::fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{"permissions":{"defaultMode":"acceptEdits"}}"#,
        )
        .expect("write local settings");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let enter = execute_tool("EnterPlanMode", &json!({})).expect("enter plan mode");
        let enter_output: serde_json::Value = serde_json::from_str(&enter).expect("json");
        assert_eq!(enter_output["changed"], true);
        assert_eq!(enter_output["managed"], true);
        assert_eq!(enter_output["previousLocalMode"], "acceptEdits");
        assert_eq!(enter_output["currentLocalMode"], "plan");

        let local_settings = std::fs::read_to_string(cwd.join(".claw").join("settings.local.json"))
            .expect("local settings after enter");
        assert!(local_settings.contains(r#""defaultMode": "plan""#));
        let state =
            std::fs::read_to_string(cwd.join(".claw").join("tool-state").join("plan-mode.json"))
                .expect("plan mode state");
        assert!(state.contains(r#""hadLocalOverride": true"#));
        assert!(state.contains(r#""previousLocalMode": "acceptEdits""#));

        let exit = execute_tool("ExitPlanMode", &json!({})).expect("exit plan mode");
        let exit_output: serde_json::Value = serde_json::from_str(&exit).expect("json");
        assert_eq!(exit_output["changed"], true);
        assert_eq!(exit_output["managed"], false);
        assert_eq!(exit_output["previousLocalMode"], "acceptEdits");
        assert_eq!(exit_output["currentLocalMode"], "acceptEdits");

        let local_settings = std::fs::read_to_string(cwd.join(".claw").join("settings.local.json"))
            .expect("local settings after exit");
        assert!(local_settings.contains(r#""defaultMode": "acceptEdits""#));
        assert!(!cwd
            .join(".claw")
            .join("tool-state")
            .join("plan-mode.json")
            .exists());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exit_plan_mode_clears_override_when_enter_created_it_from_empty_local_state() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "clawd-plan-mode-empty-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".claw")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".claw")).expect("cwd dir");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("CLAW_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("CLAW_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let enter = execute_tool("EnterPlanMode", &json!({})).expect("enter plan mode");
        let enter_output: serde_json::Value = serde_json::from_str(&enter).expect("json");
        assert_eq!(enter_output["previousLocalMode"], serde_json::Value::Null);
        assert_eq!(enter_output["currentLocalMode"], "plan");

        let exit = execute_tool("ExitPlanMode", &json!({})).expect("exit plan mode");
        let exit_output: serde_json::Value = serde_json::from_str(&exit).expect("json");
        assert_eq!(exit_output["changed"], true);
        assert_eq!(exit_output["currentLocalMode"], serde_json::Value::Null);

        let local_settings = std::fs::read_to_string(cwd.join(".claw").join("settings.local.json"))
            .expect("local settings after exit");
        let local_settings_json: serde_json::Value =
            serde_json::from_str(&local_settings).expect("valid settings json");
        assert_eq!(
            local_settings_json.get("permissions"),
            None,
            "permissions override should be removed on exit"
        );
        assert!(!cwd
            .join(".claw")
            .join("tool-state")
            .join("plan-mode.json")
            .exists());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("CLAW_CONFIG_HOME", value),
            None => std::env::remove_var("CLAW_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn structured_output_echoes_input_payload() {
        let result = execute_tool("StructuredOutput", &json!({"ok": true, "items": [1, 2, 3]}))
            .expect("StructuredOutput should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["data"], "Structured output provided successfully");
        assert_eq!(output["structured_output"]["ok"], true);
        assert_eq!(output["structured_output"]["items"][1], 2);
    }

    #[test]
    fn given_empty_payload_when_structured_output_then_rejects_with_error() {
        let result = execute_tool("StructuredOutput", &json!({}));
        let error = result.expect_err("empty payload should fail");
        assert!(error.contains("must not be empty"));
    }

    #[test]
    fn repl_executes_python_code() {
        let result = execute_tool(
            "REPL",
            &json!({"language": "python", "code": "print(1 + 1)", "timeout_ms": 500}),
        )
        .expect("REPL should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["language"], "python");
        assert_eq!(output["exitCode"], 0);
        assert!(output["stdout"].as_str().expect("stdout").contains('2'));
    }

    #[test]
    fn given_empty_code_when_repl_then_rejects_with_error() {
        let result = execute_tool("REPL", &json!({"language": "python", "code": "   "}));

        let error = result.expect_err("empty REPL code should fail");
        assert!(error.contains("code must not be empty"));
    }

    #[test]
    fn given_unsupported_language_when_repl_then_rejects_with_error() {
        let result = execute_tool("REPL", &json!({"language": "ruby", "code": "puts 1"}));

        let error = result.expect_err("unsupported REPL language should fail");
        assert!(error.contains("unsupported REPL language: ruby"));
    }

    #[test]
    fn given_timeout_ms_when_repl_blocks_then_returns_timeout_error() {
        let result = execute_tool(
            "REPL",
            &json!({
                "language": "python",
                "code": "import time\ntime.sleep(1)",
                "timeout_ms": 10
            }),
        );

        let error = result.expect_err("timed out REPL execution should fail");
        assert!(error.contains("REPL execution exceeded timeout of 10 ms"));
    }

    #[test]
    fn powershell_runs_via_stub_shell() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "clawd-pwsh-bin-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let script = dir.join("pwsh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while [ "$1" != "-Command" ] && [ $# -gt 0 ]; do shift; done
shift
printf 'pwsh:%s' "$1"
"#,
        )
        .expect("write script");
        std::process::Command::new("/bin/chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .expect("chmod");
        let original_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", dir.display(), original_path));

        let result = execute_tool(
            "PowerShell",
            &json!({"command": "Write-Output hello", "timeout": 1000}),
        )
        .expect("PowerShell should succeed");

        let background = execute_tool(
            "PowerShell",
            &json!({"command": "Write-Output hello", "run_in_background": true}),
        )
        .expect("PowerShell background should succeed");

        std::env::set_var("PATH", original_path);
        let _ = std::fs::remove_dir_all(dir);

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["stdout"], "pwsh:Write-Output hello");
        assert!(output["stderr"].as_str().expect("stderr").is_empty());

        let background_output: serde_json::Value = serde_json::from_str(&background).expect("json");
        assert!(background_output["backgroundTaskId"].as_str().is_some());
        assert_eq!(background_output["backgroundedByUser"], true);
        assert_eq!(background_output["assistantAutoBackgrounded"], false);
    }

    #[test]
    fn powershell_errors_when_shell_is_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_path = std::env::var("PATH").unwrap_or_default();
        let empty_dir = std::env::temp_dir().join(format!(
            "clawd-empty-bin-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&empty_dir).expect("create empty dir");
        std::env::set_var("PATH", empty_dir.display().to_string());

        let err = execute_tool("PowerShell", &json!({"command": "Write-Output hello"}))
            .expect_err("PowerShell should fail when shell is missing");

        std::env::set_var("PATH", original_path);
        let _ = std::fs::remove_dir_all(empty_dir);

        assert!(err.contains("PowerShell executable not found"));
    }

    struct TestServer {
        addr: SocketAddr,
        shutdown: Option<std::sync::mpsc::Sender<()>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn spawn(handler: Arc<dyn Fn(&str) -> HttpResponse + Send + Sync + 'static>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            listener
                .set_nonblocking(true)
                .expect("set nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let (tx, rx) = std::sync::mpsc::channel::<()>();

            let handle = thread::spawn(move || loop {
                if rx.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0_u8; 4096];
                        let size = stream.read(&mut buffer).expect("read request");
                        let request = String::from_utf8_lossy(&buffer[..size]).into_owned();
                        let request_line = request.lines().next().unwrap_or_default().to_string();
                        let response = handler(&request_line);
                        stream
                            .write_all(response.to_bytes().as_slice())
                            .expect("write response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("server accept failed: {error}"),
                }
            });

            Self {
                addr,
                shutdown: Some(tx),
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                handle.join().expect("join test server");
            }
        }
    }

    struct HttpResponse {
        status: u16,
        reason: &'static str,
        content_type: &'static str,
        body: String,
    }

    impl HttpResponse {
        fn html(status: u16, reason: &'static str, body: &str) -> Self {
            Self {
                status,
                reason,
                content_type: "text/html; charset=utf-8",
                body: body.to_string(),
            }
        }

        fn text(status: u16, reason: &'static str, body: &str) -> Self {
            Self {
                status,
                reason,
                content_type: "text/plain; charset=utf-8",
                body: body.to_string(),
            }
        }

        fn to_bytes(&self) -> Vec<u8> {
            format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                self.status,
                self.reason,
                self.content_type,
                self.body.len(),
                self.body
            )
            .into_bytes()
        }
    }

    /// 验证 ProviderRuntimeClient 保留调用方显式指定的模型
    #[test]
    fn provider_runtime_client_creates_with_default_config() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let client = ProviderRuntimeClient::new_in_directory(
            String::from("claude-opus-4-6"),
            BTreeSet::from([String::from("read_file")]),
            &test_working_directory(),
        );

        match &client {
            Ok(c) => {
                assert_eq!(
                    c.model, "claude-opus-4-6",
                    "子代理应该保留调用方显式指定的模型"
                );
                eprintln!("[测试] ProviderRuntimeClient 创建成功: model={}", c.model);
            }
            Err(e) => {
                panic!("ProviderRuntimeClient 创建失败: {e}");
            }
        }
    }

    /// 验证 stream() 能真正调用 API 并拿到响应
    /// 用 default provider（不走 subagent 专用模型）
    #[test]
    fn provider_runtime_client_stream_makes_real_api_call() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let config = brain_llm::config::LlmConfig::load_default().expect("config");
        let default_model = config.llm.default_model.clone();
        let default_provider = &config.llm.default_provider;

        // 直接构建 client，不走目录化构造函数（它会用 subagent 模型）
        let api_key = config.resolve_api_key(default_provider).expect("api key");
        let provider_config = config
            .llm
            .providers
            .get(default_provider.as_str())
            .expect("provider");
        let openai_config = api::OpenAiCompatConfig {
            provider_name: "test",
            api_key_env: "",
            base_url_env: "",
            default_base_url: "",
        };
        let openai_client = api::OpenAiCompatClient::new(api_key, openai_config)
            .with_base_url(provider_config.api_base.clone());
        let mut client = ProviderRuntimeClient {
            runtime: tokio::runtime::Runtime::new().expect("runtime"),
            client: api::ProviderClient::OpenAi(openai_client),
            provider: default_provider.clone(),
            model: default_model.clone(),
            max_output_tokens: api::max_tokens_for_model(&default_model),
            temperature: 0.0,
            allowed_tools: BTreeSet::new(),
        };

        eprintln!(
            "[测试] client model={}, provider={}",
            client.model, default_provider
        );

        let request = ApiRequest {
            system_prompt: vec![String::from(
                "You are a helpful assistant. Reply in one short sentence.",
            )],
            messages: vec![ConversationMessage::user_text("What is 1+1?")],
        };

        let result = client.stream(request);

        match &result {
            Ok(events) => {
                eprintln!("[测试] stream 返回 {} 个事件", events.len());
                for (i, event) in events.iter().enumerate() {
                    eprintln!("[测试]   event[{}]: {:?}", i, event);
                }
                // 至少要有一个 TextDelta 或 ToolUse
                let has_content = events.iter().any(|e| {
                    matches!(e, AssistantEvent::TextDelta(t) if !t.is_empty())
                        || matches!(e, AssistantEvent::ToolUse { .. })
                });
                assert!(has_content, "stream 应该返回文本或工具调用");
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e, AssistantEvent::MessageStop)),
                    "stream 应该包含 MessageStop"
                );
            }
            Err(e) => {
                panic!("stream() 调用失败: {e}\n这是子代理不可用的根因——API 调用本身就有问题");
            }
        }
    }

    /// 端到端：用 default provider 通过 AgentRuntime 跑一轮（无工具，纯对话）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_real_run_turn_no_tools() {
        let config = brain_llm::config::LlmConfig::load_default().expect("config");
        let default_model = config.llm.default_model.clone();
        let default_provider = &config.llm.default_provider;
        let api_key = config.resolve_api_key(default_provider).expect("api key");
        let provider_config = config
            .llm
            .providers
            .get(default_provider.as_str())
            .expect("provider");
        let openai_config = api::OpenAiCompatConfig {
            provider_name: "test",
            api_key_env: "",
            base_url_env: "",
            default_base_url: "",
        };
        let openai_client = api::OpenAiCompatClient::new(api_key, openai_config)
            .with_base_url(provider_config.api_base.clone());
        let api_client = ProviderRuntimeClient {
            runtime: tokio::runtime::Runtime::new().expect("runtime"),
            client: api::ProviderClient::OpenAi(openai_client),
            provider: default_provider.clone(),
            model: default_model.clone(),
            max_output_tokens: api::max_tokens_for_model(&default_model),
            temperature: 0.0,
            allowed_tools: BTreeSet::new(),
        };

        eprintln!("[测试] 创建 AgentRuntime, model={}", api_client.model);
        let model = api_client.resolved_model_policy();
        let profile = build_agent_profile_in_directory("Explore", &test_working_directory())
            .expect("profile");
        let sink = TestArtifactSink::default();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let pool = AgentWorkerPool::new(1).expect("worker pool");
        let lease = pool.acquire(&cancellation).await.expect("worker lease");
        let runtime = AgentRuntime::new(
            api_client,
            SubagentToolExecutor::new(BTreeSet::new(), std::env::current_dir().expect("cwd")),
            agent_permission_policy(),
            sink,
        );
        let spec = AgentRunSpec {
            agent_instance_id: "real-agent".into(),
            instance_run_id: "real-run".into(),
            member_id: None,
            inbox_item_id: None,
            task_run_id: "real-task".into(),
            node_id: "real-node".into(),
            profile,
            context_snapshot: ContextSnapshot::from_text(
                "real-context",
                "What is the capital of France? Reply in one short sentence.",
            )
            .expect("context"),
            input_artifacts: Vec::new(),
            model: model.clone(),
            reasoning: ReasoningPolicy::medium(),
            budget_reservation: BudgetReservation {
                reservation_id: "real-budget".into(),
                max_input_tokens: u32::MAX,
                max_output_tokens: model.max_output_tokens,
            },
            deadline_unix_ms: None,
        };

        let outcome = runtime.spawn(spec, lease, cancellation).wait().await;

        assert_eq!(
            outcome.status,
            AgentRunStatus::Completed,
            "AgentRuntime 失败: {:?}",
            outcome.error
        );
        assert!(
            outcome
                .artifact
                .as_ref()
                .is_some_and(|artifact| !artifact.content.is_empty()),
            "AgentRuntime 应该返回非空 Artifact"
        );
    }

    /// 诊断测试：用全部 MVP 工具定义（和子代理完全一致）测 MiMo
    #[test]
    fn diagnose_mimo_stream_with_all_mvp_tools() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let config = brain_llm::config::LlmConfig::load_default().expect("config");
        let default_provider = &config.llm.default_provider;
        let api_key = config.resolve_api_key(default_provider).expect("api key");
        let provider_config = config
            .llm
            .providers
            .get(default_provider.as_str())
            .expect("provider");
        let model = &config.llm.default_model;
        let endpoint = format!("{}/chat/completions", provider_config.api_base);

        // 用和子代理完全一样的工具集
        let allowed_tools = allowed_tools_for_subagent("Explore");
        let tools: Vec<serde_json::Value> =
            super::tool_specs_for_allowed_tools(Some(&allowed_tools))
                .into_iter()
                .map(|spec| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": spec.name,
                            "description": spec.description,
                            "parameters": spec.input_schema,
                        }
                    })
                })
                .collect();

        eprintln!("[测试] 工具数量: {}", tools.len());
        for t in &tools {
            eprintln!("  - {}", t["function"]["name"]);
        }

        let payload = serde_json::json!({
            "model": model,
            "max_tokens": 64000,
            "messages": [
                {"role": "system", "content": "You are a sub-agent."},
                {"role": "user", "content": "Read the file /tmp/test.txt"}
            ],
            "stream": true,
            "tools": tools,
            "tool_choice": "auto"
        });

        let payload_str = serde_json::to_string(&payload).expect("json");
        eprintln!("[测试] 请求大小: {} bytes", payload_str.len());

        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&endpoint)
            .header("content-type", "application/json")
            .bearer_auth(&api_key)
            .body(payload_str)
            .send()
            .expect("HTTP 请求失败");

        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        eprintln!(
            "[测试] MiMo 响应: {}\n{}",
            status,
            &body[..body.len().min(1000)]
        );
        assert!(
            status.is_success(),
            "MiMo 全工具 stream 返回 {}: {}",
            status,
            body
        );
    }

    /// 端到端：模拟真实子代理场景 — 带工具（read_file + glob_search）+ tool loop
    /// 这是最接近实际使用的测试：LLM 需要调用工具、拿到结果、再生成最终回答
    /// 需要配置支持 tool calling 的模型（如 deepseek），在 brain_models.subagent 中指定
    #[test]
    fn subagent_real_run_turn_with_tools() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // 用 subagent 模型跑完整 tool loop
        let llm_config = brain_llm::config::LlmConfig::load_default().expect("config");
        let subagent_model = llm_config.model_for_brain("subagent").to_string();
        eprintln!("[测试] 子代理模型: {subagent_model}");

        // 创建一个临时文件让子代理去读
        let tmp_file = temp_path("subagent-e2e-test.txt");
        std::fs::write(
            &tmp_file,
            "AI Brain v2 sub-agent test file.\nThe answer is 42.",
        )
        .expect("write tmp file");
        let tmp_path_str = tmp_file.display().to_string();

        let allowed_tools = BTreeSet::from([
            String::from("read_file"),
            String::from("glob_search"),
            String::from("grep_search"),
        ]);

        let api_client = ProviderRuntimeClient::new_in_directory(
            String::new(),
            allowed_tools.clone(),
            &test_working_directory(),
        )
        .expect("ProviderRuntimeClient 应该创建成功");

        eprintln!("[测试] 创建带工具的 runtime, model={}", api_client.model);

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            api_client,
            SubagentToolExecutor::new(
                allowed_tools,
                std::env::current_dir().expect("cwd"),
            ),
            agent_permission_policy(),
            vec![format!(
                "You are a sub-agent. Read the file at {tmp_path_str} and report its contents. Use the read_file tool."
            )],
        )
        .with_max_iterations(10);

        let prompt = format!("Please read the file at {tmp_path_str} and tell me what it says.");
        let result = runtime.run_turn(&prompt, None);

        match &result {
            Ok(summary) => {
                let text = final_assistant_text(summary);
                eprintln!(
                    "[测试] run_turn (with tools) 成功, iterations={}, answer={}",
                    summary.iterations,
                    &text[..text.len().min(300)]
                );
                assert!(!text.is_empty(), "带工具的 run_turn 应该返回非空文本");
                assert!(
                    summary.iterations >= 1,
                    "带工具的 run_turn 至少要有 1 次迭代"
                );

                // 验证 session 里确实有工具调用
                let has_tool_use = runtime
                    .session()
                    .messages
                    .iter()
                    .flat_map(|m| m.blocks.iter())
                    .any(|b| matches!(b, runtime::ContentBlock::ToolUse { .. }));
                let has_tool_result = runtime
                    .session()
                    .messages
                    .iter()
                    .flat_map(|m| m.blocks.iter())
                    .any(|b| matches!(b, runtime::ContentBlock::ToolResult { .. }));
                eprintln!(
                    "[测试] has_tool_use={}, has_tool_result={}",
                    has_tool_use, has_tool_result
                );
                assert!(has_tool_use, "session 应该包含工具调用");
                assert!(has_tool_result, "session 应该包含工具结果");
            }
            Err(e) => {
                panic!("带工具的 run_turn 失败: {e}\n这是子代理带工具不可用的根因");
            }
        }

        let _ = std::fs::remove_file(&tmp_file);
    }
}
