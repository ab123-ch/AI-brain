# 常驻小说脑架构设计

## 1. 结论

小说脑应重构为 v2 运行时中的常驻领域脑，而不是每次通过
`Agent(subagent_type="Novel")` 创建一个新会话。

目标架构遵循两个所有权边界：

1. `NovelBrain` 长期存活，负责小说项目的工作态、创作推理、自检和修订连续性。
2. `MemoryBrain` 是所有持久化小说记忆的唯一入口，负责工作检查点、原始任务事件、
   Canon、冲突审计、发布日志和图谱投影。
3. `MainBrain` 负责把用户意图和项目材料组织成任务环境包，独立复审小说脑产物，
   再把合格候选稿展示给用户。

项目正文文件仍是作品内容的权威来源；MemoryBrain 保存 Canon、任务事件、文件引用和
内容 hash；Novel 图谱仍只是目录和关系索引。

## 2. 当前问题

当前实现有四个与目标不一致的点：

- Novel 复用了通用 `Agent`，每次创建 `Session::new()`，任务结束后运行时即销毁。
- Novel 通过注入的 memory root 和 graph DB path 自行打开存储，绕过 MemoryBrain。
- 主脑复审、保存正文、提交 Delta 的顺序主要由提示词约束，没有运行时状态机。
- Novel 模型只复用了模型名，未完整使用 `brain_providers.novel` 和
  `brain_params.novel`。

## 3. 目标与非目标

### 3.1 目标

- Orchestrator 启动时创建 NovelBrain，关闭时安全 checkpoint 并停止。
- NovelBrain 跨用户轮次保留同一小说项目的短期工作上下文。
- 重启后从 MemoryBrain 恢复未完成任务，而不是依赖 NovelBrain 自有文件。
- 用户只与 MainBrain 对话；NovelBrain 的澄清问题、草稿和修订都经 MainBrain 转发。
- 主脑向 NovelBrain 提供任务环境包和材料引用；NovelBrain 自己通过 MemoryBrain 获取 Canon
  和生命周期记忆，不由主脑复制整份长期记忆。
- 每个项目严格隔离；同一项目一次只允许一个可变写作事务。
- 默认顺序为小说脑自检、主脑复审、用户确认、作品保存和 Canon 提交，并由有类型状态机强制。
- MemoryBrain 统一处理小说记忆召回、检查点、Canon 提交和图谱投影。
- 保留按目录、详情、关系追踪、原文解析的渐进式记忆召回。

### 3.2 非目标

- 不让 NovelBrain 绕过 MainBrain 自主响应用户或自主发布作品。
- 不把所有历史对话永久保留在模型上下文中。
- 不把 Novel 图谱升级为 Canon 或正文的第二份权威存储。
- 不把新 NovelBrain 强行挂到旧 v1 BrainBus。
- 不在第一阶段删除通用 Agent；它仍服务 Explore、Plan、Verification 等临时代理。

## 4. 总体架构

```text
User
  |
  | requirements / answers / feedback
  v
MainBrain (environment provider + coordinator + independent reviewer)
  |
  | typed Novel commands through RealToolExecutor
  v
NovelBrainHandle  ---- status/events ----> Cockpit / Web trace
  |
  v
Resident NovelBrain supervisor
  |-- ProjectWorkspace(project-a)
  |-- ProjectWorkspace(project-b)
  `-- bounded project-session cache
        |                         |
        | memory RPC              | exact project resources
        v                         v
  NovelMemoryPort           NovelResourcePort
        |                         |
        v                         v
  PyramidMemoryBrain        scoped file/artifact access
        |
        |-- raw task events and draft refs
        |-- task checkpoints
        |-- Novel project Canon
        |-- publication journal
        `-- derived Novel graph projection
```

常驻指 NovelBrain 服务和项目工作态在多轮请求间继续存在，不代表上下文无限增长。
冷项目可以 checkpoint 后从内存中逐出，再由 MemoryBrain 恢复。

## 5. 组件所有权

