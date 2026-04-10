# AI Brain Agent 接口规约

> 日期：2026-04-03
> 状态：待审批
> 作用：各 crate 的开发契约，对着这份文档开发，不用互相看源码

---

## 一、Crate 依赖关系

```
brain-core (基础类型 + trait)
    ↑
brain-bus (消息总线，依赖 brain-core 的类型)
    ↑
brain-sensory / brain-master / brain-memory / brain-reasoning / brain-motor / brain-validation / brain-evaluation
    (各副脑，依赖 brain-core + brain-bus)
    ↑
ai-brain-cli (CLI 入口，依赖所有副脑)
```

**禁止**：副脑之间互相依赖。所有跨脑通信通过 brain-bus 的三通道完成。

---

## 二、brain-core — 公共类型定义

### 2.1 基础类型 (`types.rs`)

```rust
use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc};

/// 副脑唯一标识
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrainId(pub String);

impl BrainId {
    pub fn sensory() -> Self { Self("sensory".into()) }
    pub fn master() -> Self { Self("master".into()) }
    pub fn reasoning() -> Self { Self("reasoning".into()) }
    pub fn memory() -> Self { Self("memory".into()) }
    pub fn motor() -> Self { Self("motor".into()) }
    pub fn validation() -> Self { Self("validation".into()) }
    pub fn evaluation() -> Self { Self("evaluation".into()) }
}

/// 副脑种类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrainKind {
    Sensory,
    Master,
    Reasoning,
    Memory,
    Motor,
    Validation,
    Evaluation,
}

/// 权重 [0.1, 1.0]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Weight(pub f64);

impl Weight {
    pub fn default_value() -> Self { Self(0.5) }

    pub fn strengthen(&mut self, delta: f64) {
        self.0 = (self.0 + delta).min(1.0).max(0.1);
    }

    pub fn weaken(&mut self, delta: f64) {
        self.0 = (self.0 - delta).min(1.0).max(0.1);
    }
}

/// 消息优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessagePriority {
    High,
    Normal,
    Low,
}

/// 环境上下文（感知脑注入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainContext {
    pub current_date: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub platform: String,
}

/// 知识来源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KnowledgeSource {
    WebSearch { url: String },
    Memory { memory_id: String, layer: MemoryLayer },
    LlmReasoning { model: String },
    UserConfirmation,
    OtherBrain { brain_id: BrainId },
}

/// 记忆层级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryLayer {
    TaskSummary,  // L0
    EventIndex,   // L1
    ShortTerm,    // L2
    Raw,          // L3
}

/// 快思考结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastThinkResult {
    pub relevant: bool,
    pub confidence: f64,               // [0.0, 1.0]
    pub summary: Option<String>,       // 一句话结论
    pub suggested_tools: Vec<String>,  // 建议使用的工具
    pub matched_experience: Option<String>, // 命中的经验 ID
}

/// 慢思考结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowThinkResult {
    pub conclusion: String,
    pub reasoning_path: Vec<String>,
    pub confidence: f64,
    pub sources: Vec<KnowledgeSource>,
    pub new_experience: Option<NewExperience>,
}

/// 新经验（慢思考产出，待写入记忆脑）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewExperience {
    pub trigger_pattern: String,
    pub reasoning_path: Vec<String>,
    pub tools_used: Vec<String>,
}

/// 思考上下文（慢思考时注入的额外信息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkContext {
    pub related_memories: Vec<MemoryEntry>,
    pub task_history: Vec<String>,
}

/// 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub tags: Vec<String>,
    pub layer: MemoryLayer,
    pub importance: f64,               // 召回优先级（不影响存储）
    pub source: KnowledgeSource,
    pub confidence: f64,
    pub reference_count: u32,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub consolidated: bool,
}

/// 协作消息类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CollaborationKind {
    Request,
    Response,
    Dispatch,
}
```

### 2.2 BrainAgent Trait (`agent.rs`)

