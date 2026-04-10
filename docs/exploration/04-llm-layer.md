# LLM 调用层探索报告

> 探索日期: 2026-04-09

## 一、两套独立的 LLM 体系

| 维度 | api 层 (Claude Code) | brain-llm 层 (AI Brain) |
|------|---------------------|------------------------|
| trait | `Provider` (stream + non-stream) | `LlmProvider` (仅 complete) |
| 消息模型 | `Vec<InputContentBlock>` 多态枚举 | `ChatMessage { role, content: String }` 纯文本 |
| tool_use | ✅ 完整支持 | ❌ 完全没有 |
| streaming | ✅ 完整支持 | ❌ 完全没有 |
| System prompt | 独立字段 `MessageRequest.system` | messages 数组中 system 角色 |
| Provider 覆盖 | Anthropic / OpenAI / xAI | 仅 OpenAI 兼容（智谱 GLM） |
| 重试/错误处理 | 指数退避 + 错误分类 | 简单 HTTP status |
| Prompt Cache | 服务端自动 + 本地 completion cache | 无 |

---

## 二、api 层详细结构

### 2.1 核心类型 (`api/types.rs`)

```rust
struct MessageRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<InputMessage>,
    system: Option<String>,              // 独立 system prompt
    tools: Option<Vec<ToolDefinition>>,  // 工具定义
    tool_choice: Option<ToolChoice>,     // Auto / Any / Tool{name}
    stream: bool,
}

enum InputContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: Vec<ToolResultContentBlock>, is_error: bool },
}

enum OutputContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    Thinking { thinking: String, signature: Option<String> },
    RedactedThinking { data: Value },
}
```

### 2.2 Provider trait (`api/providers/mod.rs`)

```rust
trait Provider {
    type Stream;
    fn send_message(&self, request: &MessageRequest) -> MessageResponse;
    fn stream_message(&self, request: &MessageRequest) -> Self::Stream;
}
```

### 2.3 OpenAI 兼容翻译 (`api/providers/openai_compat.rs`)

- Anthropic ↔ OpenAI 格式**双向翻译**
- `translate_message()`: ToolUse → tool_calls, ToolResult → role:"tool"
- `normalize_response()`: tool_calls → OutputContentBlock::ToolUse
- 流式: delta.tool_calls 增量拼接为 StreamEvent

### 2.4 tool_use 循环

api 层本身**不含循环编排**。循环在上层调用方（conversation.rs 的 run_turn）。api 层提供的是类型支持和格式翻译。

---

## 三、brain-llm 层详细结构

### 3.1 核心类型 (`brain-llm/provider.rs`)

```rust
struct ChatMessage {
    role: MessageRole,  // System / User / Assistant
    content: String,    // 纯文本
}

struct ChatRequest {
    model: Option<String>,
    messages: Vec<ChatMessage>,
    max_tokens: Option<u32>,
    temperature: Option<f64>,
    stream: Option<bool>,  // 存在但从未使用
}

struct ChatResponse {
    content: String,  // 纯文本
    model: String,
    usage: TokenUsage,
    finish_reason: Option<String>,
}
```

### 3.2 LlmProvider trait

```rust
trait LlmProvider: Send + Sync {
    fn model(&self) -> &str;
    fn complete(&self, request: ChatRequest) -> Future<Result<ChatResponse>>;
}
```

### 3.3 OpenAiCompatClient (`brain-llm/openai_compat.rs`)

- `ApiMessage { role: String, content: String }` — 无 tool_calls
- `ApiChatRequest` — 无 tools / tool_choice 字段
- `ApiChatResponse` — 只取 text，不解析 tool_calls
- stream 硬编码 `Some(false)`
- **无 streaming 实现**

---

## 四、副脑实际使用情况

### 4.1 感知脑（brain-sensory）

定义了**自己的第三个 LlmProvider trait**（与 brain-llm 的和 api 层的都不同）：

```rust
// brain-sensory/src/llm.rs
fn complete(&self, model: &str, system_prompt: &str, user_input: &str, max_tokens: u32)
    -> Future<Result<String, String>>
```

最简化接口：字符串进，字符串出。

### 4.2 推理脑（brain-reasoning）

使用 brain-llm 的 `LlmProvider`：

```rust
// 每次调用全新构建 messages，无历史累积
messages: vec![
    ChatMessage::system(REASONING_SYSTEM_PROMPT),
    ChatMessage::user(&prompt),
],
```

### 4.3 记忆脑（brain-memory）

**不依赖 brain-llm**，LLM 调用需求尚未实现。

---

## 五、差距汇总

### 5.1 brain-llm 缺失的关键能力

| 能力 | api 层 | brain-llm | 差距程度 |
|------|--------|-----------|---------|
| ToolDefinition 类型 | ✅ | ❌ | 需新增 |
| ToolChoice 类型 | ✅ | ❌ | 需新增 |
| InputContentBlock::ToolUse | ✅ | ❌ | 需新增 |
| InputContentBlock::ToolResult | ✅ | ❌ | 需新增 |
| OutputContentBlock::ToolUse | ✅ | ❌ | 需新增 |
| OpenAI tool_calls 请求翻译 | ✅ | ❌ | 需新增 |
| OpenAI tool_calls 响应解析 | ✅ | ❌ | 需新增 |
| Streaming | ✅ | ❌ | 需新增 |
| 流式 tool_use 增量拼接 | ✅ | ❌ | 需新增 |
| 重试机制 | ✅ | ❌ | 需新增 |

### 5.2 trait 统一建议

**不统一**，但 brain-llm 需要升级：

1. `ChatMessage.content` 从 `String` 升级为 `Vec<ContentBlock>`
2. `ChatRequest` 增加 `tools` 和 `tool_choice` 字段
3. `LlmProvider` 增加 `stream_complete()` 方法
4. 推理脑内部自己管理 tool_use 循环编排

### 5.3 感知脑 trait 债务

感知脑定义了第三个 LlmProvider trait，应该统一到 brain-llm 的 trait 上。感知脑的需求（文本进文本出）完全可以由 `LlmProvider::complete()` 满足。
