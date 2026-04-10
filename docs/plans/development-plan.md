# AI Brain Agent 开发计划

> 日期：2026-04-03
> 状态：待审批
> 基于：`docs/architecture/ai-brain-agent-design.md`

---

## 一、现有代码库可复用资产

| 现有模块 | 行数 | 复用方向 |
|---------|------|---------|
| `session.rs` | 1239 | Memory Brain 工作记忆（Layer 1） |
| `compact.rs` | 689 | Memory Brain 记忆巩固 |
| `mcp_stdio.rs` | 2406 | Rust-Python 通信（JSON-RPC over stdio） |
| `api/client.rs` | — | 慢思考引擎（多 Provider LLM 调用） |
| `conversation.rs` | 1679 | Sub-Brain 生命周期（Agent 循环框架） |
| `config.rs` | 1532 | Brain 配置层级（复用 ConfigLoader） |
| `permissions.rs` | 675 | 校验脑安全规则基础 |
| `hooks.rs` | 987 | 前后置钩子 → 执行脑安全拦截 |
| `prompt.rs` | 795 | 各副脑 System Prompt 构建 |

---

## 二、新增 Crate 结构

在现有 workspace 下新增以下 crate：

```
rust/crates/
├── brain-bus/          # 三通道消息总线
│   └── src/lib.rs      # BroadcastChannel, CollaborationChannel, ResultChannel
│
├── brain-core/         # Agent trait + 公共类型
│   └── src/
│       ├── lib.rs
│       ├── agent.rs    # BrainAgent trait (快思考/慢思考/生命周期)
│       ├── types.rs    # BrainId, BrainMessage, BrainResponse, Weight
│       ├── config.rs   # BrainConfig, BrainRegistry
│       └── experience.rs # 经验存储结构
│
├── brain-sensory/      # 感知脑（唯一入口）
│   └── src/lib.rs      # LLM 解析 + 环境上下文注入 + 投递广播
│
├── brain-master/       # 主脑（裁判 + 调度）
│   └── src/
│       ├── lib.rs
│       ├── orchestrator.rs  # 汇总/排序/调度
│       ├── weight_engine.rs # 权重进化
│       └── idle_tasks.rs    # 空闲任务（记忆巩固/评估触发）
│
├── brain-memory/       # 记忆脑（海马体）
│   └── src/
│       ├── lib.rs
│       ├── working.rs      # Layer 1: 工作记忆（复用 session.rs）
│       ├── short_term.rs   # Layer 2: 短期记忆
│       ├── long_term.rs    # Layer 3: 长期记忆 + 经验路径
│       ├── consolidation.rs # 巩固 + 遗忘曲线
│       └── recall.rs       # 多层检索策略
│
├── brain-reasoning/    # 推理脑（前额叶）
│   └── src/
│       ├── lib.rs
│       ├── experience.rs   # 经验路径库
│       └── reasoning.rs    # 快/慢思考推理引擎
│
├── brain-motor/        # 执行脑（运动皮层）
│   └── src/
│       ├── lib.rs
│       └── tool_registry.rs # 工具注册表
│
├── brain-validation/   # 校验脑（ACC + 基底节）
│   └── src/
│       ├── lib.rs
│       ├── safety.rs       # 安全性校验
│       └── truthfulness.rs # 真实性校验（来源可信度）
│
├── brain-evaluation/   # 评估脑（脑干反射）
│   └── src/
│       ├── lib.rs
│       └── context_health.rs # 上下文健康度评估
│
└── ai-brain-cli/      # 新 CLI 入口（替代 rusty-claude-cli）
    └── src/
        ├── main.rs
        ├── app.rs
        └── api_server.rs   # HTTP API (axum)
```

---

## 三、分阶段开发计划

### Phase 1: 基础设施层（预计 3-5 天）

> **目标**：三通道消息总线 + BrainAgent trait + 基础类型 + 配置系统

#### Task 1.1: 创建 `brain-core` crate

**文件**: `rust/crates/brain-core/src/`

```rust
// types.rs — 公共类型
pub struct BrainId(String);           // 副脑唯一标识
pub enum BrainKind {
    Sensory, Master, Reasoning, Memory,
    Motor, Validation, Evaluation,
}
pub struct Weight(f64);               // 权重 [0.1, 1.0]
pub enum MessagePriority { High, Normal, Low }

// agent.rs — BrainAgent trait
pub trait BrainAgent: Send + Sync {
    fn id(&self) -> &BrainId;
    fn kind(&self) -> BrainKind;

    // 快思考 — 本地规则，不调 LLM
    fn fast_think(&self, msg: &BrainMessage) -> FastThinkResult;

    // 慢思考 — 需要 LLM，返回 Future
    fn slow_think(&self, msg: &BrainMessage, context: &ThinkContext) -> Pin<Box<dyn Future<Output = SlowThinkResult>>>;

    // 生命周期
    fn on_activate(&mut self) {}
    fn on_deactivate(&mut self) {}
    fn on_message(&mut self, msg: BrainMessage);  // 接收广播消息
}
```

