# Agent Dispatch 消息中间件实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 为 v2 路径构建统一的 agent 消息调度中间件，解决子代理 fire-and-forget、异步通知缺失、副脑编排不自动的问题。

**Architecture:** 新建 `brain-dispatch` crate，用全局优先级事件队列 + Agent 注册表模式。同步子代理通过 oneshot channel 阻塞等待；异步子代理完成后通过 `inject()` 注入通知到全局队列；副脑任务编排改为事件驱动。

**Tech Stack:** Rust, tokio (mpsc/oneshot/broadcast/watch), serde, thiserror, tracing

---

## Task 1: 创建 brain-dispatch crate 骨架

**Files:**
- Create: `rust/crates/brain-dispatch/Cargo.toml`
- Create: `rust/crates/brain-dispatch/src/lib.rs`
- Create: `rust/crates/brain-dispatch/src/types.rs`

**Step 1: 创建 Cargo.toml**

```toml
[package]
name = "brain-dispatch"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
brain-core = { path = "../brain-core" }
tokio = { version = "1", features = ["sync", "time", "rt", "rt-multi-thread", "macros"] }
serde = { version = "1", features = ["derive"] }
serde_json.workspace = true
chrono = { version = "0.4", features = ["serde"] }
thiserror = "2"
tracing = "0.1"

[lints]
workspace = true
```

**Step 2: 创建 types.rs**

```rust
//! brain-dispatch 核心类型定义

use brain_core::types::BrainId;
use std::collections::BTreeSet;
use std::time::Duration;

/// Agent 唯一标识
pub type AgentId = String;

/// 子代理类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentType {
    Explore,
    GeneralPurpose,
    Plan,
    Verification,
}

/// Agent 执行结果
#[derive(Debug, Clone)]
pub struct AgentResult {
    pub agent_id: AgentId,
    pub status: AgentStatus,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Agent 状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Completed,
    Failed,
}

/// 调度事件 — 全局事件队列中流转的消息
#[derive(Debug)]
pub enum DispatchEvent {
    /// 同步子代理请求（调用者通过 reply oneshot 阻塞等待结果）
    SyncAgentRequest {
        agent_id: AgentId,
        prompt: String,
        subagent_type: SubagentType,
        description: String,
        model: Option<String>,
        reply: tokio::sync::oneshot::Sender<AgentResult>,
    },

    /// 异步子代理完成通知（子代理完成后注入队列）
    AsyncAgentCompleted {
        agent_id: AgentId,
        name: String,
        status: AgentStatus,
        result: Option<String>,
        error: Option<String>,
    },

    /// 副脑异步任务完成通知
    BrainTaskCompleted {
        brain_id: BrainId,
        task_type: String,
        result_summary: String,
    },

    /// 用户新输入（最高优先级）
    UserInput {
        content: String,
    },
}

/// 优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// 用户输入、安全相关
    Urgent = 0,
    /// 同步子代理请求
    Normal = 1,
    /// 异步完成通知、副脑任务完成
    Background = 2,
}

/// 带优先级的事件包装
#[derive(Debug)]
pub struct PrioritizedEvent {
    pub event: DispatchEvent,
    pub priority: Priority,
    /// 入队时间戳，用于同优先级 FIFO
    pub enqueued_at: std::time::Instant,
}

impl PrioritizedEvent {
    pub fn new(event: DispatchEvent, priority: Priority) -> Self {
        Self {
            event,
            priority,
            enqueued_at: std::time::Instant::now(),
        }
    }
}

/// Agent 类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentType {
    /// 主脑
    Main,
    /// 子代理（Agent 工具创建的）
    SubAgent,
    /// 副脑（评估脑、记忆脑等）
    Brain,
}

/// Agent 注册信息
#[derive(Debug, Clone)]
pub struct AgentHandle {
    pub id: AgentId,
    pub brain_id: Option<BrainId>,
    pub agent_type: AgentType,
    pub allowed_tools: BTreeSet<String>,
}

/// 主循环消息 — dispatch_loop 输出给主脑消费的消息
#[derive(Debug, Clone)]
pub enum MainLoopMessage {
    /// 异步子代理完成，需要注入到主脑 LLM 上下文
    AgentNotification {
        agent_id: AgentId,
        name: String,
        status: AgentStatus,
        result: Option<String>,
    },
    /// 副脑任务完成通知
    BrainTaskNotification {
        brain_id: BrainId,
        task_type: String,
        result_summary: String,
    },
}

/// 错误类型
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("agent not found: {0:?}")]
    AgentNotFound(BrainId),
    #[error("channel closed")]
    ChannelClosed,
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("agent failed: {reason}")]
    AgentFailed { reason: String },
    #[error("queue full")]
    QueueFull,
    #[error("{0}")]
    Other(String),
}
```

