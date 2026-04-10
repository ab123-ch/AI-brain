# claw-code-parity 架构深度分析报告
> 目标：为 AI 大脑 Agent 工程提供架构参考

---

## 一、项目定位

**claw-code-parity** 是 Claude Code CLI（Anthropic 官方 AI 编程助手）的开源 Rust 重写。它实现了一个完整的 **AI Agent Harness**——能接收用户指令、调用 LLM、执行工具、管理上下文的自主代理系统。

---

## 二、整体架构（8 Crate 分层）

```
┌─────────────────────────────────────────────────────────────────┐
│                     rusty-claude-cli (CLI 层)                    │
│   REPL · 流式渲染 · 斜杠命令 · 会话管理 · 参数解析               │
├─────────────────────────────────────────────────────────────────┤
│  commands        │  tools           │  compat-harness            │
│  (命令注册表)     │  (18+ 工具实现)   │  (TS 清单对齐)             │
├─────────────────────────────────────────────────────────────────┤
│                         api (API 客户端层)                       │
│   Anthropic / xAI / OpenAI · SSE 流解析 · Prompt Cache          │
├─────────────────────────────────────────────────────────────────┤
│                    runtime (核心运行时)                           │
│   会话 · 对话循环 · 压缩 · 权限 · Hook · MCP · Prompt 组装      │
├──────────────────────┬──────────────────────────────────────────┤
│   plugins (插件系统)  │  telemetry (遥测追踪)                     │
│   Hook 注册/聚合      │  SessionTracer · JSONL 持久化             │
└──────────────────────┴──────────────────────────────────────────┘
```

**依赖关系**：`telemetry/plugins` → `runtime` → `api/commands/tools` → `cli`

---

## 三、核心控制流：AI 如何被控制

### 3.1 对话循环（Agent Loop）

**核心文件**：`runtime/src/conversation.rs`

```
用户输入
  │
  ▼
┌─────────────────────────────────────────────┐
│  run_turn() — 迭代式 Agent 循环              │
│                                             │
│  1. 构建 ApiRequest { system_prompt,         │
│     messages, tools }                        │
│                    │                         │
│                    ▼                         │
│  2. 调用 api_client.stream(request)          │
│     → LLM 返回流式响应                        │
│                    │                         │
│                    ▼                         │
│  3. 解析为 ConversationMessage               │
│     (含 Text + ToolUse blocks)               │
│                    │                         │
│                    ▼                         │
│  4. 提取 pending_tool_uses                   │
│     如果没有 → 结束循环                       │
│                    │                         │
│                    ▼                         │
│  5. 对每个 tool_use:                         │
│     a. run_pre_tool_use_hook()  ← Hook 前置  │
│     b. authorize_with_context() ← 权限检查   │
│     c. tool_executor.execute()  ← 执行工具   │
│     d. run_post_tool_use_hook() ← Hook 后置  │
│     e. push_message(tool_result)             │
│                    │                         │
│                    ▼                         │
│  6. 回到步骤 2（带工具结果继续对话）            │
│     直到无更多 tool_use 或达到 max_iterations │
│                    │                         │
│                    ▼                         │
│  7. maybe_auto_compact() — 上下文自动压缩     │
│  8. 返回 TurnSummary                         │
└─────────────────────────────────────────────┘
```

**关键设计**：LLM 的响应不是最终答案，而是**指令**——它决定调用什么工具，系统执行后把结果喂回 LLM，LLM 再决定下一步。这是经典的 **ReAct（Reasoning + Acting）模式**。

### 3.2 系统提示构建（控制 AI 行为的核心）

**核心文件**：`runtime/src/prompt.rs`

```
SystemPromptBuilder 按顺序组装:
│
├── 1. Intro section — 基础身份定义
├── 2. Output Style — 输出风格配置
├── 3. System section — 系统行为规则
├── 4. Doing tasks section — 任务执行准则
├── 5. Actions section — 操作安全规范
├── 6. === DYNAMIC BOUNDARY === ← 静态/动态分界
├── 7. Environment — 模型族、工作目录、日期、平台
├── 8. Project context — git status、diff 快照
└── 9. CLAUDE.md 指令文件 — 用户自定义指令
        │
        └── discover_instruction_files():
            从当前目录向上遍历到根目录
            搜索 CLAUDE.md / CLAUDE.local.md / .claw/CLAUDE.md
            单文件限制 4,000 字符，总预算 12,000 字符
```