```rust
use std::future::Future;
use std::pin::Pin;
use crate::types::*;

/// 副脑必须实现的 trait
#[allow(async_fn_in_trait)]
pub trait BrainAgent: Send + Sync {
    /// 副脑唯一标识
    fn id(&self) -> &BrainId;

    /// 副脑种类
    fn kind(&self) -> BrainKind;

    /// 快思考 — 本地规则/经验匹配，不调 LLM
    /// 延迟目标: ~10ms
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult;

    /// 慢思考 — 需要 LLM 调用
    /// 延迟目标: ~1-5s
    fn slow_think(
        &self,
        msg: &BroadcastMessage,
        context: &ThinkContext,
    ) -> Pin<Box<dyn Future<Output = SlowThinkResult> + Send + '_>>;

    /// 接收广播消息（通道1）
    /// 各副脑内部决定是否响应
    fn on_broadcast(&mut self, msg: BroadcastMessage);

    /// 接收协作消息（通道2）
    fn on_collaboration(&mut self, msg: CollaborationMessage);

    /// 启动时初始化
    fn on_activate(&mut self) {}

    /// 关闭时清理
    fn on_deactivate(&mut self) {}
}

/// 不参与快/慢思考循环的副脑（评估脑）
pub trait StatelessBrain: Send + Sync {
    fn id(&self) -> &BrainId;
    fn kind(&self) -> BrainKind;

    /// 执行评估，返回结果后自毁
    fn evaluate(&self, snapshots: Vec<ContextSnapshot>) -> EvaluationResult;
}
```

### 2.3 配置类型 (`config.rs`)

```rust
use serde::{Serialize, Deserialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainConfig {
    pub brain: BrainSection,
    pub python: PythonSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainSection {
    pub model_fast: String,           // 感知脑模型，默认 "haiku"
    pub model_slow: String,           // 慢思考模型，默认 "sonnet"
    pub memory_dir: PathBuf,          // 默认 "~/.ai-brain/memory"
    pub thresholds: ThresholdConfig,
    pub weights: WeightConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConfig {
    pub fast_think_confidence: f64,      // 快思考置信度阈值，默认 0.7
    pub consolidation_importance: f64,   // 巩固重要性阈值，默认 0.7
    pub memory_recall_min_importance: f64, // 召回最低 importance，默认 0.2
    pub context_warning_threshold: f64,  // 上下文警告阈值，默认 0.7
    pub context_danger_threshold: f64,   // 上下文危险阈值，默认 0.85
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightConfig {
    pub reasoning: f64,    // 默认 0.5
    pub memory: f64,       // 默认 0.5
    pub motor: f64,        // 默认 0.5
    pub validation: f64,   // 默认 0.5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonSection {
    pub mcp_command: String,        // Python MCP Server 启动命令
    pub mcp_args: Vec<String>,      // 启动参数
}
```

---

## 三、brain-bus — 三通道消息总线

### 3.1 消息类型

```rust
use brain_core::types::*;

/// 广播消息（通道1）— 感知脑 → 所有副脑
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub content: String,              // 感知脑 LLM 解析后的自然语言描述
    pub raw_input: String,            // 原始用户输入
    pub context: BrainContext,         // 环境上下文
    pub timestamp: DateTime<Utc>,
}

/// 协作消息（通道2）— 副脑间点对点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationMessage {
    pub id: String,
    pub from: BrainId,
    pub to: Vec<BrainId>,
    pub correlation_id: Option<String>,  // 请求-响应配对 ID
    pub hop_count: u32,                  // 防循环（>3 丢弃）
    pub priority: MessagePriority,
    pub content: String,
    pub kind: CollaborationKind,
}

/// 副脑响应（通道3）— 副脑 → 主脑
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainResponse {
    pub from: BrainId,
    pub relevance: f64,               // 相关度 [0, 1]
    pub confidence: f64,              // 置信度 [0, 1]
    pub result: BrainResponsePayload,
    pub need_slow_think: bool,
    pub timestamp: DateTime<Utc>,
}

/// 响应载荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrainResponsePayload {
    FastThink(FastThinkResult),
    SlowThink(SlowThinkResult),
    SafetyCheck(SafetyCheckResult),
    TruthfulnessCheck(TruthfulnessResult),
    MemoryRecall(Vec<MemoryEntry>),
    ToolResult(ToolExecutionResult),
    Evaluation(EvaluationResult),
    NotRelevant { reason: String },
}
```

