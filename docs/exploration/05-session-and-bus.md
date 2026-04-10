# Session 管理与消息总线探索报告

> 探索日期: 2026-04-09

## 一、Session 管理

### 1.1 Session 结构

`crates/runtime/src/session.rs`

```rust
struct Session {
    version, session_id, created_at_ms, updated_at_ms,
    messages: Vec<ConversationMessage>,
    compaction: Option<SessionCompaction>,
    fork: Option<SessionFork>,
    persistence: Option<SessionPersistence>,
}
```

### 1.2 ConversationMessage

```rust
enum ConversationMessage {
    System { blocks: Vec<ContentBlock> },
    User { blocks: Vec<ContentBlock> },
    Assistant { blocks: Vec<ContentBlock> },
    Tool { tool_use_id, tool_name, blocks: Vec<ContentBlock>, is_error },
}

enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, tool_name: String, output: String, is_error: bool },
}
```

### 1.3 持久化

- JSONL 追加写入
- 文件轮转：256KB 后轮转，最多 3 个历史文件
- 路径：`~/.claude/projects/{project-hash}/sessions/`

### 1.4 与新架构的差距

| 差距 | 说明 | 严重程度 |
|------|------|---------|
| **推理脑无对话历史** | 推理脑每次 LLM 调用全新构建 messages，无状态累积 | 高 |
| **记忆脑无法读取推理脑上下文** | 两脑之间只有纯文本通道，无法传递结构化对话历史 | 高 |
| **主脑无 TaskPlan 状态管理** | 只有单一 TaskPhase 枚举，不是多步骤状态机 | 中 |
| **序列化体系不兼容** | Session 用自定义 JsonValue，副脑用 serde_json | 低 |

---

## 二、消息总线（brain-bus）

### 2.1 三通道设计

`crates/brain-bus/src/bus.rs`

| 通道 | 类型 | 方向 | 用途 |
|------|------|------|------|
| **通道1 broadcast** | `tokio::broadcast` | 感知脑 → 所有副脑 | 广播输入 |
| **通道2 collaboration** | `mpsc` + `CollaborationRouter` | 副脑间点对点 | 协作调度 |
| **通道3 result** | `mpsc` | 副脑 → 主脑 | 提交结果 |

### 2.2 消息类型

`crates/brain-bus/src/types.rs`

```rust
struct BroadcastMessage {
    content: String,
    raw_input: String,
    context: BrainContext,
    timestamp: DateTime<Utc>,
}

struct CollaborationMessage {
    id: String,
    from: BrainId,
    to: Vec<BrainId>,           // 空=广播给所有
    correlation_id: Option<String>,
    hop_count: u32,             // >3 被丢弃（防循环）
    priority: Priority,
    content: String,            // ← 纯文本！无法传结构化数据
    kind: CollaborationKind,    // Request / Response / Dispatch
}

struct BrainResponse {
    from: BrainId,
    relevance: f64,
    confidence: f64,
    result: BrainResponsePayload,
    need_slow_think: bool,
    timestamp: DateTime<Utc>,
}

enum BrainResponsePayload {
    FastThink / SlowThink / SafetyCheck / TruthfulnessCheck /
    MemoryRecall / ToolResult / Evaluation / NotRelevant / Processing
}
```

### 2.3 当前使用模式

```
感知脑 process_input()
  → broadcast_tx.send(BroadcastMessage)          [通道1]

各副脑 on_broadcast()
  → fast_think(&msg)
  → if relevant { submit_result(FastThink) }      [通道3]
  → if !relevant { submit_result(NotRelevant) }   [通道3]

主脑 run_once()
  → broadcast_rx.recv()                            [通道1]
  → collect_fast_thinks() (5s 超时)                [通道3]
  → if need_slow: dispatch_slow_think()            [通道2]
  → collect_slow_thinks() (60s 超时)               [通道3]
  → synthesize() → MasterOutput
```

### 2.4 与新架构的差距

| 差距 | 说明 | 严重程度 |
|------|------|---------|
| **广播→收集模式不适配** | 新架构是主脑调度→推理脑执行→记忆脑按需服务 | 高 |
| **消息体是纯文本** | CollaborationMessage.content 是 String，无法传结构化数据 | 高 |
| **缺乏请求-响应关联** | correlation_id 存在但无实际匹配逻辑 | 中 |
| **主脑到副脑应走直接方法调用** | 当前通过通道异步传递，增加了延迟和复杂度 | 中 |

### 2.5 新架构下消息总线的建议

**保留通道1和通道3用于异步事件通知，但核心调用走直接方法调用：**

```
新架构消息流：

通道1（保留）：感知脑广播 TaskPlan
  → 通知所有副脑"有新任务了"

直接方法调用（新增）：
  主脑 → 推理脑.execute_step(step, tools)   // 直接调用
  推理脑 → 记忆脑.query(query)                // 直接调用
  推理脑 → 校验 guard_check(tool_call)         // 函数调用

通道3（保留）：异步事件通知
  推理脑 → 通知主脑"step 完成"
  记忆脑 → 通知主脑"压缩完成"
```

这样主脑持有各副脑的 `Arc<dyn BrainAgent>` 引用，调度就是直接方法调用，不需要通过消息通道中转。