**记忆类比**：系统提示 = **前额叶的工作记忆**——当前任务上下文、规则约束、用户偏好都在这里。

---

## 四、工具调用系统

### 4.1 工具注册与执行

**核心文件**：`tools/src/lib.rs`、`runtime/src/conversation.rs`

```rust
// 工具注册表模式
pub struct StaticToolExecutor {
    handlers: BTreeMap<String, ToolHandler>,
}

// 每个 Handler 是一个异步函数签名
type ToolHandler = Box<dyn Fn(Value) -> Pin<Box<dyn Future<Output = String>>>>;
```

**已实现的 18+ 工具**：

| 工具 | 功能 | 类型 |
|------|------|------|
| bash | 执行 shell 命令 | 执行类 |
| read_file / write_file / edit_file | 文件 CRUD | 文件类 |
| glob_search / grep_search | 文件搜索 | 搜索类 |
| WebFetch / WebSearch | 网络访问 | 信息类 |
| TodoWrite | 任务管理 | 状态类 |
| Skill | 技能执行 | 扩展类 |
| Agent | 子 Agent 调度 | 编排类 |
| NotebookEdit | Jupyter 编辑 | 特殊类 |
| Sleep / SendUserMessage / Config | 辅助功能 | 辅助类 |

### 4.2 工具调用在 API 请求中的表示

```json
// 发送给 LLM 的工具定义（tools 参数）
{
  "name": "Bash",
  "description": "Executes a bash command...",
  "input_schema": {
    "type": "object",
    "properties": {
      "command": { "type": "string", "description": "The command to execute" }
    },
    "required": ["command"]
  }
}

// LLM 返回的 tool_use block
{
  "type": "tool_use",
  "id": "toolu_abc123",
  "name": "Bash",
  "input": { "command": "cargo test" }
}

// 系统返回的 tool_result block
{
  "type": "tool_result",
  "tool_use_id": "toolu_abc123",
  "content": "running 42 tests..."
}
```

**这是整个 AI 控制的核心协议**——LLM 不直接执行任何操作，它通过 `tool_use` 结构告诉系统"我想做什么"，系统执行后通过 `tool_result` 反馈结果。

### 4.3 工具执行的三层来源

工具可从三个来源注册，按优先级：

```
1. 内置工具 (StaticToolExecutor)
   └── BTreeMap<String, ToolHandler> 硬编码注册
       bash / read_file / write_file / edit_file / ...

2. MCP 工具 (McpServerManager)
   └── tool_index: BTreeMap<String, ToolRoute>
       key = "mcp__{server}__{tool}"
       如: "mcp__memory-system__memory_save"

3. 插件工具 (PluginTool)
   └── 从 plugin.json 的 tools 字段加载
       子进程执行，通过 stdin JSON 传入参数
       环境变量: CLAWD_PLUGIN_ID / CLAWD_TOOL_NAME / CLAWD_TOOL_INPUT
```

**冲突检测**：插件工具名称与内置工具冲突时拒绝注册。

### 4.4 Skill 系统（技能加载机制）

**核心文件**：`tools/src/lib.rs`（Skill 工具实现）

Skill 不是预注册的工具，而是**按需加载的提示词文件**：

```
Skill 工作流:
│
├── 1. LLM 调用 Skill 工具: { skill: "commit", args: "-m 'fix bug'" }
│
├── 2. resolve_skill_path() 搜索技能文件:
│   ├── $CODEX_HOME/.agents/skills/{name}/SKILL.md
│   ├── $HOME/.codex/skills/{name}/SKILL.md
│   ├── $HOME/.agents/skills/{name}/SKILL.md
│   └── 项目目录下递归查找
│
├── 3. 读取 SKILL.md 文件内容（提示词 + 指令）
│
├── 4. parse_skill_description() 提取描述
│   └── 取第一个 # 标题行作为描述
│
└── 5. 返回 SkillOutput { name, path, description, prompt }
    └── 提示词被注入到后续对话中，指导 LLM 执行该技能
```