### 3.2 Bus 接口

```rust
use brain_core::types::*;

pub struct BrainBus {
    // 内部实现: tokio broadcast + mpsc
}

impl BrainBus {
    /// 创建消息总线
    /// - broadcast_capacity: 广播通道容量（默认 256）
    /// - collaboration_capacity: 协作通道容量（默认 1024）
    /// - result_capacity: 结果通道容量（默认 256）
    pub fn new(broadcast_capacity: usize, collaboration_capacity: usize, result_capacity: usize) -> Self;

    // --- 通道1: 广播 ---

    /// 感知脑投递广播
    pub fn broadcast(&self, msg: BroadcastMessage) -> Result<(), BusError>;

    /// 订阅广播通道（每个副脑调用一次）
    pub fn subscribe_broadcast(&self) -> BroadcastReceiver;

    // --- 通道2: 协作 ---

    /// 发送协作消息
    pub fn send_collaboration(&self, msg: CollaborationMessage) -> Result<(), BusError>;

    /// 订阅协作通道（按 BrainId 过滤，只收到发给自己或广播的）
    pub fn subscribe_collaboration(&self, brain_id: BrainId) -> CollaborationReceiver;

    // --- 通道3: 结果 ---

    /// 副脑提交结果
    pub fn submit_result(&self, response: BrainResponse) -> Result<(), BusError>;

    /// 主脑订阅结果通道
    pub fn subscribe_results(&self) -> ResultReceiver;

    // --- 生命周期 ---

    /// 关闭所有通道
    pub fn shutdown(&self);
}

/// 广播接收端
pub struct BroadcastReceiver { /* tokio broadcast::Receiver */ }
impl BroadcastReceiver {
    pub async fn recv(&mut self) -> Result<BroadcastMessage, BusError>;
}

/// 协作接收端（已按 BrainId 过滤）
pub struct CollaborationReceiver { /* 内部实现 */ }
impl CollaborationReceiver {
    pub async fn recv(&mut self) -> Result<CollaborationMessage, BusError>;
}

/// 结果接收端
pub struct ResultReceiver { /* tokio mpsc::Receiver */ }
impl ResultReceiver {
    pub async fn recv(&mut self) -> Option<BrainResponse>;
}

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("channel closed")]
    ChannelClosed,
    #[error("channel full")]
    ChannelFull,
    #[error("hop count exceeded: {hop_count}")]
    HopCountExceeded { hop_count: u32 },
}
```

---

## 四、各副脑公开接口

### 4.1 感知脑 (`brain-sensory`)

```rust
pub struct SensoryBrain {
    /* 内部字段 */
}

impl SensoryBrain {
    /// 创建感知脑
    /// - llm_model: 用于解析的模型名（建议 haiku）
    /// - bus: 共享的消息总线
    pub fn new(config: &BrainConfig, bus: BrainBus) -> Result<Self, SensoryError>;

    /// 接收外部输入，LLM 解析后投递广播
    /// 这是系统的唯一入口
    ///
    /// 流程:
    ///   1. 接收 raw_input
    ///   2. 构建环境上下文 (date, cwd, git_branch)
    ///   3. 调用 LLM (haiku) 生成自然语言描述
    ///   4. 投递到广播通道
    ///   5. 降级: LLM 失败 → 直接投递原始输入 + 标记 "未解析"
    ///
    /// 返回: 解析后的描述（用于日志/展示）
    pub async fn process_input(&self, raw_input: &str) -> Result<String, SensoryError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SensoryError {
    #[error("LLM call failed: {0}")]
    LlmFailed(String),
    #[error("broadcast send failed: {0}")]
    BroadcastFailed(BusError),
}
```

