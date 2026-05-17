# Agent Dispatch 消息中间件设计

**日期**: 2026-05-17
**状态**: 已确认
**范围**: v2 路径专用（不碰 v1 brain-bus）

## 背景

当前子代理（Agent 工具）使用 `std::thread::spawn` fire-and-forget 模式，完成后无法通知主脑。主脑 tool_loop 拿到 `status="running"` 后停止工作，只有用户再次输入才会检查子代理状态。副脑（评估脑）的异步任务编排也存在类似问题。

参考 Claude Code 的设计：Foreground 子代理用 `for await` 阻塞等待，Background 子代理完成后通过全局输入队列注入通知。

## 方案选型

| 方案 | 描述 | 结论 |
|------|------|------|
| tokio channel 原生 | 散落在多个 crate 中 | 跨 crate 难维护 |
| Stream + channel 混合 | Stream trait 复杂度高 | 对当前场景无额外收益 |
| **新 crate + trait 抽象** | brain-dispatch，统一入口 | **选定** |

选定理由：架构一致性（已有 crate-per-component 模式）、可测试性（trait 可 mock）、易维护（修改集中在单 crate）。

## 架构：全局优先级事件队列 + Agent 注册表

```
┌─────────────────────────────────────────────────────────────┐
│                        AgentDispatch                         │
│  (brain-dispatch crate)                                     │
│                                                             │
│  ┌──────────────┐   ┌──────────────────────────────────┐   │
│  │ AgentRegistry│   │      PriorityQueue               │   │
│  │              │   │   Vec<PrioritizedEvent>           │   │
│  │ main ──┐    │   │   Urgent > Normal > Background    │   │
│  │ eval ──┤    │   └──────────┬───────────────────────┘   │
│  │ memory─┤    │              │                            │
│  │ evolver│    │   ┌──────────▼───────────────────────┐   │
│  └──────────────┘   │     dispatch_loop (tokio spawn)  │   │
│                     │                                  │   │
│  ┌──────────────┐   │  1. 从队列取最高优先级事件        │   │
│  │ trait         │   │  2. 匹配事件类型                 │   │
│  │ MessageBus   │   │  3. 路由到目标 agent              │   │
│  │ {            │   │  4. 处理结果/通知                 │   │
│  │  register()  │   └──────────────────────────────────┘   │
│  │  inject()    │                                          │
│  │  send_to()   │                                          │
│  │ }            │                                          │
│  └──────────────┘                                          │
│         ▲ implements                                       │
│  ┌──────┴────────┐                                         │
│  │TokioDispatch  │  tokio::mpsc + oneshot 实现             │
│  └───────────────┘                                         │
└─────────────────────────────────────────────────────────────┘
```

## 核心类型

### 消息类型

```rust
/// 调度事件 — 全局事件队列中流转的消息
enum DispatchEvent {
    // ── 子代理相关 ──
    /// 同步子代理请求（调用者通过 reply oneshot 阻塞等待结果）
    SyncAgentRequest {
        agent_id: AgentId,
        prompt: String,
        subagent_type: SubagentType,
        model: Option<String>,
        reply: oneshot::Sender<AgentResult>,
    },

    /// 异步子代理完成通知（子代理完成后注入队列）
    AsyncAgentCompleted {
        agent_id: AgentId,
        name: String,
        status: AgentStatus,  // Completed / Failed
        result: Option<String>,
    },

    // ── @脑名 路由 (P2 暂缓) ──
    /// 定向消息（@评估脑、@记忆脑 等）
    DirectedMessage {
        from: AgentId,
        to: BrainId,
        content: String,
        reply: Option<oneshot::Sender<AgentResult>>,
    },

    // ── 副脑异步任务 ──
    /// 副脑异步任务完成通知
    BrainTaskCompleted {
        brain_id: BrainId,
        task_type: String,     // "evaluation" / "memory_analysis" / ...
        result: BrainResponse,
    },

    // ── 用户输入 ──
    /// 用户新输入（最高优先级）
    UserInput {
        content: String,
    },
}
```

### 优先级

```rust
enum Priority {
    Urgent     = 0,  // 用户输入、安全相关
    Normal     = 1,  // 同步子代理请求
    Background = 2,  // 异步子代理完成通知、副脑任务完成
}
```

### Agent 注册表

```rust
struct AgentHandle {
    id: AgentId,
    brain_id: Option<BrainId>,
    tx: mpsc::Sender<AgentMessage>,  // 投递消息的通道
    agent_type: AgentType,           // Main / SubAgent / Brain
    capabilities: AgentCapabilities,
}

struct AgentCapabilities {
    has_tool_loop: bool,
    allowed_tools: Vec<String>,
    has_llm_access: bool,
}

struct AgentRegistry {
    agents: HashMap<AgentId, AgentHandle>,
}
```

### 错误处理

```rust
enum DispatchError {
    AgentNotFound(BrainId),
    ChannelClosed,
    Timeout(Duration),
    AgentFailed { reason: String },
    QueueFull,
}
```

## MessageBus Trait

```rust
#[async_trait]
pub trait MessageBus: Send + Sync {
    /// 注册 agent 到总线
    fn register(&self, handle: AgentHandle) -> Result<(), DispatchError>;

    /// 注入事件到全局队列（子代理完成、副脑任务完成等）
    async fn inject(&self, event: DispatchEvent, priority: Priority);

    /// 点对点发送（@脑名），阻塞等待结果
    async fn send_to(&self, to: BrainId, msg: String) -> Result<AgentResult, DispatchError>;

    /// 启动调度主循环（消费队列 → 路由 → 处理）
    async fn run(&self, output_tx: mpsc::Sender<MainLoopMessage>);

    /// 优雅关闭
    async fn shutdown(&self);
}
```