**关键设计**：Skill 本质是**延迟加载的 System Prompt 片段**——LLM 先通过 ToolSearch 发现技能，再通过 Skill 工具加载技能的完整提示词。

### 4.5 Agent 工具（子代理编排）

```rust
struct AgentInput {
    description: String,      // 3-5 词描述
    prompt: String,           // 子代理的完整任务描述
    subagent_type: Option<String>,  // 子代理类型
    name: Option<String>,     // 子代理名称
    model: Option<String>,    // 可指定独立模型
}
```

Agent 工具实现了 **Agent-as-Tool 模式**——主 Agent 通过工具调用生成子 Agent，每个子 Agent 拥有独立的：
- `ConversationRuntime` 实例（独立对话循环）
- `Session`（独立消息历史）
- 系统提示词和工具列表
- 会话历史压缩功能

### 4.6 ToolSearch 工具（按需发现）

```
ToolSearch 支持延迟加载工具发现:
│
├── 搜索语法: { query: "skill name or keyword" }
│
├── 精确选择: { query: "select:tool_name" }
│
├── 匹配逻辑:
│   ├── +term → 必须包含的术语
│   ├── -term → 排除的术语
│   └── max_results → 限制返回数量
│
└── 返回: 匹配的工具列表（name + description）
```

### 4.7 API 多 Provider 支持

```
ProviderClient 支持三种提供商:
│
├── Anthropic (默认)
│   ├── 端点: https://api.anthropic.com/v1/messages
│   ├── 认证: ANTHROPIC_API_KEY + ANTHROPIC_AUTH_TOKEN
│   ├── 支持 OAuth Bearer Token
│   └── 模型别名: opus→claude-opus-4-6, sonnet→claude-sonnet-4-6
│
├── OpenAI 兼容
│   ├── 环境变量: OPENAI_API_KEY
│   └── 自动检测: 模型名不含 "claude" 时使用
│
└── xAI
    ├── 环境变量: XAI_API_KEY
    └── 支持 Grok 系列模型

自动检测逻辑:
  模型名含 "claude" → Anthropic
  否则 → 检测环境变量 → fallback Anthropic
```

---

## 五、MCP（Model Context Protocol）集成

### 5.1 分层架构

```
┌────────────────────────────────────────────────┐
│           McpServerManager (编排层)              │
│  · 管理多个 MCP Server 生命周期                  │
│  · 全局工具路由索引 (tool_index)                  │
│  · 自动重试 + reset 恢复机制                      │
├────────────────────────────────────────────────┤
│          McpClientBootstrap (引导层)             │
│  · 配置 → 传输实例转换                            │
│  · 工具名前缀计算 (mcp__{server}__{tool})         │
│  · 签名生成（标识服务唯一性）                      │
├────────────────────────────────────────────────┤
│          McpStdioProcess (传输层)                │
│  · 子进程 stdin/stdout 通信                       │
│  · Content-Length 帧协议                         │
│  · JSON-RPC 2.0 消息格式                         │
├────────────────────────────────────────────────┤
│          JSON-RPC 协议层                         │
│  · initialize / tools/list / tools/call          │
│  · resources/list / resources/read               │
│  · protocolVersion: "2025-03-26"                 │
└────────────────────────────────────────────────┘
```

### 5.2 MCP 工具发现与调用流程

```
1. 启动阶段:
   McpServerManager::from_runtime_config(config)
   → 仅接受 Stdio 传输的服务器
   → 其他类型标记为 unsupported

2. 发现阶段:
   discover_tools()
   → 对每个服务器:
     a. ensure_server_ready() — spawn 子进程 + initialize 握手
     b. tools/list — 获取工具列表（支持分页）
     c. 注册到 tool_index: key = "mcp__{server}__{tool}"

3. 调用阶段:
   call_tool("mcp__memory-system__memory_save", args)
   → tool_index 路由到目标服务器
   → 确保服务器就绪（按需 spawn，含自动重试）
   → 通过 McpStdioProcess 发送 tools/call
   → 出错时自动 reset_server（kill + 重新 spawn）
```