### 4.2 主脑 (`brain-master`)

```rust
pub struct MasterBrain {
    /* 内部字段 */
}

impl MasterBrain {
    pub fn new(config: &BrainConfig, bus: BrainBus) -> Result<Self, MasterError>;

    /// 主循环 — 阻塞运行，处理一个完整任务
    ///
    /// 流程:
    ///   1. 监听通道1，记录任务上下文
    ///   2. 等待通道3的快思考结果（设超时 5s）
    ///   3. 权重排序 + 决策
    ///   4. 如需慢思考 → 通道2发送调度指令
    ///   5. 等待慢思考结果
    ///   6. 调度校验脑审核
    ///   7. 输出最终响应
    ///   8. 更新权重 + 记录经验
    pub async fn run_loop(&mut self) -> Result<MasterOutput, MasterError>;

    /// 触发空闲任务（外部定时器调用）
    ///
    /// 空闲任务:
    ///   - 触发记忆巩固
    ///   - 触发评估脑
    pub async fn trigger_idle_tasks(&mut self) -> Result<(), MasterError>;

    /// 获取所有副脑的权重
    pub fn get_weights(&self) -> &HashMap<BrainId, Weight>;

    /// 获取当前任务上下文
    pub fn current_task(&self) -> Option<&TaskContext>;
}

/// 主脑输出给用户/系统的最终结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterOutput {
    pub answer: String,                      // 最终回答
    pub confidence: f64,                      // 总体可信度
    pub sources: Vec<KnowledgeSource>,        // 信息来源列表
    pub participating_brains: Vec<BrainId>,   // 参与的副脑
    pub usage: TurnUsage,                     // Token 用量
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnUsage {
    pub total_tokens: u64,
    pub llm_calls: u32,
    pub duration_ms: u64,
}

/// 任务上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    pub input: BroadcastMessage,
    pub start_time: DateTime<Utc>,
    pub phase: TaskPhase,
    pub brain_responses: HashMap<BrainId, BrainResponse>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPhase {
    WaitingFastThink,
    DispatchingSlowThink,
    WaitingSlowThink,
    Validating,
    Finalizing,
}
```

### 4.3 记忆脑 (`brain-memory`)

```rust
pub struct MemoryBrain {
    /* 内部字段 */
}

impl MemoryBrain {
    pub fn new(config: &BrainConfig, bus: BrainBus) -> Result<Self, MemoryError>;

    // --- 写入接口 ---

    /// 存储一条记忆
    /// 始终写入 L3（原始层），同时写入 L2（短期层）
    pub async fn store(&mut self, entry: MemoryEntry) -> Result<String, MemoryError>;

    /// 批量存储
    pub async fn store_batch(&mut self, entries: Vec<MemoryEntry>) -> Result<Vec<String>, MemoryError>;

    // --- 召回接口 ---

    /// 快思考召回（纯关键词匹配，不调 LLM）
    ///
    /// 检索顺序: L0 → L1 → L2 → L3
    /// 召回规则:
    ///   - importance > 0.5 → 正常返回
    ///   - 0.2 < importance <= 0.5 → 需要 tags 精确匹配才返回
    ///   - importance <= 0.2 → 不返回
    pub fn recall_fast(&self, query: &RecallQuery) -> Vec<MemoryEntry>;

    /// 慢思考召回（候选 + LLM 精排）
    /// 先走 recall_fast 粗筛，再让 LLM 精排
    pub async fn recall_slow(&self, query: &RecallQuery) -> Vec<MemoryEntry>;

    // --- 巩固接口 ---

    /// 执行一次巩固周期
    /// 从 L3 提取任务级总结 → 写入 L0
    /// 从 L2 提炼事件索引 → 写入 L1
    pub async fn consolidate(&mut self) -> Result<ConsolidationReport, MemoryError>;

    /// 获取记忆统计
    pub fn stats(&self) -> MemoryStats;
}

/// 召回查询
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub keywords: Vec<String>,        // 关键词
    pub tags: Vec<String>,            // 标签过滤
    pub max_results: usize,           // 最大返回数量，默认 10
    pub min_importance: f64,          // 最低 importance，默认 0.2
    pub layers: Vec<MemoryLayer>,     // 搜索哪些层，默认 [L0, L1, L2]
}

/// 巩固报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationReport {
    pub task_summaries_created: u32,    // 新建 L0 任务总结数
    pub event_indexes_created: u32,     // 新建 L1 事件索引数
    pub memories_consolidated: u32,     // 从 L2 巩固到 L0 的记忆数
    pub duration_ms: u64,
}

/// 记忆统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub l0_count: u32,    // L0 任务总结数
    pub l1_count: u32,    // L1 事件索引数
    pub l2_count: u32,    // L2 短期记忆数
    pub l3_count: u32,    // L3 原始记忆数
    pub total_size_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("storage IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}
```