## 四大场景数据流

### 场景一：同步子代理（P0 — 阻塞等待）

```
tool_loop → LLM 返回 tool_use: Agent(prompt, subagent_type)
    │
    ▼
real_tool_executor.execute()
    │
    ├─ 创建 oneshot::channel()
    ├─ dispatch.inject(SyncAgentRequest { reply: tx }, Priority::Normal)
    │
    ▼
dispatch_loop 收到 SyncAgentRequest
    │
    ├─ spawn 子代理线程 (std::thread::spawn)
    │       ├─ run_agent_job()
    │       └─ 完成后 reply.send(AgentResult) ← oneshot
    │
    ▼
real_tool_executor 阻塞在 rx.recv()
    │
    ▼
拿到最终结果 (completed/failed)，返回给 tool_loop
    │
    ▼
tool_loop 将结果作为 tool_result 写入 messages，继续循环
```

**关键**：对 tool_loop 来说 Agent 工具和 bash 工具行为一致——调用 → 阻塞 → 结果。

### 场景二：异步子代理（P0 — 完成通知）

```
tool_loop → LLM 返回 tool_use: Agent(prompt, run_in_background=true)
    │
    ▼
real_tool_executor.execute()
    │
    ├─ 立即返回 { status: "async_launched", agent_id: "xxx" }
    │   （不阻塞，tool_loop 继续运行）
    │
    ├─ spawn 子代理线程
    │       ├─ run_agent_job()
    │       └─ 完成后 dispatch.inject(AsyncAgentCompleted, Priority::Background)
    │
    ▼
主脑继续处理其他事情...
    │
    ▼
dispatch_loop 取出 AsyncAgentCompleted
    │
    ├─ 注入到主脑消息流: AgentMessage::Notification(content)
    │   content = "<task-result><agent-id>xxx</agent-id>..."
    │
    ▼
主脑 tool_loop 在下一轮将通知作为 user 消息发给 LLM
    │
    ▼
LLM 看到子代理结果，决定下一步行动
```

**关键**：Priority::Background 确保不打断用户当前对话，主脑下一轮空闲时自动处理。

### 场景三：副脑异步任务编排（P1）

```
主脑处理完 → 评估脑判定需要评估
    │
    ▼
orchestrator 判定 should_eval
    │
    ├─ dispatch.send_to(BrainId::eval(), "评估主脑输出...")
    │
    ▼
评估脑 agent_loop 收到消息，异步执行 evaluate_with_verification()
    │
    ├─ 完成后 dispatch.inject(BrainTaskCompleted { result }, Priority::Background)
    │
    ▼
dispatch_loop → 注入主脑
    │
    ├─ evaluation.pass == false → AgentMessage::RevisionRequest { feedback }
    │   主脑 tool_loop 重新调用 LLM 修订
    │
    ├─ evaluation.pass == true → AgentMessage::TaskAck
    │   主脑继续或输出最终结果
```

**关键**：副脑任务完成后不再需要 orchestrator 轮询检查——全局队列自动触发下一步。

### 场景四：@脑名 路由（P2 暂缓）

@脑名 = 主脑旁路，指定脑作为独立 agent 完整处理任务（带 tool_loop + LLM + 工具集）。
结果直接输出到 TUI，不经过主脑。
需要为副脑配备完整 tool_loop 能力，暂缓实现。

## Crate 结构

```
rust/crates/brain-dispatch/
├── Cargo.toml
└── src/
    ├── lib.rs              — 公开接口 + re-export
    ├── bus.rs              — TokioDispatch 实现 (全局队列 + 注册表)
    ├── types.rs            — DispatchEvent, Priority, AgentHandle, AgentMessage...
    ├── registry.rs         — AgentRegistry (注册/查找/注销)
    ├── priority_queue.rs   — 优先级队列 (Vec 排序)
    └── dispatch_loop.rs    — 调度循环 (消费队列 → 路由 → 处理)
```

## 集成点

| 文件 | 改动 | 场景 |
|------|------|------|
| `tools/src/lib.rs` | `spawn_agent_job` 改为 oneshot 阻塞 + 可选 async 模式 | 一、二 |
| `real_tool_executor.rs` | Agent 工具走 dispatch 同步/异步路径 | 一、二 |
| `orchestrator.rs` | 初始化 TokioDispatch，注入 RealToolExecutor；评估闭环改事件驱动 | 三 |
| `ai-brain-cli/Cargo.toml` | 依赖 brain-dispatch | 集成 |

### 不动的部分

- **brain-bus** — v1 路径完全不动
- **brain-master** — v1 主脑不动
- **brain-core/agent.rs** — BrainAgent trait 不动
- **tool_loop.rs** — 核心循环逻辑不变

## 实施优先级

1. **P0**: 场景一（同步子代理阻塞等待）+ 场景二（异步子代理完成通知）
2. **P1**: 场景三（副脑异步任务编排）
3. **P2**: 场景四（@脑名 路由）— 暂缓

## 测试策略

- `brain-dispatch` 自带单元测试：mock agent 注册 + 消息收发 + 优先级排序
- 集成测试：验证同步子代理阻塞 + 异步子代理通知的端到端流程
- 现有 254 个测试不应受影响（v1 不动，v2 只改 Agent 工具调用方式）