| 组件 | 拥有 | 不拥有 |
|---|---|---|
| User | 需求、偏好、反馈、候选稿确认、冲突取舍 | NovelBrain 会话和持久化 |
| MainBrain | 用户会话、任务环境包、材料导航、独立复审、候选稿展示 | Novel 项目长期记忆和正文创作 |
| NovelBrain | LLM 客户端、项目短期工作态、草稿版本、自检、修订上下文 | 记忆路径、图谱路径、Canon 文件写权限 |
| MemoryBrain | 用户任务、主脑合同/结论、小说脑草稿/自检/修订、用户反馈、checkpoint、Canon、发布日志、图谱投影 | 作品正文的创作决策 |
| NovelResourcePort | 授权上下文读取、作品原子写入、内容 hash | Canon 和长期记忆 |
| Novel graph | 目录、摘要、关系、source refs | 正文全文和权威 Canon |

## 6. 新增运行时组件

### 6.1 `brain-novel` crate

建议新增 `crates/brain-novel`，包含：

```text
src/
  lib.rs
  actor.rs          # resident supervisor and command loop
  handle.rs         # cloneable MainBrain-facing handle
  types.rs          # requests, drafts, reviews, receipts, events
  state.rs          # project/task state machines
  runtime.rs        # model/tool loop and context compaction
  ports.rs          # NovelMemoryPort and NovelResourcePort
  review.rs         # typed self-review validation
```

该 crate 可以依赖 `brain-memory` 的 Novel contract types，但不得创建
`NovelMemoryStore`、`GraphStore` 或接收任何存储路径。

### 6.2 稳定身份

在 `brain-core` 增加：

```rust
BrainId::novel()
BrainKind::Novel
```

这用于状态、事件和驾驶舱标识。执行协议不复用旧 `BrainAgent`，避免让 v2 新设计依赖
旧广播总线。

### 6.3 `NovelBrainHandle`

MainBrain 不持有 NovelBrain mutex，只持有一个可克隆 handle：

```rust
pub struct NovelBrainHandle {
    tx: tokio::sync::mpsc::Sender<NovelCommand>,
}

pub enum NovelCommand {
    StartTask {
        request: NovelTaskRequest,
        reply: oneshot::Sender<Result<NovelOutcome, NovelBrainError>>,
    },
    ResumeTask {
        task_id: NovelTaskId,
        input: NovelResumeInput,
        reply: oneshot::Sender<Result<NovelOutcome, NovelBrainError>>,
    },
    ReviewDraft {
        review: MainReviewRecord,
        reply: oneshot::Sender<Result<ReviewTransition, NovelBrainError>>,
    },
    Publish {
        task_id: NovelTaskId,
        draft_version: u32,
        reply: oneshot::Sender<Result<PublicationReceipt, NovelBrainError>>,
    },
    Status {
        project_id: Option<String>,
        reply: oneshot::Sender<NovelBrainStatus>,
    },
    Shutdown,
}
```

通道必须有界，满载时返回明确 backpressure 错误，不静默丢消息。

### 6.4 NovelBrain supervisor

NovelBrain 作为一个稳定服务存在，内部按 `project_id` 管理工作区：

```rust
struct NovelBrain {
    llm: Arc<dyn LlmProvider>,
    memory: Arc<dyn NovelMemoryPort>,
    resources: Arc<dyn NovelResourcePort>,
    projects: HashMap<ProjectId, ProjectWorkspace>,
    events: broadcast::Sender<NovelBrainEvent>,
    status: ResidentBrainStatus,
}

struct ProjectWorkspace {
    project_id: ProjectId,
    canon_revision: u64,
    runtime: BoundedNovelRuntime,
    active_task: Option<NovelTaskState>,
    recent_task_refs: VecDeque<MemoryRef>,
    last_accessed_at: DateTime<Utc>,
}
```

第一版允许全局最多一个 LLM generation；状态仍按项目分区。后续可以扩展为不同项目并发，
但同一项目始终串行。

## 7. 记忆脑接口

### 7.1 禁止路径注入

删除 `AgentRuntimeContext.novel_memory_root`、`graph_db_path` 以及 Novel executor 中直接创建
`NovelMemoryStore` / `GraphStore` 的逻辑。

NovelBrain 只依赖下列端口：