**依赖**: 无外部依赖，纯类型定义

#### Task 1.2: 创建 `brain-bus` crate — 三通道消息总线

**文件**: `rust/crates/brain-bus/src/lib.rs`

```rust
// 基于 tokio broadcast + mpsc 实现

pub struct BrainBus {
    // 通道1: 广播通道 — 感知脑 → 所有副脑
    broadcast_tx: tokio::sync::broadcast::Sender<BroadcastMessage>,

    // 通道2: 协作通道 — 副脑间点对点/点对多
    collaboration_tx: tokio::sync::mpsc::Sender<CollaborationMessage>,

    // 通道3: 结果通道 — 副脑 → 主脑
    result_tx: tokio::sync::mpsc::Sender<BrainResponse>,
}

// 广播消息（通道1）
pub struct BroadcastMessage {
    pub content: String,          // 感知脑解析后的自然语言描述
    pub raw_input: String,        // 原始用户输入
    pub context: BrainContext,     // 环境上下文（日期、目录、分支等）
    pub timestamp: DateTime<Utc>,
}

// 协作消息（通道2）
pub struct CollaborationMessage {
    pub id: String,
    pub from: BrainId,
    pub to: Vec<BrainId>,
    pub correlation_id: Option<String>,  // 请求-响应配对
    pub hop_count: u32,                  // 防循环（>3 丢弃）
    pub priority: MessagePriority,
    pub content: String,
    pub kind: CollaborationKind,        // Request / Response / Dispatch
}

// 副脑响应（通道3）
pub struct BrainResponse {
    pub from: BrainId,
    pub relevance: f64,           // 相关度 [0, 1]
    pub confidence: f64,          // 置信度 [0, 1]
    pub result: ThinkResult,      // 快思考/慢思考结果
    pub need_slow_think: bool,    // 是否需要慢思考
    pub timestamp: DateTime<Utc>,
}
```

**依赖**: `tokio` (broadcast + mpsc channels)

#### Task 1.3: 创建 `brain-core` 配置系统

复用 `config.rs` 的 `ConfigLoader` 模式，新增 Brain 配置：

```toml
# ~/.ai-brain/config.toml
[brain]
model_fast = "haiku"          # 感知脑使用 haiku
model_slow = "sonnet"         # 慢思考使用 sonnet
memory_dir = "~/.ai-brain/memory"

[brain.weights]
reasoning = 0.5
memory = 0.5
motor = 0.5
validation = 0.5

[brain.thresholds]
fast_think_confidence = 0.7   # 快思考置信度阈值
consolidation_importance = 0.7 # 记忆巩固重要性阈值
memory_decay_rate = 0.1        # 记忆衰减速率/天

[python]
endpoint = "http://localhost:8765"  # Python AI 层地址
```

---

### Phase 2: 感知脑 + 主脑骨架（预计 3-4 天）

> **目标**：系统最小可用闭环 — 感知脑接收输入 → 广播 → 主脑汇总 → 输出

#### Task 2.1: 感知脑 (`brain-sensory`)

- 接收外部输入（CLI / API）
- 调用 LLM (haiku) 解析为自然语言描述
- 注入环境上下文（日期、目录、git 分支）
- 投递到广播通道（通道1）
- 降级策略：LLM 失败时直接投递原始输入

```rust
pub struct SensoryBrain {
    id: BrainId,
    llm_client: ProviderClient,    // 复用 api crate
    bus: BrainBus,
    system_prompt: String,         // 感知脑 prompt（固定简短）
}
```

#### Task 2.2: 主脑骨架 (`brain-master`)

- 监听通道1（广播）— 记录任务上下文
- 收集通道3（结果）— 权重排序
- 只读监听通道2（协作）— 知道副脑在做什么
- 调度慢思考 — 通过通道2发送调度指令
- 输出最终响应给用户

```rust
pub struct MasterBrain {
    id: BrainId,
    weights: HashMap<BrainId, Weight>,  // 副脑权重
    bus: BrainBus,
    current_task: Option<TaskContext>,
    weight_engine: WeightEngine,
    idle_scheduler: IdleScheduler,
}
```

#### Task 2.3: 最小闭环集成测试

