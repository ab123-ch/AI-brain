# ConversationMessage 升级实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将 ConversationMessage.content 从 String 升级为 Vec<ContentBlock>，修复第二轮会话丢失工具调用信息 + Ctrl+C 中断抢救。

**Architecture:** ContentBlock 从 brain-llm 下沉到 brain-core，ConversationHistory 直接存储结构化的 ToolUse/ToolResult，to_chat_messages() 精确还原。Ctrl+C 从 abort() 改为协作取消，tool_loop 返回已执行结果后统一写回。

**Tech Stack:** Rust, tokio, serde, chrono

---

### Task 1: ContentBlock 下沉到 brain-core

**Files:**
- Modify: `rust/crates/brain-core/src/types.rs` (新增 ContentBlock 定义)
- Modify: `rust/crates/brain-core/Cargo.toml` (确保 serde 依赖)
- Modify: `rust/crates/brain-llm/src/types.rs` (删除定义，改为重导出)
- Test: `rust/crates/brain-core/src/types.rs` (迁移测试)

**Step 1: 在 brain-core/src/types.rs 头部新增 ContentBlock 定义**

在 `use serde::{Deserialize, Serialize};` 之后，`BrainId` 之前，添加从 brain-llm/types.rs 复制的 ContentBlock 枚举及其 impl 块（text/as_text/is_tool_use/is_tool_result/thinking/is_thinking/as_thinking）。添加 `pub use self::content_block::ContentBlock;` 或直接内联。

确保字段完全一致：Text/Thinking/ToolUse{id,name,input}/ToolResult{tool_use_id,content,is_error}。

**Step 2: 修改 brain-llm/src/types.rs**

删除 ContentBlock 枚举定义和 impl ContentBlock 块。
在文件头部添加：`pub use brain_core::types::ContentBlock;`
保留 ToolDefinition/ToolChoice/FinishReason/StreamEvent/TokenUsage 及其测试。
更新 `use crate::types::ContentBlock` 为 `use brain_core::types::ContentBlock`（如果 provider.rs 中有引用）。

**Step 3: 迁移 ContentBlock 测试到 brain-core**

将 brain-llm types.rs 中的 content_block_*_roundtrip 测试移到 brain-core types.rs 的 tests 模块。

**Step 4: 运行 cargo check 验证编译**

Run: `cd rust && cargo check --workspace 2>&1 | head -50`
Expected: 编译通过（或只有本 crate 的预期错误）

**Step 5: Commit**

```bash
git add rust/crates/brain-core/src/types.rs rust/crates/brain-core/Cargo.toml rust/crates/brain-llm/src/types.rs
git commit -m "refactor: 将 ContentBlock 从 brain-llm 下沉到 brain-core"
```

---

### Task 2: ConversationMessage.content 升级为 Vec<ContentBlock>

**Files:**
- Modify: `rust/crates/brain-core/src/types.rs` (ConversationMessage 定义 + serde 兼容)
- Test: `rust/crates/brain-core/src/types.rs`

**Step 1: 写失败测试 — ConversationMessage 存储 ToolUse**

在 brain-core types.rs 的 tests 模块中：

```rust
#[test]
fn conversation_message_stores_tool_use() {
    let msg = ConversationMessage::assistant_blocks(vec![
        ContentBlock::text("我来搜索"),
        ContentBlock::ToolUse {
            id: "toolu_01".into(),
            name: "WebFetch".into(),
            input: serde_json::json!({"url": "https://example.com"}),
        },
    ]);
    assert_eq!(msg.role, MessageRole::Assistant);
    assert_eq!(msg.content.len(), 2);
    assert!(msg.content[1].is_tool_use());
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-core conversation_message_stores_tool_use -- --nocapture`
Expected: FAIL（assistant_blocks 方法不存在）

**Step 3: 升级 ConversationMessage 定义**

修改 `brain-core/src/types.rs` 中的 ConversationMessage：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: MessageRole,
    #[serde(
        serialize_with = "serialize_content_blocks",
        deserialize_with = "deserialize_content_blocks",
        // 默认用 default 让旧格式兼容
        default,
    )]
    pub content: Vec<ContentBlock>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