```rust
#[async_trait]
pub trait NovelMemoryPort: Send + Sync {
    async fn load_workspace(
        &self,
        project_id: &str,
    ) -> Result<NovelWorkspaceSnapshot, NovelMemoryError>;

    async fn recall_project(
        &self,
        project_id: &str,
        task_type: NovelTaskType,
    ) -> Result<NovelRecallPack, NovelMemoryError>;

    async fn check_consistency(
        &self,
        project_id: &str,
    ) -> Result<ConsistencyReport, NovelMemoryError>;

    async fn search_catalog(&self, query: NovelCatalogQuery)
        -> Result<NovelCatalogPage, NovelMemoryError>;
    async fn get_detail(&self, request: NovelDetailRequest)
        -> Result<NovelMemoryDetail, NovelMemoryError>;
    async fn trace(&self, request: NovelTraceRequest)
        -> Result<NovelMemoryTrace, NovelMemoryError>;
    async fn resolve_refs(&self, refs: &[MemoryRef])
        -> Result<Vec<ResolvedMemoryRef>, NovelMemoryError>;

    async fn append_task_event(&self, event: NovelTaskEvent)
        -> Result<MemoryRef, NovelMemoryError>;
    async fn save_checkpoint(&self, checkpoint: NovelTaskCheckpoint)
        -> Result<(), NovelMemoryError>;

    async fn begin_publication(&self, request: BeginNovelPublication)
        -> Result<PublicationLease, NovelMemoryError>;
    async fn complete_publication(&self, request: CompleteNovelPublication)
        -> Result<CommitReport, NovelMemoryError>;
    async fn abort_publication(&self, publication_id: &str, reason: &str)
        -> Result<(), NovelMemoryError>;
    async fn pending_publications(&self)
        -> Result<Vec<PendingNovelPublication>, NovelMemoryError>;
}
```

`ai-brain-cli` 提供该 trait 的 adapter，内部锁定当前 `PyramidMemoryBrain` 并调用它的方法。
MemoryBrain 可以继续使用现有 JSON snapshot 和 SQLite graph，但这些成为它的私有实现细节。

### 7.2 记忆分层

MemoryBrain 保存四类数据：

1. `NovelTaskEvent`：追加式原始事件，如请求、澄清、草稿、自检、主脑反馈和用户反馈。
2. `NovelTaskCheckpoint`：任务当前状态、draft ref、runtime summary、版本和恢复信息。
3. `NovelProject`：已确认 Canon、进度、冲突和 revision。
4. `NovelPublicationRecord`：发布事务的 pending/completed/aborted 日志。

完整作品正文仍保存在项目文件。MemoryBrain 对已发布正文保存路径、hash、章节和 source ref，
不把全文复制到图谱。

### 7.3 渐进召回

NovelBrain 的模型工具保留低上下文的四阶段接口，但全部通过 MemoryBrain：

```text
novel_memory_catalog
  -> novel_memory_detail
  -> novel_memory_trace
  -> novel_memory_resolve_refs
```

普通写作先使用任务类型过滤后的 `recall_project`；只有出现具体疑点时才进入 catalog/detail/trace。

## 8. 项目资源接口

记忆与项目文件分开。主脑负责发现文件，然后传递不可变引用：

```rust
pub struct ContextRef {
    pub role: ContextRole,
    pub canonical_path: PathBuf,
    pub sha256: String,
    pub description: Option<String>,
}
```

NovelBrain 通过 `NovelResourcePort::read_context` 读取。端口必须：

- 只允许任务合同内的路径；
- 重新计算 hash，变化时返回 `ContextChanged`；
- 禁止目录遍历和符号链接逃逸；
- 不提供任意写入能力。

发布由另一个受限方法完成：

```rust
async fn write_artifact_atomic(
    &self,
    path: &Path,
    exact_content: &str,
) -> Result<ArtifactReceipt, ArtifactError>;
```

`ArtifactReceipt` 至少包含 canonical path、SHA-256、字节数和写入时间。

## 9. 有类型交付协议

### 9.1 主脑任务合同

```rust
pub struct NovelTaskRequest {
    pub task_id: NovelTaskId,
    pub project_id: ProjectId,
    pub task_type: NovelTaskType,
    pub target_chapter: Option<u32>,
    pub expected_revision: u64,
    pub output_path: PathBuf,
    pub context_refs: Vec<ContextRef>,
    pub must_happen: Vec<String>,
    pub must_not_change: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub allow_web_research: bool,
    pub publication_policy: PublicationPolicy,
    pub parent_task_id: Option<NovelTaskId>,
}
```

