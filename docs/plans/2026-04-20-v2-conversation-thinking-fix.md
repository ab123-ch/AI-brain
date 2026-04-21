# V2 修复计划 — 对话事务性 + Thinking 统一处理

> 日期: 2026-04-20
> 参考: Claude Code 源码分析（Issue #21849, #17953, #5683 + Anthropic API 文档）

---

## 问题清单

### P0-1: LLM 调用失败后上下文断裂

**现状**: `process_input()` 在调用 tool_loop 之前就 `push_user(input)`。如果 LLM 调用失败（网络错误/API 500），用户消息已写入历史但没有配对的 assistant 回复。下一轮对话历史变成：

```
[User: "第一次问题"]     ← 孤儿消息，没有 assistant 配对
[User: "第二次问题"]
[Assistant: "xxx"]
```

LLM 看到的上下文就乱了。

**Claude Code 做法**: 事务式写入 — 成功返回后才把完整一轮（user + assistant + tool 中间消息）落盘到 JSONL。失败时历史不变，重试时带着同一份上下文重新请求。

**影响文件**:
- `rust/crates/brain-main/src/main_brain.rs` — process_input
- `rust/crates/brain-main/src/conversation.rs` — ConversationHistory（可能需要 pop_last_user）

### P0-2: Thinking 内容在文本层正则删除，每个调用点都要手动 strip

**现状**: `strip_thinking()` 函数在两个文件中各有一份（main_brain.rs + consolidation.rs），用字符串查找删除 `<thinking>...</thinking>` 标签。每个消费 LLM 响应的地方都要记得调用。

问题：
1. 两份实现不一致（一个匹配 `<thinking>`，另一个匹配 `<think`）
2. 新模型（QwQ 用 `<think/>`，DeepSeek 用 `<think。\n`）都要改
3. 本质上是在数据层做了渲染层的活

**Claude Code 做法**: 协议级区分 — API 的 content block 有明确的 `type` 字段（`thinking` / `text` / `tool_use`），渲染层只渲染 `text` 和 `tool_use`，不渲染 `thinking`。不存在 `strip_thinking` 这种函数。

**影响文件**:
- `rust/crates/brain-llm/src/types.rs` — ContentBlock 枚举
- `rust/crates/brain-llm/src/provider.rs` — ChatResponse::text() / ChatMessage::text_content()
- `rust/crates/brain-llm/src/openai_compat.rs` — parse_api_message 解析 thinking
- `rust/crates/brain-llm/src/stream.rs` — SSE 事件中识别 thinking
- `rust/crates/brain-main/src/main_brain.rs` — 删除 strip_thinking
- `rust/crates/brain-memory/src/consolidation.rs` — 删除第二份 strip_thinking

---

## 修复方案

### Fix 1: 对话事务性写入 (P0-1)

**思路**: process_input 不再先 push_user，而是临时暂存用户输入，等 tool_loop 成功后一次性写入完整一轮。

**Step 1.1**: 修改 `process_input` 签名和流程

```rust
// main_brain.rs — process_input
pub async fn process_input(&mut self, input: &str, ...) -> Result<MainBrainOutput> {
    // 不再: self.history.push_user(input);

    // 1. 构建 messages 时临时追加用户消息
    let mut messages = self.build_messages_with_user(input);

    // 2. 跑 tool_loop
    let loop_result = tool_loop::run_tool_loop_with_config(...).await?;  // 失败直接返回，历史不变

    // 3. 成功后一次性写入完整一轮
    self.history.push_user(input);        // 用户消息
    // ... 追加 tool_loop 中新增的 assistant + tool 消息
    self.history.push_assistant(&answer); // 最终回答
}
```

**Step 1.2**: `build_messages_with_user` — 临时追加用户消息到 messages 末尾，但不写入 history

```rust
fn build_messages_with_user(&self, user_input: &str) -> Vec<ChatMessage> {
    let mut messages = self.build_messages();
    messages.push(ChatMessage::user(user_input));
    messages
}
```

**Step 1.3**: 确保 streaming 路径 (`process_input_streaming`) 也做同样处理

### Fix 2: ContentBlock Thinking 统一处理 (P0-2)

**思路**: 在协议层增加 Thinking block 类型，Provider 层统一解析，`text()` 方法自动跳过，删除所有 `strip_thinking`。

**Step 2.1**: ContentBlock 新增 Thinking 变体

```rust
// types.rs
pub enum ContentBlock {
    Text { text: String },
    Thinking { content: String },  // 新增：推理过程
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}
```

新增辅助方法：
- `ContentBlock::thinking(content)` — 构造函数
- `ContentBlock::is_thinking()` — 判断
- `ContentBlock::as_thinking()` — 提取内容

**Step 2.2**: 修改 `as_text()` 和 `text()` — 自动跳过 Thinking

