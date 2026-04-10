# 工具定义与执行层探索报告

> 探索日期: 2026-04-09

## 一、三层工具体系

项目中存在三套互相独立的工具体系：

| 层级 | 位置 | 职责 | 状态 |
|------|------|------|------|
| **tools crate** | `crates/tools/src/lib.rs` | ToolSpec 定义 + execute_tool 路由 | ✅ 完整 |
| **runtime crate** | `crates/runtime/src/file_ops.rs`, `bash.rs`, `conversation.rs` | 底层执行函数 + tool_use 循环 | ✅ 完整 |
| **brain-motor crate** | `crates/brain-motor/src/motor_brain.rs` | AI Brain 执行脑 | ❌ stub 阶段 |

---

## 二、工具定义（ToolSpec）

### 2.1 内建工具列表

`crates/tools/src/lib.rs` — `mvp_tool_specs()` 返回 30+ 个工具：

| 工具名 | 权限级别 | 说明 |
|--------|---------|------|
| bash | DangerFullAccess | Shell 命令执行 |
| read_file | ReadOnly | 读文件 |
| write_file | WorkspaceWrite | 写文件 |
| edit_file | WorkspaceWrite | 编辑文件 |
| glob_search | ReadOnly | 文件搜索 |
| grep_search | ReadOnly | 内容搜索 |
| WebFetch | ReadOnly | 网页获取 |
| WebSearch | ReadOnly | 网页搜索 |
| TodoWrite | WorkspaceWrite | 待办管理 |
| Skill | ReadOnly | 技能调用 |
| Agent | DangerFullAccess | 子代理 |
| ToolSearch | ReadOnly | 工具搜索 |
| NotebookEdit | WorkspaceWrite | Jupyter 编辑 |
| MCP | DangerFullAccess | MCP 工具调用 |
| Task 系列 | ReadOnly/Write | 任务管理 |
| Team 系列 | DangerFullAccess | 团队管理 |
| Cron 系列 | ReadOnly | 定时任务 |
| LSP | ReadOnly | 语言服务 |
| MCP Resource 系列 | ReadOnly | MCP 资源 |

### 2.2 ToolSpec → ToolDefinition 转换

```rust
// tools/lib.rs:148
pub fn definitions(&self, allowed_tools: Option<&BTreeSet<String>>) -> Vec<ToolDefinition> {
    let builtin = mvp_tool_specs().into_iter()
        .filter(|spec| allowed_tools allows spec.name)
        .map(|spec| ToolDefinition { name, description, input_schema });
    let plugin = self.plugin_tools.iter().map(|t| ToolDefinition { ... });
    builtin.chain(plugin).collect()
}
```

---

## 三、工具执行

### 3.1 execute_tool 路由

`crates/tools/src/lib.rs:839` — 巨大的 match 路由：

```rust
pub fn execute_tool(name: &str, input: &Value) -> Result<String, String> {
    match name {
        "bash" => from_value::<BashCommandInput>(input).and_then(run_bash),
        "read_file" => from_value::<ReadFileInput>(input).and_then(run_read_file),
        "write_file" => from_value::<WriteFileInput>(input).and_then(run_write_file),
        "edit_file" => from_value::<EditFileInput>(input).and_then(run_edit_file),
        "glob_search" => from_value::<GlobSearchInputValue>(input).and_then(run_glob_search),
        "grep_search" => from_value::<GrepSearchInputValue>(input).and_then(run_grep_search),
        "MCP" => from_value::<McpToolInput>(input).and_then(run_mcp_tool),  // ← STUB!
        // ... 30+ 工具 ...
    }
}
```

### 3.2 内建工具实现

**file_ops.rs** — 5 个文件操作工具：

| 函数 | 签名 | 返回 |
|------|------|------|
| `read_file` | `(path, offset, limit) -> io::Result<ReadFileOutput>` | 结构化输出（content, num_lines, start_line） |
| `write_file` | `(path, content) -> io::Result<WriteFileOutput>` | 含 structured_patch |
| `edit_file` | `(path, old, new, replace_all) -> io::Result<EditFileOutput>` | 含 structured_patch |
| `glob_search` | `(pattern, path) -> io::Result<GlobSearchOutput>` | filenames, truncated |
| `grep_search` | `(input) -> io::Result<GrepSearchOutput>` | 按模式搜索内容 |

**bash.rs** — Bash 执行器：