**Step 3: 创建 lib.rs**

```rust
//! brain-dispatch: v2 路径的统一 agent 消息调度中间件
//!
//! 提供全局优先级事件队列 + Agent 注册表，覆盖：
//! - 同步子代理（阻塞等待）
//! - 异步子代理（完成通知）
//! - 副脑异步任务编排

mod bus;
mod types;

pub use bus::TokioDispatch;
pub use types::*;
```

**Step 4: 创建 bus.rs 骨架**

```rust
//! TokioDispatch — 基于 tokio channel 的消息总线实现

use crate::*;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 基于 tokio channel 的 MessageBus 实现
pub struct TokioDispatch {
    queue_tx: tokio::sync::mpsc::Sender<PrioritizedEvent>,
    queue_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<PrioritizedEvent>>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl TokioDispatch {
    pub fn new(capacity: usize) -> Self {
        let (queue_tx, queue_rx) = tokio::sync::mpsc::channel(capacity);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        Self {
            queue_tx,
            queue_rx: Arc::new(Mutex::new(queue_rx)),
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// 注入事件到全局队列
    pub async fn inject(&self, event: DispatchEvent, priority: Priority) {
        let pe = PrioritizedEvent::new(event, priority);
        if let Err(e) = self.queue_tx.send(pe).await {
            tracing::error!("dispatch inject failed: {e}");
        }
    }

    /// 优雅关闭
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}
```

**Step 5: 验证编译**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo check -p brain-dispatch`
Expected: 编译通过（可能有一些 unused warnings，正常）

**Step 6: Commit**

```bash
git add rust/crates/brain-dispatch/
git commit -m "feat(dispatch): create brain-dispatch crate skeleton with core types"
```

---

## Task 2: 同步子代理阻塞等待 — 改造 tools crate

**Files:**
- Modify: `rust/crates/tools/src/lib.rs` — `spawn_agent_job` 和 `execute_agent_with_spawn`

**目标**：让 `execute_agent` 同步阻塞直到子代理完成，返回最终结果（不再是 "running"）。

**Step 1: 修改 spawn_agent_job 返回 oneshot Receiver**

找到 `spawn_agent_job` 函数（约行 1967），替换为：

```rust
/// Agent 子代理执行结果
struct AgentDone {
    status: String,
    final_text: Option<String>,
    error: Option<String>,
    duration_ms: u64,
}

