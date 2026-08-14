# Graph-first 统一记忆架构与 Web 管理设计

## 背景

当前人格记忆生产链由 L1/L2/L3/L4 JSON、旧 `graph/graph.db`、根 `memory.db` 与根 `graph.db` 多套存储组成。现状审计确认：本机默认人格有 208 份 L1，但 L2/L3 为空，L4/Profile 不存在；生产召回仍调用 `recall_for_context()`，而 `progressive_recall.rs` 的 L2/L1 下钻尚未完成。详细证据见 `docs/architecture/2026-08-14-memory-system-current-state-analysis.md`，既有整改基线见 `docs/plans/2026-08-14-memory-system-graph-first-remediation.md`。

本设计在既有 Graph-first 决策之上补齐统一应用服务、CLI/Web 管理入口、状态治理和用户可见的来源链。已经确认的产品决策是：

- Web 与 CLI 使用同一套记忆格式、状态语义、数据库和应用服务。
- Web 新增独立“记忆管理”页；聊天和驾驶舱仅保留轻量入口与指标。
- 默认展示当前人格，可切换全部人格；共享记忆作为独立作用域。
- 遗忘采用两阶段机制：先进入可恢复的遗忘区，再二次确认永久清除。
- 采用一次性停机迁移，不长期双写旧链路。

## 目标

1. 保留 L1 原始对话作为不可变证据，让根 `memory.db` 成为唯一派生事实源。
2. 让根 `graph.db` 成为由 Memory outbox 幂等维护、可全量重建的关系投影。
3. 让 Web、CLI、TUI、对话运行时和模型工具共用同一服务与 DTO。
4. 所有派生记忆都具备作用域、版本、状态、信任等级、来源和审计记录。
5. 完成可解释的 catalog → detail → resolve_refs 召回链，并统一执行 token 预算。
6. 提供确认、编辑、归档、遗忘、恢复、清除、重建、诊断和导出能力。
7. 迁移当前数据时保持 L1 哈希不变，并具备完整校验与回滚路径。

## 非目标

- 不引入 DSH 记忆插件作为运行时依赖。
- 首期不依赖 embedding 服务；先使用可测试的中英文规范化、n-gram 和字段加权检索。
- 不把图数据库设为事实源；Graph 落后时允许降级到 Memory 查询。
- 不在首期物理删除 L1；涉及原始证据的 purge 是独立、强确认的管理动作。
- 不重构 Novel namespace 或协作业务模型，只保证现有数据与投影不受损。

## 总体架构

```text
Web HTTP ───────┐
CLI / TUI ──────┼──▶ BrainMemoryAdmin ───────┐
模型管理工具 ───┘                             │
                                             ├──▶ L1 JSONL（原始证据）
对话运行时 ───────▶ BrainMemoryRuntime ──────┤
                                             ├──▶ memory.db（唯一派生事实源）
                                             └──▶ outbox ─▶ graph.db（可重建投影）

召回：scope/status/trust 过滤
   ─▶ catalog
   ─▶ detail
   ─▶ ContextBuilder
   ─▶ 必要时 resolve_refs 展开 L1
```

### 存储职责

| 数据 | 事实源 | 生命周期 |
|---|---|---|
| 原始用户、助手和受限工具轨迹 | 每人格 L1 JSONL | 只追加；失效 generation 移出 active |
| 摘要、经验、偏好、决策、画像和规则 | 根 `memory.db` | 事务、版本、状态、来源、审计 |
| 目录、实体关系和证据关系 | 根 `graph.db` | outbox 幂等投影，可重建 |
| L4/Profile/Eval 注入文本 | 有界缓存 | 由 active/authorized Memory 生成 |
| 旧 L2/L3 JSON、旧 L4、`graph/graph.db` | 迁移备份 | 切换后停止生产读写 |

## 公共领域模型

Web、CLI 和 Rust 内部适配器共享同一组序列化 DTO，不建立 Web 专用记忆结构。

