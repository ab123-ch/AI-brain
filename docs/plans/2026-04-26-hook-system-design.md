# Hook 系统设计文档

> 日期：2026-04-26
> 状态：已确认

## 背景

当前评估脑每轮都调 LLM 做完整评估（~2000-4000 tokens），浪费 token。需要一种可扩展的 hook 机制，让主脑自己决定是否需要评估脑介入。

## 方案

新建 `brain-hooks` crate，实现参考 Claude Code 的 hook 框架。

### 事件类型

| 事件 | 触发位置 | 用途 |
|------|---------|------|
| `PreToolUse` | tool_loop.rs 工具执行前 | 拦截/修改工具调用 |
| `PostToolUse` | tool_loop.rs 工具执行后 | 后处理/日志 |
| `PostQuery` | orchestrator.rs 主脑回复后 | 评估脑自决策 |
| `OnShutdown` | orchestrator.rs 关闭时 | 清理/保存 |
| `SessionStart` | orchestrator.rs 启动时 | 初始化 |

### 核心类型

```rust
// 匹配器
pub enum Matcher {
    MatchAll,
    PipeDelimited(Vec<String>),  // "Bash|Edit"
}

// Handler 类型
#[serde(tag = "type")]
pub enum HookHandler {
    #[serde(rename = "command")]
    Command { command: String, matcher: Option<String>, timeout: u64 },
    #[serde(rename = "builtin")]
    Builtin { name: String },  // "eval_gate"
}

// 输入输出
pub struct HookInput { event, tool_name, tool_input, tool_output, user_input, ai_output, ... }
pub struct HookOutput { decision: Allow/Deny, trigger_eval: bool, system_message, ... }
```

### 配置格式（config.toml）

```toml
[hooks]
enabled = true

[hooks.eval_gate]
enabled = true
model = "eval"

[[hooks.pre_tool_use]]
matcher = "Bash|Edit"
command = "echo 'check'"
timeout = 30

[[hooks.post_query]]
command = "echo 'done'"
timeout = 60
```

### Handler 支持

1. **command** — shell 命令执行（复用 runtime/hooks.rs 模式：环境变量 + stdin JSON + 退出码 0/2）
2. **builtin: eval_gate** — 内置评估脑自决策（~200 token 轻量 LLM 调用）

### 集成点

| 文件 | 位置 | 事件 |
|------|------|------|
| tool_loop.rs:172 | 工具执行前 | PreToolUse |
| tool_loop.rs:234 | 工具执行后 | PostToolUse |
| orchestrator.rs:528 | 主脑回复后 | PostQuery → eval_gate |
| orchestrator.rs:1015 | 关闭时 | OnShutdown |

### eval_gate 决策流程

```
PostQuery → 调用 LLM（~200 token prompt）
  → need_eval=true → 触发完整评估脑（2000-4000 token）
  → need_eval=false → 跳过评估
```

### Crate 结构

```
brain-hooks/
├── src/
│   ├── lib.rs
│   ├── types.rs       ← 核心类型
│   ├── runner.rs      ← HookRunner 执行引擎
│   ├── config.rs      ← config.toml [hooks] 加载
│   └── builtins.rs    ← eval_gate 内置 handler
```

### 依赖

- brain-hooks → serde, serde_json, tokio, tracing, brain-llm
- ai-brain-cli → brain-hooks