```

添加 serde 兼容函数：

```rust
fn serialize_content_blocks<S: serde::Serializer>(
    blocks: &Vec<ContentBlock>,
    s: S,
) -> Result<S::Ok, S::Error> {
    // 如果只有一个 Text 块，序列化为字符串（向后兼容）
    if blocks.len() == 1 {
        if let ContentBlock::Text { text } = &blocks[0] {
            return s.serialize_str(text);
        }
    }
    blocks.serialize(s)
}

fn deserialize_content_blocks<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<ContentBlock>, D::Error> {
    use serde::de;

    // 尝试先作为字符串反序列化（旧格式兼容）
    struct ContentVisitor;

    impl<'de> de::Visitor<'de> for ContentVisitor {
        type Value = Vec<ContentBlock>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("string or array of content blocks")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(vec![ContentBlock::text(v)])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            Vec::<ContentBlock>::deserialize(de::value::SeqAccessDeserializer::new(seq))
        }
    }

    d.deserialize_any(ContentVisitor)
}
```

更新构造方法：

```rust
impl ConversationMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: MessageRole::User, content: vec![ContentBlock::text(content)], timestamp: chrono::Utc::now() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: MessageRole::Assistant, content: vec![ContentBlock::text(content)], timestamp: chrono::Utc::now() }
    }
    pub fn assistant_blocks(blocks: Vec<ContentBlock>) -> Self {
        Self { role: MessageRole::Assistant, content: blocks, timestamp: chrono::Utc::now() }
    }
    pub fn tool_result(tool_use_id: String, content: String, is_error: bool) -> Self {
        Self { role: MessageRole::Tool, content: vec![ContentBlock::ToolResult { tool_use_id, content, is_error }], timestamp: chrono::Utc::now() }
    }
    pub fn tool(content: impl Into<String>) -> Self {
        Self { role: MessageRole::Tool, content: vec![ContentBlock::text(content)], timestamp: chrono::Utc::now() }
    }
    pub fn evaluator(content: impl Into<String>) -> Self {
        Self { role: MessageRole::Evaluator, content: vec![ContentBlock::text(content)], timestamp: chrono::Utc::now() }
    }
    /// 提取文本内容（仅 Text 块）
    pub fn text_content(&self) -> String {
        self.content.iter().filter_map(|b| b.as_text()).collect::<Vec<_>>().join("")
    }
}
```

**Step 4: 添加 serde 兼容测试**

```rust
#[test]
fn conversation_message_backward_compat_string() {
    // 旧格式 JSON: content 是字符串
    let json = r#"{"role":"user","content":"hello","timestamp":"2026-01-01T00:00:00Z"}"#;
    let msg: ConversationMessage = serde_json::from_str(json).unwrap();
    assert_eq!(msg.content.len(), 1);
    assert_eq!(msg.text_content(), "hello");
}