#### 记忆文件格式

**L3 原始记忆** (`~/.ai-brain/sessions/{session-id}.jsonl`):
```jsonl
{"type":"session_meta","id":"sess_001","created_at":"2026-04-03T10:00:00Z"}
{"type":"message","role":"user","content":"告诉我这个月有没有节假日？","timestamp":"2026-04-03T10:00:01Z"}
{"type":"message","role":"assistant","content":"2026年4月有清明节...","source":"WebSearch+记忆","confidence":0.95,"timestamp":"2026-04-03T10:00:05Z"}
```

**L2 短期记忆** (`~/.ai-brain/memory/short-term/recent.json`):
```json
{
  "entries": [
    {
      "id": "mem_001",
      "content": "2026年4月清明节4月4-6日放假3天",
      "tags": ["节假日", "清明节", "4月", "2026"],
      "layer": "ShortTerm",
      "importance": 0.85,
      "source": {"WebSearch": {"url": "https://..."}},
      "confidence": 0.95,
      "reference_count": 1,
      "created_at": "2026-04-03T10:00:05Z",
      "last_accessed": "2026-04-03T10:00:05Z",
      "consolidated": false
    }
  ]
}
```

**L1 事件索引** (`~/.ai-brain/memory/events/2026-04-03.json`):
```json
{
  "date": "2026-04-03",
  "events": [
    {
      "id": "evt_001",
      "summary": "用户查询了4月节假日，通过WebSearch确认清明节4月4-6日",
      "tags": ["节假日", "查询", "WebSearch"],
      "task_ids": ["task_001"],
      "created_at": "2026-04-03T10:00:05Z"
    }
  ],
  "preference_updates": []
}
```

**L0 任务总结** (`~/.ai-brain/memory/long-term/tasks/{task-id}.json`):
```json
{
  "id": "task_001",
  "task_description": "查询2026年4月法定节假日",
  "trigger_pattern": "当月节假日查询",
  "reasoning_path": ["记忆检索", "WebSearch", "交叉验证", "组装回答"],
  "mistakes": [],
  "files_modified": [],
  "tools_used": ["WebSearch"],
  "success_rate": 0.92,
  "shortcuts": ["下次直接从记忆返回，无需再搜索"],
  "created_at": "2026-04-03T10:00:05Z"
}
```

**关键词索引** (`~/.ai-brain/memory/index/tags.json`):
```json
{
  "节假日": ["mem_001", "task_001"],
  "清明节": ["mem_001"],
  "4月": ["mem_001", "evt_001"],
  "WebSearch": ["evt_001", "task_001"]
}
```