fn spawn_agent_job(job: AgentJob) -> Result<std::sync::mpsc::Receiver<AgentDone>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_name = format!("clawd-agent-{}", job.manifest.agent_id);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let start = std::time::Instant::now();
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_agent_job(&job)));
            let duration_ms = start.elapsed().as_millis() as u64;
            let done = match result {
                Ok(Ok(())) => {
                    // run_agent_job 内部已经调用了 persist_agent_terminal_state
                    // 读取最终的 output 文件获取结果文本
                    let final_text = std::fs::read_to_string(&job.manifest.output_file)
                        .ok()
                        .and_then(|content| {
                            // 取最后一个 ## Output 或 ## Result 之后的内容
                            content.split("## Output\n").last()
                                .or_else(|| content.split("## Result\n").last())
                                .map(|s| s.trim().to_string())
                        });
                    AgentDone {
                        status: "completed".into(),
                        final_text,
                        error: None,
                        duration_ms,
                    }
                }
                Ok(Err(error)) => {
                    let _ =
                        persist_agent_terminal_state(&job.manifest, "failed", None, Some(error.clone()));
                    AgentDone {
                        status: "failed".into(),
                        final_text: None,
                        error: Some(error),
                        duration_ms,
                    }
                }
                Err(_) => {
                    let error = String::from("sub-agent thread panicked");
                    let _ = persist_agent_terminal_state(
                        &job.manifest,
                        "failed",
                        None,
                        Some(error.clone()),
                    );
                    AgentDone {
                        status: "failed".into(),
                        final_text: None,
                        error: Some(error),
                        duration_ms,
                    }
                }
            };
            let _ = tx.send(done);
        })
        .map(|_| rx)
        .map_err(|error| error.to_string())
}
```

**Step 2: 修改 execute_agent_with_spawn 阻塞等待结果**

找到 `execute_agent_with_spawn` 函数（约行 1890），修改签名和实现：

```rust
fn execute_agent_with_spawn<F>(input: AgentInput, spawn_fn: F) -> Result<AgentOutput, String>
where
    F: FnOnce(AgentJob) -> Result<std::sync::mpsc::Receiver<AgentDone>, String>,
{
    if input.description.trim().is_empty() {
        return Err(String::from("description must not be empty"));
    }
    if input.prompt.trim().is_empty() {
        return Err(String::from("prompt must not be empty"));
    }

    let agent_id = make_agent_id();
    let output_dir = agent_store_dir()?;
    std::fs::create_dir_all(&output_dir).map_err(|error| error.to_string())?;
    let output_file = output_dir.join(format!("{agent_id}.md"));
    let manifest_file = output_dir.join(format!("{agent_id}.json"));
    let normalized_subagent_type = normalize_subagent_type(input.subagent_type.as_deref());
    let model = resolve_agent_model(input.model.as_deref());
    let agent_name = input
        .name
        .as_deref()
        .map(slugify_agent_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| slugify_agent_name(&input.description));
    let created_at = iso8601_now();
    let system_prompt = build_agent_system_prompt(&normalized_subagent_type)?;
    let allowed_tools = allowed_tools_for_subagent(&normalized_subagent_type);

    let output_contents = format!(
        "# Agent Task

- id: {}
- name: {}
- description: {}
- subagent_type: {}
- created_at: {}

## Prompt

{}
",
        agent_id, agent_name, input.description, normalized_subagent_type, created_at, input.prompt
    );
    std::fs::write(&output_file, output_contents).map_err(|error| error.to_string())?;

    let manifest = AgentOutput {
        agent_id,
        name: agent_name,
        description: input.description,
        subagent_type: Some(normalized_subagent_type),
        model: Some(model),
        status: String::from("running"),
        output_file: output_file.display().to_string(),
        manifest_file: manifest_file.display().to_string(),
        created_at: created_at.clone(),
        started_at: Some(created_at),
        completed_at: None,
        error: None,
    };
    write_agent_manifest(&manifest)?;

    let manifest_for_spawn = manifest.clone();
    let job = AgentJob {
        manifest: manifest_for_spawn,
        prompt: input.prompt,
        system_prompt,
        allowed_tools,
    };
    let result_rx = spawn_fn(job)?;

    // ── 同步阻塞等待子代理完成 ──
    let agent_done = result_rx.recv().map_err(|_| {
        String::from("sub-agent channel closed unexpectedly (thread panicked?)")
    })?;

    // 构建最终 manifest
    let final_manifest = AgentOutput {
        status: agent_done.status,
        completed_at: Some(iso8601_now()),
        error: agent_done.error,
        ..manifest
    };

    Ok(final_manifest)
}
```

**Step 3: 修改 run_agent_job 中的持久化逻辑**

`run_agent_job`（约行 1994）已经调用 `persist_agent_terminal_state`，保持不变。但需要确保最终输出文本被正确写出。修改 `run_agent_job`：

```rust
fn run_agent_job(job: &AgentJob) -> Result<(), String> {
    tracing::debug!("[子代理] run_agent_job 开始: agent={}, model={:?}", job.manifest.agent_id, job.manifest.model);
    let mut runtime = build_agent_runtime(job).map_err(|e| {
        tracing::error!("[子代理] build_agent_runtime 失败: {e}");
        e
    })?.with_max_iterations(DEFAULT_AGENT_MAX_ITERATIONS);
    tracing::debug!("[子代理] build_agent_runtime 成功，开始 run_turn");
    let summary = runtime
        .run_turn(job.prompt.clone(), None)
        .map_err(|error| {
            tracing::error!("[子代理] run_turn 失败: {error}");
            error.to_string()
        })?;
    tracing::debug!("[子代理] run_turn 完成，迭代次数: {}", summary.iterations);
    let final_text = final_assistant_text(&summary);
    persist_agent_terminal_state(&job.manifest, "completed", Some(final_text.as_str()), None)
}
```

这段代码本身不需要改，保持原样即可。因为 `spawn_agent_job` 中的 `AgentDone` 通过读取 output 文件获取结果。

**Step 4: 验证编译**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo check -p tools`
Expected: 编译通过

**Step 5: Commit**

```bash
git add rust/crates/tools/src/lib.rs
git commit -m "feat(tools): make Agent tool synchronous — block until subagent completes"
```