### 5.3 帧协议

```
帧格式: Content-Length: {n}\r\n\r\n{json_payload}

请求:
→ {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{...}}

响应:
← {"jsonrpc":"2.0","id":1,"result":{...}}

错误:
← {"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"..."}}
```

---

## 六、Hook 系统（干预点）

### 6.1 三种 Hook 事件

| 事件 | 触发时机 | 能力 |
|------|---------|------|
| **PreToolUse** | 工具执行前 | 拒绝执行、修改输入、覆盖权限决策 |
| **PostToolUse** | 工具执行成功后 | 修改输出、添加上下文 |
| **PostToolUseFailure** | 工具执行失败后 | 错误处理、重试建议 |

### 6.2 Hook 的控制能力

```json
// Hook 进程的 stdout 输出可包含：
{
  "systemMessage": "注意：此操作涉及生产环境",
  "reason": "需要用户确认",
  "decision": "block",                    // 拒绝执行
  "hookSpecificOutput": {
    "permissionDecision": "ask",           // 覆盖权限决策
    "permissionDecisionReason": "高危操作",
    "updatedInput": {                      // 修改工具输入
      "command": "echo 'safe command'"
    },
    "additionalContext": "..."             // 额外上下文
  }
}
```

### 6.3 Hook 在对话循环中的位置

```
LLM 返回 tool_use
       │
       ▼
  ┌─ PreToolUse Hook ─┐
  │ 可修改输入         │
  │ 可拒绝执行         │
  │ 可覆盖权限         │
  └────────┬──────────┘
           ▼
  ┌─ Permission Check ─┐
  │ 结合 Hook 结果 +    │
  │ 权限策略做最终决策  │
  └────────┬───────────┘
           ▼
  ┌─ Tool Execution ───┐
  │ 执行工具            │
  └────────┬───────────┘
           ▼
  ┌─ PostToolUse Hook ─┐ (成功)
  │ 或                  │
  │ PostToolUseFailure  │ (失败)
  └────────┬───────────┘
           ▼
     结果返回 LLM
```

**记忆类比**：Hook = **基底节的动作选择机制**——在动作执行前进行筛选、调制和门控。

---

## 七、上下文管理（记忆系统）

### 7.1 会话数据模型

```rust
pub struct Session {
    pub session_id: String,
    pub messages: Vec<ConversationMessage>,    // 有序消息列表
    pub compaction: Option<SessionCompaction>, // 压缩历史
    pub fork: Option<SessionFork>,             // 分叉信息
}

pub enum ContentBlock {
    Text { text: String },                          // 文本内容
    ToolUse { id, name, input },                    // 工具调用指令
    ToolResult { tool_use_id, tool_name, output, is_error }, // 工具执行结果
}
```

### 7.2 Token 预算与自动压缩

```
Token 估算: 字符数 / 4

自动压缩触发条件:
  累积 input_tokens > 100,000 (可配置)
       │
       ▼
  ┌─ should_compact() ──────────────┐
  │ 跳过已有的压缩摘要消息            │
  │ 保留最近 4 条消息                 │
  │ 可压缩消息 token ≥ 10,000        │
  └────────────┬────────────────────┘
               ▼
  ┌─ summarize_messages() ──────────┐
  │ 统计 user/assistant/tool 消息数  │
  │ 收集使用的工具名称列表            │
  │ 提取最近 3 条用户请求             │
  │ 推断待办工作 (todo/next/pending) │
  │ 提取引用的关键文件路径            │
  │ 生成时间线摘要                    │
  └────────────┬────────────────────┘
               ▼
  ┌─ merge_compact_summaries() ─────┐
  │ 旧摘要 → "Previously compacted" │
  │ 新摘要 → "Newly compacted"      │
  │ 保留完整时间线                    │
  └────────────┬────────────────────┘
               ▼
  新会话 = [System压缩摘要] + [最近4条消息]
```

**记忆类比**：
- 会话消息 = **工作记忆**（容量有限，实时访问）
- 压缩摘要 = **长期记忆**（海马体的记忆巩固，从详细经历中提取要点）
- 自动压缩阈值 = **遗忘曲线**（信息随时间衰减）