### 4.4 推理脑 (`brain-reasoning`)

```rust
pub struct ReasoningBrain {
    /* 内部字段 */
}

impl ReasoningBrain {
    pub fn new(config: &BrainConfig, bus: BrainBus) -> Result<Self, ReasoningError>;

    /// 从记忆脑加载经验库
    pub async fn load_experiences(&mut self, memory_brain: &MemoryBrain) -> Result<(), ReasoningError>;

    /// 快思考 — 经验库模式匹配
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult;

    /// 慢思考 — LLM 深度推理
    async fn slow_think(&self, msg: &BroadcastMessage, context: &ThinkContext) -> SlowThinkResult;
}

impl BrainAgent for ReasoningBrain {
    fn id(&self) -> &BrainId { &self.id }
    fn kind(&self) -> BrainKind { BrainKind::Reasoning }
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult { self.fast_think(msg) }
    fn slow_think(&self, msg: &BroadcastMessage, context: &ThinkContext) -> Pin<Box<...>> { ... }
    fn on_broadcast(&mut self, msg: BroadcastMessage) { ... }
    fn on_collaboration(&mut self, msg: CollaborationMessage) { ... }
}
```

### 4.5 执行脑 (`brain-motor`)

```rust
pub struct MotorBrain {
    /* 内部字段 */
}

impl MotorBrain {
    pub fn new(config: &BrainConfig, bus: BrainBus) -> Result<Self, MotorError>;

    /// 执行工具调用（只执行校验脑审核通过的操作）
    pub async fn execute_tool(&mut self, tool_call: &ToolCall) -> Result<ToolExecutionResult, MotorError>;

    /// 获取可用工具列表
    pub fn list_tools(&self) -> Vec<ToolDescriptor>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub validated: bool,                // 是否经过校验脑审核
    pub validation_id: Option<String>,  // 校验脑审核 ID
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub tool_name: String,
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
```

### 4.6 校验脑 (`brain-validation`)

```rust
pub struct ValidationBrain {
    /* 内部字段 */
}

impl ValidationBrain {
    pub fn new(config: &BrainConfig, bus: BrainBus) -> Result<Self, ValidationError>;

    /// 安全性校验 — 检查工具调用是否安全
    pub fn check_safety(&self, tool_call: &ToolCall) -> SafetyCheckResult;

    /// 真实性校验 — 追踪信息来源，评估可信度
    pub fn check_truthfulness(&self, claim: &str, sources: &[KnowledgeSource]) -> TruthfulnessResult;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyCheckResult {
    pub safe: bool,
    pub risk_level: RiskLevel,
    pub reason: Option<String>,
    pub requires_user_approval: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,       // 自动放行
    Medium,    // 需确认
    High,      // 必须用户授权
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthfulnessResult {
    pub confidence: f64,
    pub source_analysis: Vec<SourceAnalysis>,
    pub cross_verified: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAnalysis {
    pub source: KnowledgeSource,
    pub reliability: f64,
    pub supports_claim: bool,
}
```

### 4.7 评估脑 (`brain-evaluation`)

```rust
/// 评估脑 — 无状态，每次执行后自毁
pub struct EvaluationBrain {
    system_prompt: String,   // 固定规则，硬编码
}

impl EvaluationBrain {
    /// 创建新的评估脑实例（每次都是空白状态）
    pub fn new() -> Self;

    /// 执行评估，返回结果（调用后建议立即 drop）
    pub fn evaluate(&self, snapshots: Vec<ContextSnapshot>) -> EvaluationResult;
}

impl StatelessBrain for EvaluationBrain {
    fn id(&self) -> &BrainId { &BrainId::evaluation() }
    fn kind(&self) -> BrainKind { BrainKind::Evaluation }
    fn evaluate(&self, snapshots: Vec<ContextSnapshot>) -> EvaluationResult { self.evaluate(snapshots) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub brain_id: BrainId,
    pub token_usage: TokenUsage,
    pub message_count: usize,
    pub health_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub overall_health: f64,
    pub brain_reports: Vec<BrainHealthReport>,
    pub slim_instructions: Vec<SlimInstruction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainHealthReport {
    pub brain_id: BrainId,
    pub health_score: f64,          // [0, 1]
    pub usage_percent: f64,         // 上下文使用率
    pub redundancy_score: f64,      // 冗余度
    pub stale_score: f64,           // 过时率
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlimInstruction {
    Delete { message_ids: Vec<String>, reason: String },
    Compress { message_ids: Vec<String>, summary: String },
    Preserve { message_ids: Vec<String>, reason: String },
}
```

