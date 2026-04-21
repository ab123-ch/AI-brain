# 主脑设计 — MainBrain

> 唯一的主力 LLM 循环，等同 Claude Code 的主模型

## 1. 定位

主脑是系统的**唯一对外接口**。用户输入 → 主脑处理 → 输出给用户。
所有交互都通过主脑的 tool_loop 完成，不存在其他脑直接与用户交互。

## 2. 核心数据结构

### 2.1 对话历史

```rust
struct ConversationMessage {
    role: MessageRole,        // System / User / Assistant / Tool
    blocks: Vec<ContentBlock>, // Text / ToolUse / ToolResult
    timestamp: DateTime<Utc>,
    token_count: Option<usize>, // 预估 token 数
}
```

### 2.2 主脑状态

```rust
struct MainBrain {
    // LLM
    llm: Arc<dyn LlmProvider>,
    model: String,

    // 工具
    tool_executor: Arc<dyn ToolExecutor>,
    tool_defs: Vec<ToolDefinition>,

    // 对话历史（核心）
    messages: Vec<ConversationMessage>,

    // 进度
    progress_tx: Option<mpsc::Sender<ProgressEvent>>,

    // 记忆脑引用（只读，用于 token 估算和上下文重建）
    memory_brain: Arc<Mutex<MemoryBrain>>,

    // 配置
    config: MainBrainConfig,
}

struct MainBrainConfig {
    max_context_tokens: usize,      // 模型上下文窗口大小
    context_threshold: f64,         // 触发压缩的阈值 (0.8)
    preserve_recent_turns: usize,   // 重建时保留最近 N 轮
}
```

## 3. 核心流程

### 3.1 处理用户输入

```
用户输入
  │
  ├─ 构建 user message，追加到 messages
  │
  ├─ 估算当前 token 数
  │   └─ 如果 > 80% → 请求记忆脑触发上下文重建
  │
  ├─ 进入 tool_loop:
  │   │
  │   ├─ 发送 ProgressEvent::Connecting
  │   ├─ 构建 ChatRequest { messages, tools, tool_choice: Auto }
  │   ├─ 发送 ProgressEvent::Thinking
  │   ├─ 调用 LLM (retry_llm_call, 最多 3 次)
  │   │
  │   ├─ LLM 响应:
  │   │   ├─ finish_reason = ToolUse → 执行工具，追加结果，继续循环
  │   │   └─ finish_reason = EndTurn → 提取文本，退出循环
  │   │
  │   └─ 发送 ProgressEvent::Done
  │
  ├─ 输出结果
  │
  └─ 触发评估脑审核
      ├─ 通过 → 返回给用户
      └─ 不通过 → 插入评估反馈到 messages，重新进入 tool_loop
```

### 3.2 Messages 构造

对齐 Claude Code 的全量历史模式：

```
messages = [
    ChatMessage::system(SYSTEM_PROMPT),           // 固定前缀，KV Cache 友好
    ChatMessage::system(TOOL_DEFINITIONS),        // 固定前缀
    ChatMessage::user("今天天气怎么样"),            // 历史
    ChatMessage::assistant(tool_use + text),      // 历史
    ChatMessage::tool_result(...),                 // 历史
    ChatMessage::assistant("宁波22°C晴..."),       // 历史
    ChatMessage::user("继续开发压缩功能"),          // 当前输入 ← 新增
]
```

**不使用** `[system] + [context] + [step_goal]` 的重建模式。
直接传入完整的对话历史，LLM 通过历史理解上下文。

### 3.3 上下文重建（记忆脑触发时）

```
记忆脑判断需要重建
  │
  ├─ 生成 brain_state（总结 + 映射索引）
  │
  └─ 替换主脑的 messages:
      messages = [
          ChatMessage::system(SYSTEM_PROMPT),
          ChatMessage::system(TOOL_DEFINITIONS),
          ChatMessage::system(format!("## 之前的工作上下文\n{brain_state}")),
          // 最近 N 轮保留
          recent_turn_1,
          recent_turn_2,
          ...
      ]
```

重建后新的前缀 `[SYSTEM_PROMPT][TOOL_DEFS][BRAIN_STATE]` 会被 KV Cache 缓存，
后续请求继续命中缓存。

## 4. 工具调用流程

```
LLM 返回 finish_reason = ToolUse
  │
  ├─ 提取 ContentBlock::ToolUse { id, name, input }
  │
  ├─ 发送 ProgressEvent::ToolStart { tool_name, input_preview }
  │
  ├─ 安全检查 (guard_check)
  │   ├─ 危险操作 → 标记为需要确认
  │   └─ 安全 → 执行
  │
  ├─ 执行工具: executor.execute(tool_call)
  │   ├─ 成功 → ToolExecutionResult { output, duration_ms }
  │   └─ 失败 → ToolExecutionResult { output: error_msg, is_error: true }
  │
  ├─ 发送 ProgressEvent::ToolDone { tool_name, duration_ms, output_preview, is_error }
  │
  ├─ 追加到 messages:
  │   messages.push(ChatMessage::tool_result(id, output, is_error))
  │
  └─ 继续循环（LLM 看到工具结果后决定下一步）
```

### 4.1 工具调用参数展示

ToolStart 时截取 input 的摘要（前 80 字符）用于展示：
```
推理脑 ✔ WebSearch("宁波 天气") (519ms)
推理脑 ✔ read_file("src/main.rs") (45ms)
推理脑 ✔ bash("cargo test") (3.2s)
推理脑 ✘ WebFetch("xxx.com") (2.1s) 超时
```

## 5. System Prompt 设计

### 5.1 主脑系统提示词

```markdown
# Identity
你是一个通用任务执行引擎。你使用可用工具完成各种任务。

# Rules
- 先理解再行动。先读取现有内容、代码或文档。
- 每次只做一件事。
- 修改后验证：编译、测试、检查结果。
- 如果方法失败，先诊断根因再换策略。
- 永远不要猜测。先读取、搜索、检查，再创建或修改。

# Available Tools
{tool_definitions}

# Context
以下是你之前的工作上下文（由记忆系统提供）：
{brain_state}

# Memory Context
以下记忆系统注入的相关信息：
{memory_injection}

# Output
完成任务时，输出清晰的总结。
如果无法完成，解释原因并建议替代方案。
```

## 6. 重试策略

```rust
const MAX_LLM_RETRIES: u32 = 3;
const RETRY_INTERVAL_SECS: u64 = 10; // 改为 10 秒（原来 60 秒太长）

async fn retry_llm_call() {
    for attempt in 1..=MAX_LLM_RETRIES {
        match provider.complete(request).await {
            Ok(response) => return Ok(response),
            Err(e) => {
                发送 LlmRetry 事件
                if attempt < MAX_LLM_RETRIES {
                    tokio::time::sleep(10s).await
                }
            }
        }
    }
}
```

## 7. Token 估算

```rust
fn estimate_tokens(messages: &[ConversationMessage]) -> usize {
    messages.iter()
        .flat_map(|m| m.blocks.iter())
        .map(|b| match b {
            ContentBlock::Text { text } => text.len() / 4,
            ContentBlock::ToolUse { name, input, .. } =>
                (name.len() + input.to_string().len()) / 4,
            ContentBlock::ToolResult { content, .. } => content.len() / 4,
        })
        .sum()
}
```