```rust
execute_bash(input: BashCommandInput) -> io::Result<BashCommandOutput>
// 支持: 超时、后台运行、沙盒隔离
```

### 3.3 关键问题：MCP 工具是 STUB

```rust
// tools/lib.rs:1076 — MCP 工具未连通
fn run_mcp_tool(input: McpToolInput) -> Result<String, String> {
    to_pretty_json(json!({
        "result": null,
        "message": "MCP tool proxy not yet connected"
    }))
}
```

实际的 MCP 调用在 `runtime/mcp_stdio.rs` 的 `McpServerManager::call_tool()` 中，但 `execute_tool` 函数没有接入。

### 3.4 Plugin 工具

```rust
// plugins/src/lib.rs:304
pub fn execute(&self, input: &Value) -> Result<String, PluginError> {
    // 子进程执行，stdin 传 JSON，stdout 取结果
    Command::new(&self.command).args(&self.args).env(...)
}
```

Plugin 工具的执行是通的。

---

## 四、ToolExecutor trait 和 tool_use 循环

### 4.1 trait 定义

```rust
// runtime/conversation.rs:53
pub trait ToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError>;
}
```

**问题**：同步接口，但 MCP 调用是 async 的。

### 4.2 tool_use 循环（conversation.rs:286-460）

完整流程：

```
1. api_client.stream(request) → 发给 LLM
2. 解析 OutputContentBlock::ToolUse { id, name, input }
3. 对每个工具：
   a. pre_tool_use_hook（权限检查）
   b. permission_policy.authorize_with_context
   c. tool_executor.execute(name, input) → 结果
   d. post_tool_use_hook
   e. session.push_message(ToolResult) → 回写到会话
4. 循环回第 1 步，LLM 看到工具结果后继续
```

### 4.3 StaticToolExecutor

```rust
// conversation.rs:742
pub struct StaticToolExecutor {
    handlers: BTreeMap<String, ToolHandler>,  // FnMut(&str) -> Result<String>
}
```

闭包注册表模式。

---

## 五、MCP 客户端

### 5.1 McpServerManager

`runtime/mcp_stdio.rs`

```rust
pub struct McpServerManager {
    servers: BTreeMap<String, ManagedMcpServer>,
    tool_index: BTreeMap<String, ToolRoute>,  // qualified_name → 路由
}
```

**工具发现**：`discover_tools()` → 发 `tools/list` JSON-RPC → 注册到 tool_index

**工具调用**：`call_tool(qualified_name, arguments)` → 发 `tools/call` JSON-RPC

**工具名格式**：`mcp__{server_name}__{tool_name}`

### 5.2 与新架构的差距

- MCP 工具发现 ✅ 可复用
- MCP 工具调用 ✅ 可复用（async）
- 但 `execute_tool` 中的 MCP 路由是 STUB，需要连通
- MCP 工具定义需要传给推理脑的 LLM（作为 ToolDefinition）

---

## 六、与新架构的差距汇总

| 编号 | 差距 | 需改造的 crate | 复杂度 |
|------|------|---------------|--------|
| G1 | execute_tool 中 MCP 是 STUB | tools | 中 |
| G2 | ToolExecutor trait 是同步的，MCP 是 async | runtime | 中 |
| G3 | brain-llm ChatRequest 没有 tools 字段 | brain-llm | 低 |
| G4 | 推理脑没有 tool_use 循环 | brain-reasoning | **高** |
| G5 | 推理脑没有对话历史累积 | brain-reasoning | 中 |
| G6 | guard_check 前置安全检查未标准化 | brain-core | 中 |
| G7 | 工具结果回注 LLM 上下文路径不存在 | brain-reasoning | 中 |

### 可复用部分

| 组件 | 复用方式 |
|------|---------|
| file_ops.rs (Read/Write/Edit/Glob/Grep) | 直接调用，包装为 ToolExecutor handler |
| bash.rs | 直接调用，包装为 ToolExecutor handler |
| mvp_tool_specs() | 生成 ToolDefinition 传给推理脑 LLM |
| MCP McpServerManager | 复用工具发现和调用 |
| Plugin PluginTool::execute | 复用插件执行 |

### 需要全新实现

| 组件 | 原因 |
|------|------|
| 推理脑 tool_use 循环 | 当前完全没有，参考 conversation.rs 的 run_turn 重新实现 |
| guard_check() 函数 | 当前权限检查散落在多处，需要统一为前置检查函数 |