这就是主脑提供给小说脑的“任务环境包”。其中 `context_refs` 指向正文、上一章、章纲、
卷纲、角色卡、世界观设定和风格样例等关键材料；主脑负责发现和筛选，小说脑负责按需读取。
`publication_policy` 默认为 `RequireUserAcceptance`，只有用户明确要求自动保存时才能使用
`AutoAfterMainReview`。

### 9.2 小说脑结果

```rust
pub enum NovelOutcome {
    NeedsClarification(ClarificationRequest),
    DraftReady(NovelDraftEnvelope),
}

pub struct NovelDraftEnvelope {
    pub task_id: NovelTaskId,
    pub draft_version: u32,
    pub project_id: ProjectId,
    pub canon_revision: u64,
    pub content: String,
    pub self_review: NovelSelfReview,
    pub proposed_delta: NovelMemoryDelta,
    pub evidence_refs: Vec<SourceRef>,
}
```

不再依赖三个字符串 tag 作为主协议。过渡期可以把旧 tag 解析为该结构。

### 9.3 主脑复审记录

```rust
pub struct MainReviewRecord {
    pub task_id: NovelTaskId,
    pub draft_version: u32,
    pub reviewed_canon_revision: u64,
    pub verdict: MainReviewVerdict, // Pass | Revise
    pub checks: MainReviewChecks,
    pub issues: Vec<ReviewIssue>,
    pub evidence_refs: Vec<SourceRef>,
    pub summary: String,
}
```

`Pass` 必须满足：

- 小说脑自检完整且 verdict=pass；
- 主脑全部必选检查为 pass；
- issues 为空；
- draft version 和 Canon revision 与当前任务一致；
- evidence refs 至少包含用户要求、目标章纲/前文和 Canon recall 的引用。

主脑复审通过只表示候选稿可以展示给用户，不等于允许写入正式文件或进入 Canon。

### 9.4 用户决策记录

```rust
pub struct UserDecisionRecord {
    pub task_id: NovelTaskId,
    pub draft_version: u32,
    pub decision: UserDecision, // Accept | Revise | Reject
    pub feedback: Option<String>,
    pub decided_at: DateTime<Utc>,
}
```

默认模式只有 `Accept` 才能进入 `ApprovedForPublication`。`Revise` 会携带用户反馈回到同一
常驻 task/session；`Reject` 保存生命周期记录但不修改作品文件和 Confirmed Canon。

状态机只能强制主脑提交独立复审记录和用户决策记录，无法证明模型进行了高质量思考；质量仍由
prompt、测试和可观测证据共同保证。

## 10. 任务状态机

```text
Preparing
  | load MemoryBrain workspace + validate ContextRefs
  v
Drafting <---------------------------+
  |                                   |
  v                                   |
SelfReview                            |
  |                                   |
  +--> NeedsClarification             |
  |       | MainBrain asks User       |
  |       `---- ResumeTask -----------+
  v
AwaitingMainReview
  | MainReview=Revise
  +-----------------------------------+
  |
  | MainReview=Pass
  v
AwaitingUserDecision
  | User=Revise
  +-----------------------------------+
  | User=Reject
  +--> Rejected
  |
  | User=Accept
  v
ApprovedForPublication
  | Publish command
  v
PublicationPending
  | MemoryBrain begin -> artifact atomic write -> MemoryBrain complete
  v