### 7.3 会话持久化

```
存储格式: JSONL (每行一条消息)
路径: ~/.claw/sessions/{session-id}.jsonl

写入策略:
  首次保存 → 完整快照
  后续追加 → 仅追加新消息行

文件轮转:
  超过 256KB → 创建 .1/.2/.3 备份文件
  最多保留 3 个备份

原子写入:
  临时文件 + rename 保证数据完整性
```

---

## 八、权限系统（安全控制）

### 8.1 五级权限模式

```
ReadOnly < WorkspaceWrite < DangerFullAccess
                        + Prompt (询问用户) + Allow (自动允许)
```

### 8.2 决策流程

```
工具调用请求
     │
     ▼
  1. Hook 的 permission_override
     ├─ Deny → 立即拒绝
     ├─ Ask  → 要求用户确认
     └─ Allow → 继续检查
           │
           ▼
  2. deny 规则检查 → 命中则拒绝
           │
           ▼
  3. ask 规则检查 → 命中则询问用户
           │
           ▼
  4. allow 规则检查 → 命中则允许
           │
           ▼
  5. 当前权限模式 vs 工具所需模式
     ├─ 权限足够 → 执行
     └─ 权限不足 → Prompt 模式下询问用户
```

---

## 九、对 AI 大脑 Agent 工程的架构启示

### 9.1 核心设计模式提取

| 模式 | claw-code-parity 实现 | 适用于 AI 大脑 |
|------|----------------------|---------------|
| **Agent Loop** | run_turn() 迭代循环 | 大脑的思考-行动循环 |
| **Tool as Protocol** | LLM 通过 tool_use 指令控制执行 | 工具调用 = 神经元的输出信号 |
| **Hook as Interceptor** | PreToolUse/PostToolUse | 基底节的动作门控 |
| **Compaction as Consolidation** | 自动摘要压缩 | 海马体的记忆巩固 |
| **Session as Working Memory** | 有界消息窗口 | 前额叶的工作记忆 |
| **CLAUDE.md as Long-term Memory** | 指令文件发现和加载 | 大脑皮层的长期存储 |
| **MCP as Sensory Input** | 外部工具动态注册 | 感觉通路的信号接入 |
| **Permission as Inhibition** | 五级权限控制 | 前额叶的抑制控制 |

### 9.2 推荐的技术栈映射

```
AI 大脑 Agent                    ←  参考自 claw-code-parity
────────────────────────────────────────────────────────
核心 Agent 循环 (Rust)           ←  conversation.rs
工具定义与执行 (Rust)            ←  tools crate + ToolHandler
MCP 外部集成 (Rust)              ←  mcp_stdio.rs + McpServerManager
Hook 拦截系统 (Rust)             ←  hooks.rs + HookRunner
记忆持久化 (Rust + Python)       ←  session.rs (JSONL) + compact.rs
API 客户端 (Rust)                ←  api crate (多 Provider)
提示词工程 (Rust)                ←  prompt.rs (SystemPromptBuilder)
Python AI 层 (Python)            ←  langchain / 自定义记忆增强
TypeScript 接口层 (TS)           ←  Web UI / CLI 扩展
```

### 9.3 关键文件索引（深入研究用）

| 文件 | 学习目标 |
|------|---------|
| `runtime/src/conversation.rs` | Agent Loop 核心循环 |
| `runtime/src/prompt.rs` | 系统提示构建 |
| `runtime/src/session.rs` | 会话持久化 |
| `runtime/src/compact.rs` | 上下文压缩算法 |
| `runtime/src/hooks.rs` | Hook 拦截系统 |
| `runtime/src/permissions.rs` | 权限控制策略 |
| `runtime/src/mcp_stdio.rs` | MCP 协议实现 |
| `runtime/src/mcp_client.rs` | MCP 客户端引导 |
| `runtime/src/config.rs` | 配置层级管理 |
| `tools/src/lib.rs` | 工具注册与定义 |
| `api/src/providers/anthropic.rs` | API 客户端实现 |
| `api/src/prompt_cache.rs` | Prompt 缓存机制 |