#[test]
fn conversation_message_new_format_roundtrip() {
    let msg = ConversationMessage::assistant_blocks(vec![
        ContentBlock::text("搜索中"),
        ContentBlock::ToolUse { id: "t1".into(), name: "bash".into(), input: serde_json::json!({"cmd":"ls"}) },
    ]);
    let json = serde_json::to_string(&msg).unwrap();
    let de: ConversationMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(de.content.len(), 2);
    assert!(de.content[1].is_tool_use());
}
```

**Step 5: 运行测试**

Run: `cd rust && cargo test -p brain-core -- --nocapture`
Expected: ALL PASS

**Step 6: Commit**

```bash
git add rust/crates/brain-core/src/types.rs
git commit -m "feat: ConversationMessage.content 升级为 Vec<ContentBlock> + serde 向后兼容"
```

---

### Task 3: ConversationHistory 适配新结构

**Files:**
- Modify: `rust/crates/brain-main/src/conversation.rs`
- Test: `rust/crates/brain-main/src/conversation.rs`

**Step 1: 写失败测试 — to_chat_messages 还原 ToolUse**

```rust
#[test]
fn to_chat_messages_preserves_tool_use() {
    let mut history = ConversationHistory::new(100_000);
    history.push_user("搜索 firecrawl");
    history.push_assistant_blocks(vec![
        brain_core::types::ContentBlock::text("我来搜索"),
        brain_core::types::ContentBlock::ToolUse {
            id: "toolu_01".into(),
            name: "WebFetch".into(),
            input: serde_json::json!({"url": "https://github.com/firecrawl"}),
        },
    ]);
    history.push_tool_result("toolu_01".into(), "firecrawl 数据".into(), false);
    history.push_assistant("搜索结果如下...");

    let chat = history.to_chat_messages();
    // msg[0]=system 不在 history 中，所以 chat 从 user 开始
    assert_eq!(chat[0].role, LlmRole::User);       // "搜索 firecrawl"
    assert_eq!(chat[1].role, LlmRole::Assistant);   // 含 ToolUse
    assert!(chat[1].content.iter().any(|b| b.is_tool_use()));
    assert_eq!(chat[2].role, LlmRole::User);         // ToolResult → User
    assert!(chat[2].content.iter().any(|b| b.is_tool_result()));
    assert_eq!(chat[3].role, LlmRole::Assistant);    // "搜索结果如下..."
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-main to_chat_messages_preserves_tool_use -- --nocapture`
Expected: FAIL（push_assistant_blocks 不存在）

**Step 3: 修改 ConversationHistory**

修改 `conversation.rs`：

- `push_assistant(text)`: 内部转 `vec![ContentBlock::text(text)]`
- 新增 `push_assistant_blocks(blocks: Vec<brain_core::types::ContentBlock>)`
- `push_tool_result(tool_use_id: String, content: String, is_error: bool)`: 内部创建 ToolResult 块
- `push_user(text)`: 不变
- `push_evaluator(text)`: 不变
- `push_system(text)`: 不变

`estimate_tokens` 改为遍历 `Vec<ContentBlock>` 中 Text 块的字符数估算。

`to_chat_messages()` 改为精确还原：
- Tool 消息中如果有 ToolResult 块，直接 clone（role 映射为 User）
- Assistant 消息中如果有 ToolUse 块，直接 clone（role 映射为 Assistant）
- System/Evaluator 映射为 User，但保留原始 ContentBlock

**Step 4: 运行测试**

Run: `cd rust && cargo test -p brain-main -- --nocapture`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add rust/crates/brain-main/src/conversation.rs
git commit -m "feat: ConversationHistory 支持结构化 ToolUse/ToolResult 存储"
```

---

### Task 4: main_brain.rs 写回逻辑升级

**Files:**
- Modify: `rust/crates/brain-main/src/main_brain.rs`

**Step 1: 写失败测试 — process_input 写回 ToolUse 到历史**

```rust
#[tokio::test]
async fn process_input_writes_tool_use_to_history() {
    // 构造一个返回 ToolUse 然后 EndTurn 的 stub LLM
    // 验证 history 中有 ToolUse 和 ToolResult
}
```

**Step 2: 运行测试确认失败**

**Step 3: 修改 process_input 写回逻辑**

将 main_brain.rs:176-206（失败路径）和 :219-243（成功路径）的两处写回逻辑统一为：

```rust
for msg in messages.iter().skip(skip_count) {
    if msg.role == brain_llm::MessageRole::System { continue; }
    match msg.role {
        brain_llm::MessageRole::Assistant => {
            let has_tool_use = msg.content.iter().any(|b| b.is_tool_use());
            if has_tool_use {
                self.history.push_assistant_blocks(msg.content.clone());
            } else {
                let text = msg.text_content();
                if !text.is_empty() {
                    self.history.push_assistant(&text);
                }
            }
        }
        brain_llm::MessageRole::User => {
            for block in &msg.content {
                if let brain_llm::ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                    self.history.push_tool_result(
                        tool_use_id.clone(),
                        content.clone(),
                        *is_error,
                    );
                }
            }
        }
        _ => {}
    }
}
```

同样修改 process_input_streaming 中的类似逻辑。

**Step 4: 运行所有测试**

Run: `cd rust && cargo test -p brain-main -- --nocapture`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add rust/crates/brain-main/src/main_brain.rs
git commit -m "fix: main_brain 写回逻辑保留完整 ToolUse/ToolResult 结构"
```

---

### Task 5: compact 模块适配

**Files:**
- Modify: `rust/crates/brain-main/src/compact.rs`

**Step 1: 修改 estimate_tokens**

从 `m.content.chars().count()` 改为遍历 ContentBlock 中 Text 块的字符数。

**Step 2: 修改 compress_single_turn**

提取 ToolResult 内容时从 `m.content.as_str()` 改为遍历 ContentBlock 找 ToolResult 块的 content 字段。

**Step 3: 修改 apply_pending**

替换 Tool 消息内容时，找到 ToolResult 块并替换其 content 字段。

**Step 4: 修改 split_turn_blocks**

`msg.content.chars().count()` 改为统计 Text 块字符数。

**Step 5: 运行测试**

Run: `cd rust && cargo test -p brain-main -- --nocapture`
Expected: ALL PASS

**Step 6: Commit**

```bash
git add rust/crates/brain-main/src/compact.rs
git commit -m "refactor: compact 模块适配 Vec<ContentBlock> 结构"
```

---

### Task 6: Ctrl+C 协作取消 + tool_loop 抢救

**Files:**
- Modify: `rust/crates/brain-main/src/tool_loop.rs` (添加取消参数)
- Modify: `rust/crates/brain-main/src/main_brain.rs` (传递取消令牌)
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs` (cancel_query 改为协作取消)
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs` (传递 CancellationToken)

**Step 1: tool_loop 添加取消检测**

修改 `run_tool_loop_with_config` 签名新增 `cancel: Option<tokio_util::sync::CancellationToken>`。

在 loop 开头添加：
```rust
if let Some(ref cancel) = cancel {
    if cancel.is_cancelled() {
        tracing::info!("tool_loop 收到取消信号，返回已执行结果");
        break;
    }
}
```

修改 `run_tool_loop` 也透传 cancel 参数。

**Step 2: main_brain.rs 传递 CancellationToken**

process_input 新增 `cancel: Option<CancellationToken>` 参数，透传给 tool_loop。

process_input_streaming 同理。

**Step 3: orchestrator.rs 创建和传递 CancellationToken**

query_streaming 中创建 `CancellationToken`，传给 `brain.process_input`。

在 query_handle 的 tokio::spawn 中持有 cancel clone。暴露 cancel 给外部。

**Step 4: tui/app.rs cancel_query 改为协作取消**

App 新增 `cancel_token: Option<CancellationToken>`。

start_query 中保存 CancellationToken。

cancel_query 改为：
```rust
fn cancel_query(&mut self) {
    if let Some(cancel) = self.cancel_token.take() {
        cancel.cancel();  // 通知 tool_loop 停止
    }
    // 不 abort，等待 query_handle 自然退出
    // 但如果 2s 内没退出，则 abort
    if let Some(handle) = self.query_handle.take() {
        // 标记为正在取消，在主循环中检测完成
        self.cancelling_handle = Some(handle);
    }
    ...
}
```

在主循环 tick 中检测 cancelling_handle 是否完成，完成后执行写回。

**Step 5: 运行测试**

Run: `cd rust && cargo test --workspace --exclude brain-integration-tests -- --nocapture`
Expected: ALL PASS

**Step 6: Commit**

```bash
git add rust/crates/brain-main/src/tool_loop.rs rust/crates/brain-main/src/main_brain.rs rust/crates/ai-brain-cli/src/tui/app.rs rust/crates/ai-brain-cli/src/orchestrator.rs
git commit -m "fix: Ctrl+C 改为协作取消，tool_loop 中断时抢救写入已执行结果"
```

---

### Task 7: 全量测试 + clippy

**Step 1: 运行全量测试**

Run: `cd rust && cargo test --workspace --exclude brain-integration-tests`
Expected: ALL PASS

**Step 2: 运行 clippy**

Run: `cd rust && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无 warning

**Step 3: 运行 fmt**

Run: `cd rust && cargo fmt --check`
Expected: 无差异

**Step 4: 最终 commit**

```bash
git add -A
git commit -m "chore: ConversationMessage 升级完成，全量测试通过"
```