Completed
```

附加终态/异常态：`Cancelled`、`Failed`、`StaleRevision`、
`ArtifactSavedMemoryPending`。

所有转换都写入 MemoryBrain checkpoint。非法转换返回结构化错误，例如：

- 未复审直接 publish；
- 未取得用户确认直接 publish（除非任务合同明确使用 AutoAfterMainReview）；
- 对旧 draft version 提交 pass；
- Canon revision 变化后继续保存；
- 同一项目同时启动第二个写任务；
- 修改已经 approved 的候选正文。

## 11. 用户、主脑、小说脑协作

### 11.1 新任务

1. User 向 MainBrain 提出小说任务。
2. MainBrain 补齐关键需求，定位正文、章纲、角色卡、设定和风格文件，创建任务环境包
   `NovelTaskRequest`。
3. MemoryBrain 记录用户任务和主脑任务合同，形成生命周期起点。
4. NovelBrain 从 MemoryBrain 恢复项目工作区并召回 Canon/一致性报告。
5. NovelBrain 读取授权 ContextRefs，生成初稿并完成自检。
6. MemoryBrain 保存草稿事件、自检结果和当前 checkpoint；它们仍是 draft，不是 Confirmed Canon。
7. NovelBrain 返回 typed draft；MainBrain 独立复审。

### 11.2 需要用户澄清

1. NovelBrain 返回 `NeedsClarification`，任务保持常驻并 checkpoint。
2. MainBrain 将问题整理后询问 User。
3. User 下一轮回答。
4. MainBrain 调用 `ResumeTask`；NovelBrain 在同一 task/session 上继续。

NovelBrain 不直接向 User 提问，Web 也不需要依赖当前未完成的阻塞式 AskResponse。

### 11.3 主脑要求修订

1. MainBrain 提交 verdict=Revise 和具体 issue/evidence。
2. NovelBrain 保留 task ID 和项目工作上下文，draft version 加一。
3. NovelBrain 按反馈修订并重新自检。
4. 新草稿再次进入 `AwaitingMainReview`。

这不是重新启动一个 Agent，也不需要重新进行目录探索。

### 11.4 展示与用户决策

1. 主脑复审不通过时，候选稿先退回 NovelBrain 内部修订，不直接展示为最终稿。
2. 主脑复审通过后，将候选正文展示给 User，并说明这是待确认版本。
3. User 选择接受、提出修改意见或拒绝。
4. MainBrain 把 `UserDecisionRecord` 写回同一 task。
5. User 要求修改时，NovelBrain 保留原 task ID 和上下文继续修订、自检、主脑复审、再次展示。
6. User 接受后任务才进入 `ApprovedForPublication`。

如果用户在最初任务中明确要求“生成后直接保存”，主脑可在任务合同中设置
`AutoAfterMainReview`；这不是默认行为。

### 11.5 发布

1. MainBrain 已提交有效 Pass review，默认还必须存在 User=Accept 记录。
2. MainBrain 调用 `novel_publish(task_id, draft_version)`，不再次传正文。
3. NovelBrain 从内存状态或 MemoryBrain draft ref 取得用户确认过的正文并计算 hash。
4. NovelBrain typed state 校验 review 和用户决策；MemoryBrain `begin_publication` 校验项目、
   expected revision 和 Delta 后记录 pending journal。
5. ResourcePort 将 exact content 原子 no-clobber 安装到合同中的 output path，返回 receipt；
   已存在的同 hash 文件视为幂等成功，不同 hash 文件拒绝覆盖。
6. MemoryBrain `complete_publication` 校验 receipt/hash，提交 Delta、进度和 episode ref。
7. MemoryBrain 更新 Canon revision，并异步/可重试地投影 Novel graph。
8. NovelBrain 标记 Completed，MainBrain 向 User 汇报保存路径、revision 和发布结果。

如果第 4 步写入 journal 后、pending checkpoint 前崩溃，重启时只有在 Approved checkpoint
与 journal 的 gate、task/project、version、revision、path、hash 和 Delta 全部一致时才补写
pending checkpoint。若第 5 步后崩溃，重启时从 pending journal 检查文件 hash：一致则补完
MemoryBrain commit；不一致则停在恢复态，绝不静默覆盖。

## 12. 生命周期与上下文预算

### 12.1 启动

- Orchestrator 总是创建 `NovelBrainHandle`。
- Novel LLM 可用时状态为 `Ready`；不可用时仍保留 resident handle，状态为 `Degraded`。
- 启动只恢复未完成任务和 pending publications；其他项目按需加载。

### 12.2 运行

- 每个项目保存独立 runtime/session，禁止跨项目消息混入。
- 默认只保留最近若干轮；当前草稿、审核和发布状态保存在 typed checkpoint，完整生命周期事件
  由 MemoryBrain 持久化。
- 达到消息预算时先保存最新事件/checkpoint，再逐出最旧的内存消息；后续可在不改变端口契约的
  前提下增加 project working summary 压缩。
- project workspace 使用 LRU；只逐出无活动写事务且已有最新 checkpoint 的项目。

### 12.3 关闭

- 停止接收新命令；
- 等待当前 LLM 调用到安全点或取消；
- 独立 shutdown signal 不受任务命令队列 backpressure 影响；
- 保存所有 dirty checkpoints；
- flush MemoryBrain task events/publication journal；
- 发出 `NovelBrainDeactivated` 事件后结束 actor。

## 13. 主脑工具面

新增专用工具，取代 MainBrain 对 `Agent(Novel)` 的直接使用：

- `novel_start_task`
- `novel_resume_task`
- `novel_review_draft`
- `novel_user_decision`
- `novel_publish`
- `novel_status`

`novel_commit_delta` 改为 MemoryBrain 内部 API，不再暴露给 MainBrain LLM。否则主脑仍能绕过
审核/发布状态机直接提交 Canon。

通用 `write_file` 可以继续存在，但小说工作流的 Canon 提交只接受 `novel_publish` 产生的
服务器端 `ArtifactReceipt`。任意手工文件写入不能自动成为 Canon。

过渡期对 `Agent(subagent_type="Novel")` 做兼容转发并返回 deprecation warning；稳定后从
Agent schema 的 Novel 枚举中删除。

## 14. 可观测性

NovelBrain 发布有类型事件：

- `Activated` / `Degraded` / `Deactivated`
- `ProjectLoaded` / `ProjectEvicted`
- `TaskStarted` / `TaskResumed`
- `MemoryRecalled` / `ConsistencyChecked`
- `ClarificationRequested`
- `DraftReady` / `MainReviewRecorded` / `RevisionStarted`
- `PresentedToUser` / `UserDecisionRecorded`
- `PublicationPending` / `ArtifactSaved` / `CanonCommitted`
- `TaskFailed` / `StaleRevision`

驾驶舱中 Novel 节点始终存在；空闲时显示 `resident/idle`，任务期间显示项目、task ID、阶段、
draft version 和 memory connection，不再把它画成一次性动态 agent。

## 15. 代码改动边界

### `brain-core`

- 增加 Novel brain ID/kind。
- 如需跨 crate 使用，放置稳定的 task/review ID 基础类型。

### `brain-novel`（新增）

- 实现 resident actor、handle、状态机、typed contracts、上下文压缩和端口。
- 不依赖文件存储实现，不打开 Memory/Graph 路径。

### `brain-memory`

- 增加 workspace/checkpoint/task-event/publication journal API。
- 将 `novel_memory_store()` 降为私有实现细节。
- 将 Novel graph catalog/detail/trace/resolve 封装为 MemoryBrain 方法。
- `complete_publication` 成为唯一 Canon Delta 提交入口。

### `runtime` / `api`

- 抽取可复用 provider-backed runtime client，接收 Orchestrator 已解析的 provider/model 参数。
- 不再让 Novel runtime 自行按模型名前缀猜 provider。

### `ai-brain-cli`

- Orchestrator 启动/关闭 NovelBrain。
- 实现 `NovelMemoryPort` 和 `NovelResourcePort` adapters。
- RealToolExecutor 注入 `NovelBrainHandle` 并路由专用工具。
- runtime trace/status/Web cockpit 连接稳定 Novel identity。

### `tools`

- 添加六个专用工具 schema。
- 移除 Novel 的 ephemeral Agent runtime、路径注入和 scoped direct store executor。
- 保留非 Novel 通用 Agent。

### `brain-main`

- 修改系统提示和 skill，使用专用 Novel tools。
- 要求 typed MainReviewRecord，而不是检查 Agent manifest/tag。

## 16. 渐进迁移

### Phase 1: Contracts And Memory Boundary

- 增加 `brain-novel` contract/port/state 类型。
- 在 MemoryBrain 增加 checkpoint、task event 和 publication journal。
- 为当前 direct store 行为补 characterization tests。

### Phase 2: Resident Runtime

- Orchestrator 创建 NovelBrain actor 和 handle。
- 用 `create_brain_client("novel")` 初始化模型。
- 实现项目工作区、恢复、上下文预算和稳定事件。

### Phase 3: MainBrain Tools And Review State Machine

- 增加六个专用工具并更新 skill/prompt。
- 实现 clarification/resume、主脑 revise/pass、用户 accept/revise/reject 状态转换。
- `Agent(Novel)` 暂时兼容转发到 resident handle。

### Phase 4: Publication Transaction

- 实现原子 artifact writer、hash receipt、MemoryBrain begin/complete/recovery。
- 从 MainBrain 工具集中移除直接 `novel_commit_delta`。
- 驾驶舱展示发布和记忆事务状态。

### Phase 5: Remove Ephemeral Novel Path

- 删除 `AgentRuntimeContext` 的 Novel 存储路径。
- 删除 SubagentToolExecutor 中 Novel direct memory/graph 分支。
- 删除 `Agent` schema 中的 Novel 类型和兼容适配。
- 收紧 `NovelMemoryStore` 可见性。

### Phase 6: Entry-Point Unification

- 让 one-shot CLI 和 HTTP API 进入同一 v2 MainBrain 路径。
- 确保所有用户入口都能使用 resident NovelBrain。

## 17. 测试策略

### 单元测试

- 同一个 NovelBrain 实例连续处理两轮任务，project session 未重建。
- 两个 project 的 runtime、Canon 和 context refs 不互相可见。
- 未授权文件、hash 变化和路径逃逸被拒绝。
- 自检缺项、旧 draft review、旧 revision 和非法状态转换被拒绝。
- 未 Pass 不能 publish；publish 不接受调用方传入正文。
- 默认模式缺少 User=Accept 时不能 publish；用户修改继续使用同一 task/session。
- Memory port fake 证明 NovelBrain 从不访问存储路径。

### 状态机属性测试

- 任意默认事件序列都不能跳过 `AwaitingMainReview -> AwaitingUserDecision -> ApprovedForPublication`。
- Canon commit 前必然存在匹配 draft/hash 的 artifact receipt。
- 每个 project 同时最多一个 publication lease。
- Completed task 的 revision 单调递增。

### 故障恢复测试

在 publication 的每一步注入崩溃：

- begin 前；
- begin 后、写文件前；
- artifact 原子安装后、MemoryBrain complete 前；
- Canon commit 后、graph projection 前。

重启后必须得到 Completed、可重试 pending 或显式 conflict，不能重复写 Canon。

### 集成测试

- User -> Main -> resident Novel -> Main review -> User decision -> publish -> MemoryBrain -> User 全链路。
- Novel clarification 跨两个用户轮次恢复同一 task ID。
- Main/User revise 两次后通过，draft version 为 3，只有用户接受的最终版本进入 Canon。
- Web/TUI 切换会话不销毁 NovelBrain；进程重启从 MemoryBrain 恢复。
- 驾驶舱始终显示一个稳定 Novel 节点和完整双向通信。

## 18. 验收标准

满足以下条件才算完成重构：

1. NovelBrain 随 Orchestrator 启动，连续用户轮次使用相同 resident service。
2. NovelBrain 和 RealToolExecutor 不再接收 memory root / graph path。
3. 所有小说记忆读写都能在 MemoryBrain adapter 上观测到。
4. 重启后能恢复 AwaitingUser、AwaitingMainReview 和 PublicationPending 任务。
5. 默认模式未完成小说脑自检、主脑复审和用户确认时，运行时拒绝发布。
6. Canon 只能由带有效 artifact receipt 的 publication transaction 更新。
7. 图谱只保存索引和关系，项目文件/MemoryBrain Canon 仍是权威数据源。
8. `Agent(subagent_type="Novel")` 的 ephemeral 路径最终删除。

## 19. 关键取舍

- 不复用旧 BrainBus：可以减少表面新增，但会把 v2 主脑重新绑到 v1 广播协议。
- 使用 typed actor：比通用 Agent schema 多一些类型，但能表达常驻生命周期、恢复和状态转换。
- MemoryBrain 拥有 publication journal：增加一个事务层，但解决文件已保存、Canon 未提交时的崩溃恢复。
- bounded residency：服务永久存在，热上下文有限；这比无限保留所有项目消息更稳定，也符合上下文预算要求。
- MainBrain 保留最终授权：NovelBrain 可以长期思考和修订，但不能绕过主脑直接发布或提交 Canon。