---

## Task 3: 异步子代理 — 双模式支持

**Files:**
- Modify: `rust/crates/tools/src/lib.rs` — AgentInput 增加 `run_in_background` 字段

**目标**：Agent 工具支持 `run_in_background` 参数。默认同步阻塞；`run_in_background=true` 时立即返回 "async_launched"，完成后通过回调通知。

**Step 1: 修改 AgentInput 增加后台模式字段**

找到 `AgentInput`（约行 1028），增加字段：

```rust
#[derive(Debug, Deserialize)]
struct AgentInput {
    description: String,
    prompt: String,
    subagent_type: Option<String>,
    name: Option<String>,
    model: Option<String>,
    #[serde(default)]
    run_in_background: bool,
}
```

**Step 2: 定义异步完成回调类型**

在 tools/src/lib.rs 顶部添加：

```rust
/// 异步子代理完成回调类型
type AsyncCompletionCallback = Arc<dyn Fn(&str, &str, Option<&str>, Option<&str>) + Send + Sync>;
```

**Step 3: 修改 execute_agent_with_spawn 支持双模式**

在 `execute_agent_with_spawn` 函数中，签名增加回调参数：

```rust
fn execute_agent_with_spawn<F>(
    input: AgentInput,
    spawn_fn: F,
    async_callback: Option<AsyncCompletionCallback>,
) -> Result<AgentOutput, String>
where
    F: FnOnce(AgentJob, Option<AsyncCompletionCallback>) -> Result<std::sync::mpsc::Receiver<AgentDone>, String>,
{
    // ... 前面校验和 manifest 构建不变 ...

    let result_rx = spawn_fn(job, if input.run_in_background { async_callback } else { None })?;

    if input.run_in_background {
        // 异步模式：立即返回 "running" manifest
        Ok(manifest)
    } else {
        // 同步模式：阻塞等待完成
        let agent_done = result_rx.recv().map_err(|_| {
            String::from("sub-agent channel closed unexpectedly (thread panicked?)")
        })?;
        let final_manifest = AgentOutput {
            status: agent_done.status,
            completed_at: Some(iso8601_now()),
            error: agent_done.error,
            ..manifest
        };
        Ok(final_manifest)
    }
}
```

**Step 4: 修改 spawn_agent_job 支持异步回调**

```rust
fn spawn_agent_job(
    job: AgentJob,
    async_callback: Option<AsyncCompletionCallback>,
) -> Result<std::sync::mpsc::Receiver<AgentDone>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_name = format!("clawd-agent-{}", job.manifest.agent_id);
    let is_async = async_callback.is_some();

    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let start = std::time::Instant::now();
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_agent_job(&job)));
            let duration_ms = start.elapsed().as_millis() as u64;
            let done = match result {
                Ok(Ok(())) => {
                    let final_text = std::fs::read_to_string(&job.manifest.output_file)
                        .ok()
                        .and_then(|content| {
                            content.split("## Output\n").last()
                                .or_else(|| content.split("## Result\n").last())
                                .map(|s| s.trim().to_string())
                        });
                    AgentDone {
                        status: "completed".into(),
                        final_text: final_text.clone(),
                        error: None,
                        duration_ms,
                    }
                }
                Ok(Err(error)) => {
                    let _ =
                        persist_agent_terminal_state(&job.manifest, "failed", None, Some(error.clone()));
                    AgentDone {
                        status: "failed".into(),
                        final_text: None,
                        error: Some(error.clone()),
                        duration_ms,
                    }
                }
                Err(_) => {
                    let error = String::from("sub-agent thread panicked");
                    let _ = persist_agent_terminal_state(
                        &job.manifest,
                        "failed",
                        None,
                        Some(error.clone()),
                    );
                    AgentDone {
                        status: "failed".into(),
                        final_text: None,
                        error: Some(error),
                        duration_ms,
                    }
                }
            };

            // 异步模式：调用回调通知完成
            if let Some(cb) = async_callback {
                cb(
                    &job.manifest.agent_id,
                    &done.status,
                    done.final_text.as_deref(),
                    done.error.as_deref(),
                );
            }

            // 同步模式：通过 channel 发送结果
            if !is_async {
                let _ = tx.send(done);
            }
        })
        .map(|_| rx)
        .map_err(|error| error.to_string())
}
```

**Step 5: 更新 execute_agent 和 run_agent 调用链**