用 stub 副脑验证三通道消息流转：
```
输入 → 感知脑 → 通道1广播 → StubBrain快思考 → 通道3结果 → 主脑汇总 → 输出
```

---

### Phase 3: 记忆脑 — 总结金字塔 + 永久原始层（预计 5-7 天）

> **目标**：纯文件存储、永不删除原始记忆、总结金字塔叠在原始层之上
>
> **核心原则**：
> - 所有原始记忆永久保留（纯文件），永不删除
> - 衰减影响的是"召回行为"，不是"物理删除"
> - 不使用向量数据库，纯文件 + 关键词索引
> - 空闲时触发任务级深度巩固

#### Task 3.1: L3 — 原始记忆层（永久保留）

- **复用** `session.rs` 的 `Session` 结构体
- 适配为 Brain 内部使用：消息存储 + JSONL 持久化
- 路径：`~/.ai-brain/sessions/{session-id}.jsonl`
- Token 超限时调用 `compact.rs` 生成摘要，但**原始文件始终保留**
- 所有副脑产生的信息始终写入此层

#### Task 3.2: L2 — 短期记忆层

新建 `short_term.rs`：
- 存储：最近 N 条对话的关键信息、用户偏好、上下文片段
- 索引：关键词索引（HashMap + 文件系统）
- 召回优先级：`importance` 控制召回行为（不删除）
- 持久化：`~/.ai-brain/memory/short-term/recent.json`

```rust
pub struct ShortTermMemory {
    entries: HashMap<String, MemoryEntry>,
    index: HashMap<String, HashSet<String>>,  // tag → entry_ids
    config: ShortTermConfig,
    storage_path: PathBuf,
}

pub struct MemoryEntry {
    id: String,
    content: String,
    tags: Vec<String>,
    importance: f64,             // 召回优先级（不影响存储）
    source: KnowledgeSource,
    confidence: f64,
    reference_count: u32,
    created_at: DateTime<Utc>,
    last_accessed: DateTime<Utc>,
    consolidated: bool,
}
```

#### Task 3.3: L1 — 事件索引层

新建 `event_index.rs`：
- 按时间段（每日/每周）对 L2 做摘要
- 关键事件记录：做了什么、为什么、改了什么
- 用户偏好和行为模式更新
- 持久化：`~/.ai-brain/memory/short-term/events/{date}.json`
- 索引：关键词 HashMap

#### Task 3.4: L0 — 任务总结层（深度巩固产出）

新建 `task_summary.rs`：
- 一个完整任务的端到端总结：
  - 任务目标 + 最终结果
  - 完整思考路径（关键决策点）
  - 犯过的错 + 原因 + 如何避免
  - 修改的文件列表 + 修改原因
  - 下次同类任务的快捷路径建议
- 持久化：`~/.ai-brain/memory/long-term/tasks/{task-id}.json`
- 经验写入推理脑的经验库

```rust
pub struct TaskSummary {
    id: String,
    task_description: String,
    trigger_pattern: String,        // 用于快思考匹配
    reasoning_path: Vec<String>,    // 思考路径
    mistakes: Vec<MistakeEntry>,    // 犯过的错
    files_modified: Vec<String>,    // 修改的文件
    tools_used: Vec<String>,        // 使用的工具
    success_rate: f64,
    shortcuts: Vec<String>,         // 下次快捷路径建议
}
```

#### Task 3.5: 召回策略（衰减影响召回，不删数据）

新建 `recall.rs`：
- **读取路径**：L0 经验匹配 → L1 事件匹配 → L2 关键词匹配 → L3 原始回溯
- 快思考：纯关键词/模式匹配（不调 LLM，~10ms）
- 慢思考：候选记忆 + 问题发给 LLM 判断（~1s）
- **召回优先级**（importance 控制行为，不控制存储）：
  - `importance > 0.5` → 正常召回
  - `0.2 < importance <= 0.5` → 需要强关键词线索才召回
  - `importance <= 0.2` → 不主动召回，但数据仍在 L3

#### Task 3.6: 巩固引擎（空闲时执行）

新建 `consolidation.rs`：
- 空闲触发：从 L3 原始记忆中提取任务级总结 → 写入 L0
- 从 L2 短期记忆中提炼事件索引 → 写入 L1
- 总结内容：思考路径、犯错原因、如何避免、改了哪些文件
- 物理删除：仅在磁盘空间不足时，清理 `importance < 0.1` 的记忆

---

### Phase 4: 推理脑（预计 3-4 天）

> **目标**：经验路径库 + 快/慢思考推理引擎

#### Task 4.1: 经验路径库