---

## 五、模块间交互时序

### 5.1 完整查询时序

```
用户输入 "这个月有节假日吗？"
         │
         │ process_input()
         ▼
    ┌──────────┐
    │  感知脑   │ LLM(haiku) 解析 → 自然语言描述
    └────┬─────┘
         │ broadcast(BroadcastMessage)
         ▼
    ┌──────────┐ 通道1广播
    │  Bus     │──────────────────┐──────────────┬──────────────┐
    └──────────┘                  │              │              │
                          ┌───────▼──────┐ ┌─────▼─────┐ ┌────▼─────┐
                          │   推理脑      │ │  记忆脑    │ │  校验脑   │
                          │ fast_think() │ │ recall_fast│ │check_safe│
                          └───────┬──────┘ └─────┬─────┘ └────┬─────┘
                                  │              │             │
                                  │ submit_result()            │
                                  ▼              ▼             ▼
                           ┌──────────────────────────────────────┐
                           │             通道3 结果                 │
                           └──────────────────┬───────────────────┘
                                              │
                                    ┌─────────▼─────────┐
                                    │      主脑           │
                                    │  权重排序 + 决策     │
                                    └────┬─────────┬─────┘
                                         │         │
                        需要慢思考          │         │ 需要工具执行
                                         │         │
                              ┌──────────▼──┐  ┌───▼──────────┐
                              │ send_collab │  │ send_collab  │
                              │ → 推理脑    │  │ → 执行脑      │
                              │ slow_think  │  │ execute_tool │
                              └──────┬──────┘  └───┬──────────┘
                                     │             │
                                     │ submit_result()
                                     ▼             ▼
                              ┌──────────────────────────────┐
                              │       主脑汇总 + 校验脑审核     │
                              └──────────────┬───────────────┘
                                             │
                                             ▼
                                      MasterOutput → 用户
```

### 5.2 记忆巩固时序（空闲触发）

```
主脑 trigger_idle_tasks()
         │
         ├──→ 记忆脑.consolidate()
         │         │
         │         ├── 从 L3 提取任务级总结 → 写入 L0
         │         ├── 从 L2 提炼事件索引 → 写入 L1
         │         ├── 更新 tags.json 索引
         │         └── 返回 ConsolidationReport
         │
         └──→ EvaluationBrain::new()
                   │
                   ├── 收集各副脑 ContextSnapshot
                   ├── evaluate() → EvaluationResult
                   ├── 返回 SlimInstruction[] 给主脑
                   └── drop (自毁)
```

---

## 六、集成测试场景

### 6.1 最小闭环测试

```rust
#[tokio::test]
async fn test_minimal_loop() {
    // 1. 创建 Bus
    let bus = BrainBus::new(256, 1024, 256);

    // 2. 创建感知脑 + StubBrain + 主脑
    let sensory = SensoryBrain::new(&config, bus.clone());
    let stub = StubBrain::new(BrainId::reasoning(), bus.clone());
    let master = MasterBrain::new(&config, bus.clone());

    // 3. 感知脑投递
    let result = sensory.process_input("测试输入").await.unwrap();

    // 4. StubBrain 快思考
    let broadcast = stub.recv_broadcast().await.unwrap();
    let fast = stub.fast_think(&broadcast);
    stub.submit_result(fast).await;

    // 5. 主脑汇总
    let output = master.run_loop().await.unwrap();

    assert!(output.answer.len() > 0);
}
```