```rust
fn run_agent(input: AgentInput) -> Result<AgentOutput, String> {
    execute_agent(input)
}

fn execute_agent(input: AgentInput) -> Result<AgentOutput, String> {
    execute_agent_with_spawn(input, spawn_agent_job, None)
}
```

**Step 6: 更新 Agent 工具定义中的 input_schema**

找到 Agent 工具的 `input_schema` 定义（约行 411），在 `properties` 中添加：

```json
"run_in_background": {
    "type": "boolean",
    "description": "Set to true to run this agent in the background. You will be automatically notified when it completes.",
    "default": false
}
```

**Step 7: 验证编译**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo check -p tools`
Expected: 编译通过

**Step 8: Commit**

```bash
git add rust/crates/tools/src/lib.rs
git commit -m "feat(tools): add run_in_background support for Agent tool"
```

---

## Task 4: 集成 brain-dispatch 到 orchestrator

**Files:**
- Modify: `rust/crates/ai-brain-cli/Cargo.toml` — 添加依赖
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs` — 初始化 TokioDispatch
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs` — 接入 dispatch

**目标**：Orchestrator 持有 `TokioDispatch` 实例，RealToolExecutor 通过它调度子代理和接收异步通知。

**Step 1: 添加依赖**

在 `rust/crates/ai-brain-cli/Cargo.toml` 的 `[dependencies]` 中添加：

```toml
brain-dispatch = { path = "../brain-dispatch" }
```

**Step 2: Orchestrator 持有 TokioDispatch**

在 `orchestrator.rs` 的 `Orchestrator` struct 中添加字段：

```rust
pub struct Orchestrator {
    // ... 现有字段 ...
    dispatch: Arc<TokioDispatch>,
    dispatch_rx: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<MainLoopMessage>>>>,
}
```

在 `Orchestrator::new()` 中初始化：

```rust
let dispatch = Arc::new(TokioDispatch::new(256));
let (dispatch_output_tx, dispatch_output_rx) = tokio::sync::mpsc::channel(64);
```

启动 dispatch loop（在 tokio::spawn 中）：

```rust
{
    let dispatch_clone = dispatch.clone();
    let mut shutdown_rx = shutdown_rx.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = dispatch_clone.run_dispatch_loop(dispatch_output_tx) => {}
            _ = shutdown_rx.changed() => {
                tracing::info!("dispatch loop shutting down");
            }
        }
    });
}
```

保存 `dispatch_output_rx` 到 Orchestrator 字段。

**Step 3: RealToolExecutor 接入 dispatch**

修改 `RealToolExecutor` struct：

```rust
pub struct RealToolExecutor {
    tool_descriptors: HashMap<String, ToolDescriptor>,
    memory_brain: Option<Arc<Mutex<MemoryBrain>>>,
    dispatch: Option<Arc<TokioDispatch>>,
}
```

添加方法：

```rust
impl RealToolExecutor {
    pub fn new() -> Self { /* 现有逻辑 */ }

    pub fn with_memory(memory: Option<Arc<Mutex<MemoryBrain>>>) -> Self { /* 现有逻辑 */ }

    pub fn with_dispatch(
        memory: Option<Arc<Mutex<MemoryBrain>>>,
        dispatch: Arc<TokioDispatch>,
    ) -> Self {
        let mut executor = Self::with_memory(memory);
        executor.dispatch = Some(dispatch);
        executor
    }
}
```

在 `execute()` 方法中，对 Agent 工具添加异步回调接入：

```rust
// 在 execute() 方法的 Agent 工具特殊处理中
if name == "Agent" {
    // 检查是否有 run_in_background=true
    let is_async = input
        .get("run_in_background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_async {
        if let Some(ref dispatch) = self.dispatch_from_executor {
            // 创建异步回调，完成后注入 dispatch 队列
            let dispatch_clone = dispatch.clone();
            let async_callback: AsyncCompletionCallback = Arc::new(
                move |agent_id: &str, status: &str, result: Option<&str>, error: Option<&str>| {
                    let agent_id = agent_id.to_string();
                    let status_str = status.to_string();
                    let result_text = result.map(|s| s.to_string());
                    let error_text = error.map(|s| s.to_string());

                    // 这里无法直接 async，需要用 tokio::spawn
                    // 或者用 std::sync::mpsc 桥接
                    // 具体实现见 Task 5
                },
            );
            // 传入回调
        }
    }
}
```

> 注意：`real_tool_executor.rs` 的 `execute()` 是 async 方法，但 `tools::execute_tool` 是同步的。异步回调需要在 spawn_agent_job 的线程中调用，跨线程到 tokio runtime 需要用 `tokio::runtime::Handle::current()` 捕获。

**Step 4: 验证编译**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo check -p ai-brain-cli`
Expected: 编译通过