- 存储：`trigger_pattern → reasoning_path → success_rate`
- 快思考：模式匹配（输入与历史经验相似度）
- 命中且 `success_rate > 阈值` → 直接复用路径
- 未命中 → 慢思考生成新路径

#### Task 4.2: 推理引擎

- 快思考：经验库匹配 + 模式识别
- 慢思考：调用 LLM 做深度推理
- 新经验写入记忆脑（通过通道2协作）
- 失败路径标记为反面案例

---

### Phase 5: 执行脑 + 校验脑（预计 3-4 天）

> **目标**：工具调用 + 安全校验 + 真实性校验

#### Task 5.1: 执行脑

- **复用** `tools` crate 的工具实现
- **复用** `runtime` 的 `ToolExecutor` trait
- 新增工具注册表：记录每个工具的能力和适用场景
- 只执行经过校验脑审核通过的操作

#### Task 5.2: 校验脑 — 安全性校验

- **扩展** `permissions.rs` 的高危行为规则库
- 安全等级：低风险(自动放行) / 中风险(需确认) / 高风险(必须用户授权)
- 所有执行脑的操作必须经过校验

#### Task 5.3: 校验脑 — 真实性校验

- 信息来源追踪（WebSearch / 记忆 / LLM推理）
- 来源可信度评分表（复用文档中的评分规则）
- 交叉验证多个来源的一致性
- 可信度 < 0.60 必须标注警告

---

### Phase 6: 评估脑 + 权重进化（预计 2-3 天）

> **目标**：上下文健康度评估 + 自毁式无状态副脑 + 权重进化机制

#### Task 6.1: 评估脑

- 无状态：每次启动都是空白，只加载固定 system prompt
- 接收其他副脑的上下文快照
- 按固定规则评估健康度
- 输出瘦身指令 → 自毁清空上下文
- 触发条件：空闲5分钟 / 上下文>80% / 任务完成后

#### Task 6.2: 权重进化引擎

- 记录每次任务的副脑表现 `{task_type, agent, relevance, quality}`
- 调整规则：相关且质量高 +0.1 / 不相关 -0.05
- 权重范围 [0.1, 1.0]
- 赫布定律："一起激发的神经元连接更强"

---

### Phase 7: Python AI 层（预计 5-7 天）

> **目标**：LLM 调用（不引入向量数据库，初期用不上）

#### Task 7.1: Python MCP Server 骨架

```
python/
├── pyproject.toml
├── ai_brain/
│   ├── __init__.py
│   ├── server.py          # MCP Server 入口
│   ├── llm/
│   │   ├── __init__.py
│   │   ├── provider.py    # 多 Provider LLM 调用
│   │   └── prompts.py     # 各副脑的 system prompt
│   └── tools/
│       ├── __init__.py
│       └── definitions.py   # MCP Tool 定义
```

> **注意**：初期不引入 chromadb/qdrant 向量数据库。
> 记忆检索基于 Rust 层的纯文件 + 关键词索引。
> 向量数据库在长期记忆 > 10000 条且关键词召回率 < 70% 时再引入。

- 通过 MCP 协议与 Rust 层通信
- 暴露为 MCP Server（stdio transport）
- Rust 作为 MCP Client 调用 Python 能力

#### Task 7.2: LLM 多 Provider 调用

- 支持 Anthropic / OpenAI / 本地模型
- 不同副脑使用不同模型（感知脑 haiku，推理脑 sonnet/opus）
- 流式响应处理

#### Task 7.3: 记忆增强服务

- Rust 层粗筛候选记忆（关键词匹配）→ Python LLM 精排
- 巩固时的摘要生成（从原始对话中提炼任务级总结）
- 记忆冲突解决（多条记忆矛盾时由 LLM 判断可信度）

> **不引入向量数据库**。语义检索通过 LLM 对候选记忆做精排实现。

---

### Phase 8: CLI + API 集成（预计 3-4 天）

> **目标**：用户可通过 CLI / HTTP API 使用完整系统

#### Task 8.1: 新 CLI 入口 (`ai-brain-cli`)

```bash
# 启动交互模式
ai-brain

# 单次查询
ai-brain query "这个月有什么节假日？"

# 管理命令
ai-brain status          # 查看副脑状态
ai-brain evaluate        # 手动触发评估脑
ai-brain memory stats    # 查看记忆统计
ai-brain weights         # 查看权重分布
```

#### Task 8.2: HTTP API 服务

```rust
// 使用 axum 框架
POST /api/query          # 提交查询
GET  /api/status         # 系统状态
GET  /api/brains         # 副脑列表
GET  /api/memory/stats   # 记忆统计
POST /api/evaluate       # 手动触发评估
```