### 6.2 记忆存取测试

```rust
#[tokio::test]
async fn test_memory_store_and_recall() {
    let mut memory = MemoryBrain::new(&config, bus.clone());

    // 存入
    let entry = MemoryEntry {
        id: "mem_test_001".into(),
        content: "2026年4月清明节4月4-6日放假3天".into(),
        tags: vec!["节假日".into(), "清明节".into()],
        importance: 0.85,
        ..Default::default()
    };
    memory.store(entry).await.unwrap();

    // 快思考召回
    let results = memory.recall_fast(&RecallQuery {
        keywords: vec!["节假日".into()],
        max_results: 10,
        ..Default::default()
    });
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "2026年4月清明节4月4-6日放假3天");
}
```

### 6.3 衰减不影响存储测试

```rust
#[tokio::test]
async fn test_decay_does_not_delete() {
    let mut memory = MemoryBrain::new(&config, bus.clone());

    // 存入 importance = 0.3 的记忆
    let entry = MemoryEntry {
        id: "mem_low".into(),
        importance: 0.3,
        ..make_test_entry()
    };
    memory.store(entry).await.unwrap();

    // 模拟衰减后 importance < 0.2
    // 衰减只影响召回，不删数据
    let results = memory.recall_fast(&RecallQuery {
        keywords: vec![],
        min_importance: 0.2,
        ..Default::default()
    });
    // 召回不到
    assert_eq!(results.len(), 0);

    // 但 L3 原始文件仍然存在
    assert!(memory.has_raw_entry("mem_low"));
}
```

### 6.4 校验脑可信度测试

```rust
#[test]
fn test_truthfulness_pure_memory() {
    let validation = ValidationBrain::new(&config, bus.clone());

    let result = validation.check_truthfulness(
        "清明节4月4日",
        &[KnowledgeSource::Memory { memory_id: "mem_001".into(), layer: MemoryLayer::L2 }],
    );

    // 纯记忆无验证 → 可信度 0.5 → 必须标注警告
    assert_eq!(result.confidence, 0.5);
    assert!(result.warning.is_some());
}

#[test]
fn test_truthfulness_cross_verified() {
    let validation = ValidationBrain::new(&config, bus.clone());

    let result = validation.check_truthfulness(
        "清明节4月4-6日放假3天",
        &[
            KnowledgeSource::WebSearch { url: "https://...".into() },
            KnowledgeSource::Memory { memory_id: "mem_001".into(), layer: MemoryLayer::L2 },
        ],
    );

    // WebSearch + 记忆交叉验证 → 0.95
    assert!(result.confidence >= 0.9);
    assert!(result.cross_verified);
    assert!(result.warning.is_none());
}
```

---

## 七、错误处理规约

### 7.1 各 crate 错误类型

每个 crate 使用 `thiserror` 定义自己的错误类型，不跨 crate 传递底层错误：

```rust
// 每个 crate 的错误都遵循这个模式
#[derive(Debug, thiserror::Error)]
pub enum XxxError {
    #[error("...")]
    ConfigError(String),
    #[error("bus error: {0}")]
    BusError(#[from] BusError),
    #[error("...")]
    InternalError(String),
}
```

### 7.2 降级策略

| 场景 | 降级行为 |
|------|---------|
| 感知脑 LLM 失败 | 直接投递原始输入，标记"未解析" |
| 记忆脑不可用 | 主脑跳过记忆，纯 LLM 推理 |
| 校验脑不可用 | 中高风险操作默认拦截（安全优先） |
| 评估脑不可用 | 跳过评估，不影响主流程 |
| Python 层不可用 | 仅快思考可用，慢思考降级为错误 |
| 单个副脑超时 5s | 主脑跳过该副脑结果 |