**Step 5: Commit**

```bash
git add rust/crates/ai-brain-cli/Cargo.toml rust/crates/ai-brain-cli/src/
git commit -m "feat(cli): integrate brain-dispatch into orchestrator and tool executor"
```

---

## Task 5: 异步子代理通知桥接 — 跨线程注入

**Files:**
- Modify: `rust/crates/brain-dispatch/src/bus.rs` — 添加 `inject_sync` 方法
- Modify: `rust/crates/tools/src/lib.rs` — 异步回调中注入通知

**目标**：子代理在 `std::thread::spawn` 中运行，完成时需要跨线程注入事件到 tokio 异步队列。

**Step 1: 在 TokioDispatch 中添加 sync 注入方法**

在 `bus.rs` 中添加：

```rust
impl TokioDispatch {
    /// 从同步上下文注入事件（供 std::thread 中使用）
    pub fn inject_sync(&self, agent_id: String, name: String, status: String, result: Option<String>, error: Option<String>) {
        // 通过 tokio::runtime::Handle 向异步队列发送
        // 使用 try_send 替代 send（不需要 await）
        let event = DispatchEvent::AsyncAgentCompleted {
            agent_id,
            name,
            status: if status == "completed" {
                AgentStatus::Completed
            } else {
                AgentStatus::Failed
            },
            result,
            error,
        };
        let pe = PrioritizedEvent::new(event, Priority::Background);

        // 使用 try_into 或直接通过 channel 的 try_send
        match self.queue_tx.try_send(pe) {
            Ok(()) => tracing::info!("async agent completion injected to dispatch queue"),
            Err(e) => tracing::error!("failed to inject async agent completion: {e}"),
        }
    }
}
```

**Step 2: 在 tools crate 中使用 inject_sync**

`spawn_agent_job` 中的异步回调需要持有 `TokioDispatch` 的引用。由于 `std::thread::spawn` 需要 `'static`，需要用 `Arc`。

修改 `spawn_agent_job` 签名，增加可选的 dispatch 参数：

```rust
fn spawn_agent_job(
    job: AgentJob,
    dispatch: Option<Arc<dyn Fn(String, String, String, Option<String>, Option<String>) + Send + Sync>>,
) -> Result<std::sync::mpsc::Receiver<AgentDone>, String> {
    // ...
    // 在完成时：
    if let Some(ref dispatch_fn) = dispatch {
        dispatch_fn(
            job.manifest.agent_id.clone(),
            job.manifest.name.clone(),
            done.status.clone(),
            done.final_text.clone(),
            done.error.clone(),
        );
    }
}
```

**Step 3: 验证编译**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo check -p tools -p ai-brain-cli`
Expected: 编译通过

**Step 4: Commit**

```bash
git add rust/crates/brain-dispatch/src/bus.rs rust/crates/tools/src/lib.rs rust/crates/ai-brain-cli/src/
git commit -m "feat(dispatch): bridge async agent completion from std::thread to tokio queue"
```

---

## Task 6: dispatch_loop 调度循环实现

**Files:**
- Modify: `rust/crates/brain-dispatch/src/bus.rs` — 实现 `run_dispatch_loop`

**目标**：实现核心调度循环，消费优先级队列，路由事件到目标。

**Step 1: 实现 run_dispatch_loop**

```rust
impl TokioDispatch {
    /// 启动调度主循环
    pub async fn run_dispatch_loop(
        &self,
        output_tx: tokio::sync::mpsc::Sender<MainLoopMessage>,
    ) {
        let mut queue: Vec<PrioritizedEvent> = Vec::new();
        let mut rx = self.queue_rx.lock().await;

        loop {
            if *self.shutdown_rx.borrow() {
                tracing::info!("dispatch_loop: shutdown signal received");
                break;
            }

            // 等待下一个事件
            match rx.recv().await {
                Some(pe) => {
                    // 插入到优先级队列（保持排序）
                    let insert_pos = queue
                        .iter()
                        .position(|existing| existing.priority > pe.priority)
                        .unwrap_or(queue.len());
                    queue.insert(insert_pos, pe);

                    // 尝试排空已有事件（非阻塞）
                    while let Ok(pe) = rx.try_recv() {
                        let insert_pos = queue
                            .iter()
                            .position(|existing| existing.priority > pe.priority)
                            .unwrap_or(queue.len());
                        queue.insert(insert_pos, pe);
                    }

                    // 处理最高优先级事件
                    if let Some(event) = queue.pop() {
                        // 注意：pop 从末尾取，但优先级最低的在末尾
                        // 应该从头部取（最高优先级）
                    }
                    // 修正：从头部取
                    if !queue.is_empty() {
                        let pe = queue.remove(0); // 取最高优先级（index 0 = Urgent）
                        self.handle_event(pe.event, &output_tx).await;
                    }
                }
                None => {
                    tracing::info!("dispatch_loop: queue channel closed");
                    break;
                }
            }
        }
    }