`as_text()` 不变（只返回 Text block）。`text()` 和 `text_content()` 本来就只 filter_map `as_text()`，所以 Thinking block 自然被跳过。不需要任何额外过滤。

**Step 2.3**: OpenAI 兼容层统一解析 thinking

```rust
// openai_compat.rs — parse_api_message
fn parse_api_message(msg: &ApiMessage) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    if let Some(content) = &msg.content {
        match content {
            serde_json::Value::String(s) => {
                // 统一提取 thinking 标签
                let (thinking, text) = extract_thinking_and_text(s);
                if let Some(t) = thinking {
                    blocks.push(ContentBlock::thinking(t));
                }
                if !text.is_empty() {
                    blocks.push(ContentBlock::text(text));
                }
            }
            serde_json::Value::Array(parts) => {
                // 检查 reasoning_content 字段（部分 API 支持）
                // 否则从文本中提取
            }
            _ => {}
        }
    }
    // ... tool_calls 处理不变
}
```

新增通用提取函数 `extract_thinking_and_text`:
```rust
/// 统一提取 thinking 内容和实际文本
/// 支持格式: <thinking>..</thinking>, <think ..>..</think >
/// 返回 (thinking_content, remaining_text)
fn extract_thinking_and_text(text: &str) -> (Option<String>, String) {
    // 用正则一次性匹配所有 thinking 标签变体
    // 匹配 <think[ing]?[^>]*>...</think[ing]?> (含各种闭合变体)
    let re = regex::Regex::new(r"(?s)<think(?:ing)?[^>]*>(.*?)</think(?:ing)?>").unwrap();
    let mut thinking_parts = Vec::new();
    let mut remaining = text.to_string();

    // 循环提取所有 thinking 块
    loop {
        if let Some(caps) = re.captures(&remaining) {
            thinking_parts.push(caps[1].trim().to_string());
            remaining = remaining.replace(&caps[0], "");
        } else {
            break;
        }
    }

    let thinking = if thinking_parts.is_empty() {
        None
    } else {
        Some(thinking_parts.join("\n"))
    };

    (thinking, remaining.trim().to_string())
}
```

**Step 2.4**: SSE streaming 也识别 thinking delta

```rust
// stream.rs — process_chunk 增加对 reasoning_content 的处理
// 部分 OpenAI 兼容 API 在 delta 中返回 reasoning_content 字段
if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
    if !reasoning.is_empty() {
        events.push(StreamEvent::ThinkingDelta { content: reasoning.into() });
    }
}
```

StreamEvent 新增变体：
```rust
pub enum StreamEvent {
    TextDelta { text: String },
    ThinkingDelta { content: String },  // 新增
    ToolCallStart { ... },
    ToolCallDelta { ... },
    Done { ... },
}
```

**Step 2.5**: 删除所有 `strip_thinking` 调用

- `main_brain.rs:118` — `strip_thinking(&loop_result.response.text())` → 直接 `.text()`（已不含 thinking）
- `main_brain.rs:222` — 同上
- `main_brain.rs:332-357` — 删除整个 `strip_thinking` 函数
- `consolidation.rs:292` — `strip_thinking(&raw_text)` → 直接用 `raw_text`
- `consolidation.rs:716` — `strip_thinking(text)` → 直接用 `text`
- `consolidation.rs:748-780` — 删除整个 `strip_thinking` 函数

**Step 2.6**: `message_to_api` 处理 Thinking block

```rust
// openai_compat.rs — message_to_api
// Thinking block 在转 API 格式时直接忽略（不发送给 API）
// 因为 thinking 是我们内部的概念，API 不需要回传
ContentBlock::Thinking { .. } => {
    // 序列化时跳过，不发送给 API
}
```

---

## 修改文件清单

| 文件 | 修改内容 |
|------|---------|
| `brain-llm/src/types.rs` | ContentBlock 加 Thinking 变体 + 辅助方法 + StreamEvent 加 ThinkingDelta |
| `brain-llm/src/provider.rs` | 无需改动（text() 已基于 as_text() 过滤） |
| `brain-llm/src/openai_compat.rs` | parse_api_message 加 thinking 提取；message_to_api 跳过 Thinking |
| `brain-llm/src/stream.rs` | process_chunk 识别 reasoning_content 字段 |
| `brain-main/src/main_brain.rs` | process_input 改事务式写入；删除 strip_thinking |
| `brain-main/src/conversation.rs` | 可能需要新增临时构建方法 |
| `brain-memory/src/consolidation.rs` | 删除 strip_thinking 调用和函数 |

## 验证

1. `cargo test --workspace` — 确保现有 217 个测试通过
2. 新增测试：
   - ContentBlock::Thinking 序列化/反序列化
   - extract_thinking_and_text 各种标签变体
   - process_input 失败时历史不变
   - SSE streaming 识别 reasoning_content
3. `cargo clippy --workspace --all-targets -- -D warnings`