---

### Phase 9: 进化机制（预计 2-3 天）

> **目标**：新副脑诞生 + 退化机制 + 自我优化

#### Task 9.1: 新副脑动态创建

- 定义副脑模板（基础 prompt + 能力声明）
- 动态注册到广播通道和结果通道
- 初始权重 = 0.5

#### Task 9.2: 副脑退化

- 权重持续低于 0.2 → 进入休眠
- 休眠副脑不订阅广播，减少资源
- 相关任务出现时可唤醒

---

## 四、关键设计决策

### 4.1 通信架构

```
Rust (核心运行时) ←→ Python (AI 能力)
         ↕                    ↕
  MCP 协议(stdio)        MCP 协议(stdio)
         ↕                    ↕
  三通道消息总线        LLM / 向量服务
```

- Rust 与 Python 之间通过 **MCP 协议** 通信（复用现有 mcp_stdio.rs）
- 副脑之间通过 **三通道消息总线** 通信（tokio channels）
- 不使用 FFI，保持架构清晰

### 4.2 快/慢思考策略

| 场景 | 策略 | 调 LLM | 延迟 |
|------|------|--------|------|
| 经验命中且置信度 > 0.7 | 快思考 | 否 | ~10ms |
| 无经验或低置信度 | 慢思考 | 是 | ~1-5s |
| 简单查询（时间/计算） | 快思考 | 否 | ~10ms |
| 复杂推理/代码生成 | 慢思考 | 是 | ~2-10s |
| 记忆精确匹配 | 快思考 | 否 | ~10ms |
| 记忆语义检索 | 慢思考 | 是(Python) | ~500ms |

### 4.3 记忆持久化路径

```
~/.ai-brain/
├── config.toml                          # 全局配置
├── sessions/                            # L3: 原始记忆（永久保留）
│   ├── {session-id}.jsonl               # 完整对话历史
│   └── {session-id}.jsonl.1 (轮转)
├── memory/
│   ├── short-term/                      # L2: 短期记忆
│   │   ├── recent.json                  # 最近活跃记忆
│   │   └── preferences.json             # 用户偏好
│   ├── events/                          # L1: 事件索引
│   │   ├── 2026-04-03.json              # 每日摘要
│   │   └── 2026-04-02.json
│   ├── long-term/                       # L0: 任务总结
│   │   └── tasks/
│   │       ├── task-001.json            # 一个任务的完整总结
│   │       └── task-002.json
│   └── index/                           # 关键词索引
│       └── tags.json                    # { tag → [memory_id, ...] }
├── weights/                             # 权重数据
│   └── weights.json
└── logs/                                # 运行日志
    └── brain-2026-04-03.log
```

---

## 五、开发优先级

```
Phase 1 (基础设施)  ████████████████████  最高 — 一切依赖于此
Phase 2 (感知+主脑)  ██████████████████   高   — 最小可用闭环
Phase 3 (记忆脑)    ████████████████     高   — 系统智能的核心
Phase 4 (推理脑)    ██████████████       中高 — 深度思考能力
Phase 5 (执行+校验)  ████████████        中   — 安全保障
Phase 6 (评估+权重)  ██████████          中   — 自我优化
Phase 7 (Python层)  ████████████████     高   — AI 能力核心
Phase 8 (CLI+API)   ████████            低   — 用户接口
Phase 9 (进化)      ██████              低   — 锦上添花
```

**建议开发顺序**：Phase 1 → Phase 2 → Phase 3 → Phase 7(部分) → Phase 4 → Phase 5 → Phase 6 → Phase 8 → Phase 7(完善) → Phase 9

Phase 7 的 LLM 调用部分应在 Phase 2 就开始准备，因为感知脑和慢思考都需要调 LLM。

---

## 六、测试策略

### 单元测试

每个 crate 都需要 `#[cfg(test)] mod tests`：
- `brain-bus`: 消息收发、防循环、通道容量
- `brain-core`: Weight 范围验证、BrainMessage 序列化
- `brain-memory`: 记忆存取、巩固、遗忘、多层检索
- `brain-reasoning`: 经验匹配、推理路径
- `brain-validation`: 安全规则、可信度评分

### 集成测试

- **最小闭环**: 感知脑 → 广播 → StubBrain → 主脑 → 输出
- **记忆流转**: 写入 → 衰减 → 巩固 → 检索
- **完整查询**: 使用 `example-holiday-query-flow.md` 的场景验证
- **权重进化**: 多次任务后权重变化正确

### 端到端测试

- 节假日查询完整流程（Phase 6 完成后）
- 代码开发任务流程（Phase 7 完成后）