    async fn handle_event(
        &self,
        event: DispatchEvent,
        output_tx: &tokio::sync::mpsc::Sender<MainLoopMessage>,
    ) {
        match event {
            DispatchEvent::SyncAgentRequest { reply, .. } => {
                // 同步请求由 spawn_agent_job 直接处理
                // reply 的发送在 spawn_agent_job 的线程中完成
                // dispatch_loop 不需要额外处理
                // 但如果 reply 已经被消费，这里 drop 是安全的
                drop(reply);
            }
            DispatchEvent::AsyncAgentCompleted {
                agent_id,
                name,
                status,
                result,
                ..
            } => {
                let msg = MainLoopMessage::AgentNotification {
                    agent_id,
                    name,
                    status,
                    result,
                };
                if let Err(e) = output_tx.send(msg).await {
                    tracing::error!("dispatch_loop: failed to forward agent notification: {e}");
                }
            }
            DispatchEvent::BrainTaskCompleted {
                brain_id,
                task_type,
                result_summary,
            } => {
                let msg = MainLoopMessage::BrainTaskNotification {
                    brain_id,
                    task_type,
                    result_summary,
                };
                if let Err(e) = output_tx.send(msg).await {
                    tracing::error!("dispatch_loop: failed to forward brain notification: {e}");
                }
            }
            DispatchEvent::UserInput { .. } => {
                // 用户输入直接传递，暂时不处理
                tracing::debug!("dispatch_loop: user input event (not yet routed)");
            }
        }
    }
}
```

**Step 2: 验证编译**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo check -p brain-dispatch`
Expected: 编译通过

**Step 3: Commit**

```bash
git add rust/crates/brain-dispatch/src/bus.rs
git commit -m "feat(dispatch): implement dispatch_loop with priority queue and event routing"
```

---

## Task 7: 主脑消费异步通知 — TUI 集成

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs` — 在 query_streaming 中消费 dispatch 通知

**目标**：主脑在 tool_loop 完成后，自动消费 dispatch 队列中的异步通知，将其注入到 LLM 上下文。

**Step 1: 在 query_streaming 结尾添加 dispatch 通知消费**

在 `orchestrator.rs` 的 `query_streaming` 方法中，v2 主脑处理完成后，检查 dispatch 队列：

```rust
// --- 4. 异步子代理通知处理 ---
{
    let mut dispatch_rx = self.dispatch_rx.lock().await;
    if let Some(ref mut rx) = *dispatch_rx {
        // 非阻塞检查是否有待处理的通知
        while let Ok(msg) = rx.try_recv() {
            match msg {
                MainLoopMessage::AgentNotification { agent_id, name, status, result } => {
                    let notification = format!(
                        "<task-result>\n  <agent-id>{agent_id}</agent-id>\n  <name>{name}</name>\n  <status>{status:?}</status>\n  <result>{}</result>\n</task-result>",
                        result.unwrap_or_default()
                    );
                    // 注入到主脑消息流
                    send_progress(&tx, ProgressEvent::TextDelta {
                        text: format!("\n📡 异步子代理完成: {name} ({status:?})\n"),
                    }).await;
                    // 如果主脑还在运行，可以让它继续处理
                    tracing::info!("异步子代理通知: {name} status={status:?}");
                }
                MainLoopMessage::BrainTaskNotification { brain_id, task_type, result_summary } => {
                    send_progress(&tx, ProgressEvent::TextDelta {
                        text: format!("\n📡 副脑任务完成: {brain_id:?} {task_type}\n"),
                    }).await;
                    tracing::info!("副脑任务通知: {brain_id:?} {task_type}");
                }
            }
        }
    }
}
```

**Step 2: 验证编译**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo check -p ai-brain-cli`
Expected: 编译通过

**Step 3: Commit**

