# ConversationMessage 升级设计：ContentBlock 模型

> 日期：2026-05-21
> 状态：已确认
> 优先级：P0（修复第二轮会话丢失工具调用信息的架构缺陷）

## 问题概述

第二轮会话看不到第一轮的工具调用信息，根因是 `ConversationMessage` 只有 `role + content: String`，无法承载结构化的 ToolUse/ToolResult 数据。

### 断裂链

```
tool_loop 中 ChatMessage { Vec<ContentBlock> }
    ↓ 写回历史时 ToolUse 块被丢弃，ToolResult 只保存文本
ConversationMessage { role, content: String }
    ↓ to_chat_messages() 全部转为 Text 块
ChatMessage { role, [Text] } ← LLM 看不到工具调用上下文
```

加上 Ctrl+C abort 时直接杀 task，历史中只有 user 消息。

## 方案：ContentBlock 下沉到 brain-core

将 `brain_llm::ContentBlock` 移至 `brain_core::types`，`ConversationMessage.content` 从 `String` 升级为 `Vec<ContentBlock>`。

### 为什么选这个方案

- ContentBlock 是纯数据结构（Text/Thinking/ToolUse/ToolResult），没有 LLM 专属逻辑，放在 brain-core 完全合理
- MessageRole 已在 brain-core 定义，ContentBlock 天然同层
- 单一数据源，没有转换/同步成本

## 设计详情

### 1. ContentBlock 下沉

**brain-core/src/types.rs** 新增（从 brain-llm 移入）：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { content: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: serde_json::Value },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}
```

**brain-llm/src/types.rs** 改为重导出：

```rust
pub use brain_core::types::ContentBlock;
// 删除原定义，保留 impl 方法
```

### 2. ConversationMessage 升级

```rust
// brain-core/src/types.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: MessageRole,
    #[serde(
        serialize_with = "serialize_content",
        deserialize_with = "deserialize_content",
    )]
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
}
```

**Serde 兼容**：旧 JSON 中 `"content": "字符串"` 自动转为 `vec![Text("字符串")]`。

### 3. ConversationHistory 新方法

```rust
impl ConversationHistory {
    // 简化版（向后兼容）
    pub fn push_user(&mut self, text: impl Into<String>) { ... }
    pub fn push_assistant(&mut self, text: impl Into<String>) { ... }

    // 新增结构化写入
    pub fn push_assistant_blocks(&mut self, blocks: Vec<ContentBlock>) { ... }
    pub fn push_tool_result(&mut self, tool_use_id: String, content: String, is_error: bool) { ... }

    // 保留
    pub fn push_evaluator(&mut self, text: impl Into<String>) { ... }
    pub fn push_system(&mut self, text: impl Into<String>) { ... }
}
```

### 4. to_chat_messages 精确还原

```rust
pub fn to_chat_messages(&self) -> Vec<ChatMessage> {
    self.messages.iter().map(|m| {
        let role = match m.role {
            MessageRole::Assistant => LlmRole::Assistant,
            MessageRole::Tool => LlmRole::User,  // ToolResult 在 API 中是 User 角色
            MessageRole::System | MessageRole::Evaluator | MessageRole::User => LlmRole::User,
        };
        ChatMessage {
            role,
            content: m.content.clone(),  // 直接 clone Vec<ContentBlock>
        }
    }).collect()
}
```

### 5. main_brain.rs 写回逻辑

成功路径和错误路径统一改为：

```rust
for msg in messages.iter().skip(skip_count) {
    match msg.role {
        LlmRole::Assistant => {
            if msg.content.iter().any(|b| b.is_tool_use()) {
                self.history.push_assistant_blocks(msg.content.clone());
            } else {
                let text = msg.text_content();
                if !text.is_empty() {
                    self.history.push_assistant(&text);
                }
            }
        }
        LlmRole::User => {
            for block in &msg.content {
                if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                    self.history.push_tool_result(
                        tool_use_id.clone(), content.clone(), *is_error,
                    );
                }
            }
        }
        _ => {}
    }
}
```

### 6. Ctrl+C 抢救机制

**cancel_query 改为协作取消**：

```rust
fn cancel_query(&mut self) {
    if let Some(cancel) = self.cancel_token.as_ref() {
        cancel.cancel();  // 通知 tool_loop 停止
    }
    // 不 abort，等待 task 自然退出
}
```

**tool_loop 取消检测**：每次 LLM 调用前检查 `cancel_token.is_cancelled()`，取消时提前返回当前 messages（包含已执行的中间结果）。

**process_input** 中 tool_loop 返回后（正常或取消），统一走写回逻辑，确保已执行的 ToolUse/ToolResult 不丢失。

### 7. compact 模块适配

- `estimate_tokens` 改为遍历 `Vec<ContentBlock>` 估算
- `compress_single_turn` 从 ContentBlock 中提取 ToolResult
- `apply_pending` 替换对应的 ToolResult block
- `split_turn_blocks` 识别 turn 边界时检查 ToolUse 块

## 涉及文件

| 文件 | 变更 |
|------|------|
| `brain-core/src/types.rs` | 新增 ContentBlock，升级 ConversationMessage |
| `brain-llm/src/types.rs` | 删除 ContentBlock 定义，重导出 brain-core |
| `brain-main/src/conversation.rs` | ConversationHistory 方法升级 + to_chat_messages |
| `brain-main/src/main_brain.rs` | 写回逻辑 + 取消处理 |
| `brain-main/src/compact.rs` | 适配新结构 |
| `brain-main/src/tool_loop.rs` | 取消检测 |
| `ai-brain-cli/src/tui/app.rs` | cancel_query 改为协作取消 |

## 验收标准

1. tool_loop 结束后，历史中能找到 ToolUse 和 ToolResult 块
2. 第二轮 `to_chat_messages()` 能精确还原 assistant(ToolUse) → user(ToolResult) 序列
3. Ctrl+C 中断后，已执行的中间结果写入历史，下一轮 LLM 能看到
4. 旧的历史 JSON 文件能向后兼容加载
5. 现有测试全部通过，新增测试覆盖工具调用历史场景