```rust
pub struct MemoryRecord {
    pub id: String,
    pub version: u64,
    pub memory_type: MemoryType,
    pub title: String,
    pub content: String,
    pub status: MemoryStatus,
    pub trust: MemoryTrust,
    pub importance: f32,
    pub scopes: Vec<MemoryScope>,
    pub sources: Vec<MemorySourceRef>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### 类型

首期注册以下类型，后续只能通过 schema 迁移扩展：

- `conversation.task_summary`
- `experience.rule`
- `persona.preference`
- `persona.profile`
- `project.decision`
- `evaluation.requirement`
- `evaluation.pitfall`
- `evolution.rule`

### 作用域

- `Persona(persona_id)`：人格私有记忆。
- `Shared`：所有人格可见，但仍遵守 trust/status 过滤。
- `Project(workspace_id)`：canonical workspace identity。
- `GitBranch { workspace_id, branch }`：必须同时携带对应 project scope。

Web 默认随顶部人格选择器查询 `Persona(current)`；“全部人格”是显式跨人格管理查询，不改变正常对话的隔离规则；共享记忆在筛选器和详情中独立标识。

### 信任等级

- `Untrusted`：外部输入或无法验证的导入，永不自动注入。
- `ModelGenerated`：LLM 浓缩候选，可搜索但默认待确认。
- `UserConfirmed`：用户明确要求或人工确认，可按预算注入。
- `DomainConfirmed`：受控配置或管理员导入，可按预算注入。

### 状态机

```text
Pending ─确认────▶ Active
Pending ─归档────▶ Archived
Pending ─遗忘────▶ Forgotten
Active  ⇄ Archived
Active  ⇄ Forgotten
Forgotten ─永久清除▶ Tombstoned
```

- `Pending`、`Archived`、`Forgotten`、`Tombstoned` 均不参与自动召回。
- 恢复产生新版本，不修改旧版本；所有写操作携带 `expected_version`。
- `Tombstoned` 不恢复为同一版本；物理清除 L1 必须走独立 purge。
- 每次确认、编辑、归档、遗忘、恢复和清除均追加审计事件。

## 公共服务边界

### 热路径

```rust
pub trait BrainMemoryRuntime: Send + Sync {
    fn append_turns(&self, request: AppendTurnsRequest) -> Result<AppendReceipt>;
    fn invalidate_generation(
        &self,
        request: InvalidateMemoryRequest,
    ) -> Result<InvalidationReceipt>;
    fn search_for_context(
        &self,
        request: MemoryContextRequest,
    ) -> Result<Vec<MemoryCatalogItem>>;
    fn build_inject_context(
        &self,
        request: InjectContextRequest,
    ) -> Result<ContextSnapshot>;
}
```

### 管理路径

```rust
pub trait BrainMemoryAdmin: Send + Sync {
    fn overview(&self, query: MemoryOverviewQuery) -> Result<MemoryOverview>;
    fn search(&self, query: MemorySearchQuery) -> Result<MemoryPage>;
    fn get(&self, id: &str, scope: AuthorizedScope) -> Result<MemoryRecord>;
    fn save(&self, command: SaveMemoryCommand) -> Result<MemoryRecord>;
    fn update(&self, command: UpdateMemoryCommand) -> Result<MemoryRecord>;
    fn transition_status(
        &self,
        command: TransitionMemoryStatusCommand,
    ) -> Result<MemoryRecord>;
    fn resolve_sources(&self, query: ResolveMemorySourcesQuery)
        -> Result<Vec<ResolvedMemorySource>>;
    fn rebuild(&self, command: RebuildMemoryCommand) -> Result<MemoryJob>;
    fn diagnostics(&self, query: MemoryDiagnosticsQuery) -> Result<MemoryDiagnostics>;
    fn export(&self, query: ExportMemoryQuery) -> Result<MemoryExport>;
}
```

Orchestrator、TUI、Web handler、`RealToolExecutor` 和命令行 handler 只依赖上述接口，不再直接获取 `PyramidMemoryBrain` 锁或调用旧 `recall_for_context()`。

## 写入、浓缩与投影

### 原始写入

每轮对话继续先写 L1，确保分析失败不丢证据。人格 ID 仅允许 1～64 位 ASCII 字母、数字、`-`、`_`；所有人格目录操作必须 canonicalize 并验证目标是 `personas/` 根的严格子级。

工具输出进入 L1 前执行可配置 redaction 和单条字节限制。被编辑或重试的 Web generation 继续沿用现有 revision-safe、fail-closed 失效协议。

### 增量浓缩

每人格持久化 `last_processed_revision`。任务只读取 checkpoint 之后的 active L1，LLM 在事务外生成 proposal；程序校验类型、长度、importance、唯一键和每个 `SourceRef` 后，将 entries、versions、scopes、sources、audit、outbox 与 checkpoint 在 `memory.db` 单事务提交。

模型创建失败、超时、非法 JSON、越界引用或任务 panic 都记录为持久任务失败，不改变 canonical Memory，也不推进 checkpoint。

### Graph 投影

消费者处理 `memory_committed`、`memory_superseded` 和 `memory_tombstoned` outbox 事件，用 `GraphMutationBatch` 幂等更新节点、边、evidence link 和 projection checkpoint。投影失败不会回滚 Memory；诊断和 Web overview 暴露 lag 与失败原因。

全量 rebuild 只清理并重建 `ai-brain.memory` namespace，不影响 Novel 或其他 namespace。

## 召回与注入

召回严格分三阶段：

1. `catalog` 返回最多 5 个小候选：ID、标题、类型、匹配词、分数、trust、source count 和一行 hint。
2. `detail` 返回选中条目的结构化内容、关系、版本和来源引用。
3. `resolve_refs` 仅在明确历史意图、用户展开来源或模型显式调用工具时读取 L1 原文。

自动上下文最多注入 3 条、合计 800 tokens；启动常驻记忆最多 1,200 tokens。统一通过 `ContextBuilder` 执行 scope、status、trust、条数和 token 预算。模型候选必须标注“历史候选，不是当前事实”，并保留 ID、版本和来源，供 Web 显示本轮引用。

Graph 不可用或落后时降级到 `memory.db` catalog/detail，不阻塞正常聊天；投影状态随诊断信息返回。

## Web 记忆管理页

顶部视图切换增加“记忆”，沿用现有 `index.html`、`app.js`、`style.css` 的原生深色界面。桌面布局为三栏：

- 左栏：当前人格、全部人格、共享作用域；Active/Pending/Archived/Forgotten 状态计数；类型和 trust 过滤。
- 中栏：服务端搜索、排序、分页和记录列表。
- 右栏：详情、版本、信任、作用域、来源链和治理操作。

移动端按“筛选抽屉 → 列表 → 详情页”降级，不强行保留三栏。

交互规则：

- 进入页面时只加载 overview 和第一页，选中记录后再加载详情与来源。
- 搜索使用防抖并由服务端执行；cursor、limit 和所有过滤条件进入请求。
- 编辑、确认、归档、遗忘、恢复携带 `expected_version`；冲突时显示当前版本和本地修改。
- 永久清除要求输入记忆标题二次确认；如果影响 L1，再进入独立 purge 流程。
- rebuild 是后台任务，显示阶段、进度、失败原因和重试入口。
- 聊天消息提供“本轮引用 N 条记忆”入口，点击后定位到记忆管理页。
- 驾驶舱只新增有效记忆、待确认、projection lag 和失败任务指标。

## HTTP API 与 CLI

Web 路由挂载到现有 `serve_web_with_policy()` 的 Router，并复用相同 `AppState` 中的公共服务：

```text
GET    /api/memory/overview
GET    /api/memory/records
POST   /api/memory/records
GET    /api/memory/records/{id}
PATCH  /api/memory/records/{id}
POST   /api/memory/records/{id}/confirm
POST   /api/memory/records/{id}/archive
POST   /api/memory/records/{id}/forget
POST   /api/memory/records/{id}/restore
POST   /api/memory/records/{id}/purge
GET    /api/memory/records/{id}/sources
GET    /api/memory/diagnostics
POST   /api/memory/rebuild
GET    /api/memory/jobs/{job_id}
GET    /api/memory/export
```

列表支持 `query`、persona/scope、status、memory type、trust、时间范围、cursor、limit 和 sort。

CLI 对应 `memory list/search/show/save/update/confirm/archive/forget/restore/purge/sources/rebuild/status/doctor/export`。CLI 不通过 HTTP 绕行，而是在同一进程内调用 `BrainMemoryAdmin`；请求和响应 DTO 与 Web JSON 相同。

## 错误契约

Web 使用统一错误体：

```json
{
  "code": "memory_version_conflict",
  "message": "记忆已被其他操作更新",
  "details": {},
  "request_id": "..."
}
```

- `400`：参数或状态转换非法。
- `403`：跨人格、跨项目或其他作用域越权。
- `404`：记录、来源或任务不存在。
- `409`：expected version 或幂等键冲突。
- `422`：来源引用、内容或 schema 校验失败。
- `503`：依赖暂不可用；Graph 单独不可用时正常降级，不返回 503。

后台任务持久化 task ID、persona、input revision、status、stage、progress、attempt、started/finished、model 和 error。重启后可查询已完成结果或重试失败任务。

## 一次性迁移与回滚

1. 发布包含新 schema、服务、迁移器、doctor 和回滚支持的同一版本二进制。
2. 停止所有 Web/CLI 写入并取得进程锁。
3. 完整备份 personas、sessions、配置摘要和三个现有数据库，生成路径、大小、SHA-256、schema 与行数 manifest。
4. 对原数据库执行 integrity check，并逐行解析所有 active L1；坏行必须先处理，不允许跳过。
5. 使用 SQLite backup API 创建迁移副本，保留 Novel/协作数据并注册 `ai-brain.memory` schema。
6. 按 persona/session 固定批次迁移 L1，稳定 ID 由 persona、memory type、semantic key 和 source hash 生成；每批持久 checkpoint，可断点续跑。
7. 在副本中写入 canonical Memory、消费 outbox、重建 Graph 与缓存。
8. 校验 L1 哈希、来源解析率、Memory/Graph 数量、projection lag、namespace 隔离和数据库完整性。
9. 原子切换根数据库，把旧 L2/L3/L4 与 `graph/graph.db` 移入版本化备份。
10. 启动新版本，执行 `memory doctor`、catalog/detail/source smoke test 和 Web 页面 smoke test。

任何门槛失败都停止切换并恢复 `.pre-graph-first` 数据库、旧图数据库和旧二进制。迁移器不得自动删除备份。

## 测试与验收

实施使用测试驱动开发，先写失败测试再写最小实现。

### 单元测试

- 人格 ID、scope、workspace 和 branch 验证。
- 状态转换、expected version、幂等提交、restore 和 tombstone。
- L1 locator 边界、hash、缺失来源和错误引用拒绝。
- checkpoint 失败不推进、失效 revision 重建。
- trust/status/scope 对召回和注入的过滤。
- catalog 排序、`max_results`、中文 n-gram 和 token budget。

### 集成测试

- L1 → Memory → outbox → Graph → catalog → detail → resolve_refs 完整链。
- Web generation 编辑/重试后旧条目退出召回，新条目可检索。
- Graph 落后时降级，追平后恢复关系结果。
- CLI 与 Web 对同一记忆返回相同 DTO、版本和状态。
- Web 筛选、详情、来源、编辑、确认、遗忘、恢复、冲突和重建任务。
- 真实目录结构的脱敏迁移、重复运行、中断续跑和完整回滚。

### 完成门槛

- 当前 208 份 L1 全部保留且哈希不变。
- 默认人格产生非空 canonical Memory 和 Graph 投影。
- 所有派生条目都有有效 scope、trust、version、status、provenance 和 SourceRef。
- 生产路径不再调用旧 `recall_for_context()`，也不再打开 `graph/graph.db`。
- `ContextBuilder`、max results、项目/分支 scope 和状态过滤真实生效。
- 后台失败可查看、可重试且不推进 checkpoint。
- 定向 crate 测试、Web JS 测试、`cargo fmt --check` 和相关 Clippy 通过；最后执行 workspace 测试。
- 迁移、烟雾测试和回滚演练形成可审计记录。

## 预计代码落点

- `rust/crates/knowledge-core`：公共 Memory DTO、状态/schema、catalog/detail 查询契约。
- `rust/crates/brain-memory`：Brain Memory adapter、批次事务、增量 checkpoint、admin/runtime 服务。
- `rust/crates/brain-graph`：Memory outbox 投影和 namespace 限定重建。
- `rust/crates/ai-brain-cli/src/orchestrator.rs`：改接 `BrainMemoryRuntime`，统一召回和注入。
- `rust/crates/ai-brain-cli/src/command/memory_cmd.rs`：替换 placeholder，接入 `BrainMemoryAdmin`。
- `rust/crates/ai-brain-cli/src/api_server.rs`：Web memory HTTP handlers 和路由。
- `rust/crates/ai-brain-cli/src/web/static/index.html`：新增记忆管理 view。
- `rust/crates/ai-brain-cli/src/web/static/app.js`：页面状态、请求、治理与任务轮询。
- `rust/crates/ai-brain-cli/src/web/static/style.css`：桌面三栏与移动端布局。
- Web 静态测试与相关 Rust crate 测试：覆盖共享 DTO、行为一致性和页面关键交互。

## 实施顺序

```text
人格路径安全与后台任务可观测
  → 公共领域模型与 schema
  → BrainMemoryRuntime / BrainMemoryAdmin
  → 增量浓缩与原子批次提交
  → Memory outbox 到 Graph 投影
  → 三阶段生产召回与安全注入
  → CLI/TUI/模型工具统一
  → Web API 与记忆管理页
  → 迁移器、doctor、演练与一次性切换
```

各阶段必须保持旧数据可读和测试可运行；正式切换前不删除旧文件，任何迁移验收失败都回滚。