```bash
git add rust/crates/ai-brain-cli/src/orchestrator.rs
git commit -m "feat(cli): consume async agent notifications in orchestrator query_streaming"
```

---

## Task 8: 测试验证

**Files:**
- Create: `rust/crates/brain-dispatch/tests/dispatch_test.rs`
- Modify: `rust/crates/tools/src/lib.rs` — 确保 tests 模块通过

**Step 1: 编写 brain-dispatch 单元测试**

```rust
use brain_dispatch::*;

#[tokio::test]
async fn test_dispatch_lifecycle() {
    let dispatch = TokioDispatch::new(64);
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);

    // 启动 dispatch loop
    let dispatch_clone = dispatch.clone_arc();
    let handle = tokio::spawn(async move {
        dispatch_clone.run_dispatch_loop(tx).await;
    });

    // 注入异步完成通知
    dispatch.inject(
        DispatchEvent::AsyncAgentCompleted {
            agent_id: "test-agent-1".into(),
            name: "test-agent".into(),
            status: AgentStatus::Completed,
            result: Some("task done".into()),
            error: None,
        },
        Priority::Background,
    ).await;

    // 等待通知
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        rx.recv(),
    ).await.unwrap().unwrap();

    match msg {
        MainLoopMessage::AgentNotification { agent_id, name, status, result } => {
            assert_eq!(agent_id, "test-agent-1");
            assert_eq!(name, "test-agent");
            assert_eq!(status, AgentStatus::Completed);
            assert_eq!(result, Some("task done".into()));
        }
        _ => panic!("unexpected message type"),
    }

    dispatch.shutdown().await;
    let _ = handle.await;
}
```

> 注意：`TokioDispatch` 需要实现 `Clone`（内部用 Arc 包装），或者提供 `clone_arc()` 方法。

**Step 2: 为 TokioDispatch 添加 Clone 支持**

在 `bus.rs` 中：

```rust
impl Clone for TokioDispatch {
    fn clone(&self) -> Self {
        Self {
            queue_tx: self.queue_tx.clone(),
            queue_rx: self.queue_rx.clone(),
            shutdown_tx: self.shutdown_tx.clone(),
            shutdown_rx: self.shutdown_rx.clone(),
        }
    }
}
```

**Step 3: 运行测试**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo test -p brain-dispatch`
Expected: PASS

**Step 4: 运行全量测试确保无回归**

Run: `cd /Users/chenh/RustObject/claw-code-parity/rust && cargo test --workspace --exclude brain-integration-tests`
Expected: 所有测试通过

**Step 5: Commit**

```bash
git add rust/crates/brain-dispatch/
git commit -m "test(dispatch): add dispatch lifecycle test and verify no regressions"
```

---

## Task 9: 副脑异步任务编排 — 评估脑事件驱动

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs` — 评估闭环改为事件驱动

**目标**：评估脑完成评估后，通过 dispatch 注入通知，触发主脑修订（而非当前的同步等待循环）。

这个任务依赖 Task 4-7 的基础设施工作。具体改动是：

1. 评估脑 `evaluate_with_verification()` 完成后，调用 `dispatch.inject(BrainTaskCompleted, Priority::Background)`
2. dispatch_loop 将通知转发到 `MainLoopMessage::BrainTaskNotification`
3. orchestrator 在主循环中消费通知，如果评估不通过，触发主脑修订

此任务是 P1 优先级，在 P0（同步/异步子代理）验证通过后再实施。

---

## 执行顺序与依赖关系

```
Task 1 (骨架) ──────────────────────────────────────┐
                                                     │
Task 2 (同步子代理) ── 无依赖，可独立进行 ──────────┤
                                                     │
Task 3 (异步子代理) ── 依赖 Task 2 ─────────────────┤
                                                     │
Task 4 (集成 orchestrator) ── 依赖 Task 1 + 3 ──────┤
                                                     │
Task 5 (跨线程桥接) ── 依赖 Task 4 ─────────────────┤
                                                     │
Task 6 (dispatch_loop) ── 依赖 Task 1 ──────────────┤
                                                     │
Task 7 (TUI 集成) ── 依赖 Task 4 + 6 ──────────────┤
                                                     │
Task 8 (测试) ── 依赖 Task 6 + 7 ──────────────────┤
                                                     │
Task 9 (评估脑编排) ── 依赖 Task 8, P1 暂缓 ────────┘
```

**建议先做**: Task 1 → Task 2 → Task 6 → Task 3 → Task 4 → Task 5 → Task 7 → Task 8 → Task 9
