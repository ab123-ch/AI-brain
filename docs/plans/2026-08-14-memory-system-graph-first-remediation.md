# 智脑记忆系统 Graph-first 整改实施方案

> 日期：2026-08-14  
> 状态：待实施  
> 前置分析：`docs/architecture/2026-08-14-memory-system-current-state-analysis.md`  
> 决策：Graph-first 收敛、一次性停机迁移、不引入 DSH 插件运行时依赖

## 1. 目标与非目标

### 1.1 目标

1. 让当前 208 份及后续 L1 会话稳定产生可检索、可追溯的长期记忆。
2. 将人格金字塔与通用 Memory/Graph 收敛为一条生产链。
3. 所有派生记忆必须有精确来源、作用域、信任等级、版本和生命周期。
4. 召回采用 catalog→detail→resolve_refs，控制上下文成本并保留原文证据。
5. 提供单条记忆治理、可观测后台任务、安全注入和可回滚迁移。

### 1.2 非目标

- 不把任何 DSH 插件作为运行依赖。
- 首期不强制接入 embedding 服务；先完成透明、确定性的关键词检索。
- 不删除 L1 原文和旧数据库；至少保留一个完整回滚版本。
- 不重构 Novel 业务模型，只保证其现有 namespace 和投影数据不受损。

## 2. 目标架构

```text
                         ┌───────────────────────────────┐
每轮对话 ──append──────▶ │ L1 per-persona JSONL          │
                         │ 原始证据、generation scope     │
                         └───────────────┬───────────────┘
                                         │ 增量浓缩
                                         ▼
                         ┌───────────────────────────────┐
                         │ memory.db                     │
                         │ 唯一派生事实源                 │
                         │ entry/version/trust/scope/ref │
                         └───────────────┬───────────────┘
                                         │ outbox
                                         ▼
                         ┌───────────────────────────────┐
                         │ graph.db                      │
                         │ 可重建目录、关系、证据投影     │
                         └───────────────┬───────────────┘
                                         │
                     ┌───────────────────┴──────────────────┐
                     ▼                                      ▼
            catalog/detail/refs                    L4/Profile/Eval Cache
            按需召回原始证据                         有界、可重建、非事实源
```

### 2.1 存储职责

| 数据 | 事实源 | 说明 |
|---|---|---|
| 原始对话与工具轨迹 | L1 JSONL | 不可变证据；失效 generation 移出 active L1 |
| 摘要、经验、画像、踩坑、规则 | 根 `memory.db` | 事务、版本、scope、trust、retention、status |
| 目录、实体关系、来源关系 | 根 `graph.db` | 从 Memory outbox 幂等投影，可全量重建 |
| 潜意识、启动注入文本 | cache | 只从 active/authorized MemoryEntry 生成 |
| 旧 L2/L3 JSON、`graph/graph.db` | migration backup | 切换后停止读写，不立即删除 |

### 2.2 固定标识

- Tenant：`local-user`。
- Namespace：`ai-brain.memory`。
- Scope 类型：
  - `persona`：必有，key 为人格 ID。
  - `project`：可选，key 为 canonical workspace identity。
  - `git_branch`：可选，必须同时具备对应 project scope。
- Memory 类型：
  - `conversation.task_summary`
  - `experience.rule`
  - `persona.profile`
  - `evaluation.requirement`
  - `evaluation.pitfall`
  - `evolution.rule`
- Trust：
  - 原始用户明确要求：`UserConfirmed`。
  - 领域配置/人工导入：`DomainConfirmed`。
  - LLM 浓缩结果：`ModelGenerated`。
  - 未验证外部内容：`Untrusted`。
- Retention：画像与用户确认规则为 `Permanent`；任务摘要和经验为 `LongTerm`；会话级临时推断为 `Task`。

## 3. 公共接口调整

### 3.1 统一运行时入口

新增 `BrainMemoryService`，替代 Orchestrator 直接操作 `PyramidMemoryBrain` 的方式。对外只暴露：

```rust
pub trait BrainMemoryRuntime: Send + Sync {
    fn append_turns(&self, request: AppendTurnsRequest) -> Result<AppendReceipt>;
    fn invalidate(&self, request: InvalidateMemoryRequest) -> Result<InvalidationReceipt>;
    fn search_catalog(&self, request: MemoryCatalogRequest) -> Result<Vec<MemoryCatalogItem>>;
    fn get_detail(&self, request: MemoryDetailRequest) -> Result<MemoryDetail>;
    fn resolve_refs(&self, request: ResolveMemoryRefsRequest) -> Result<Vec<ResolvedMemorySource>>;
    fn build_inject_context(&self, request: InjectContextRequest) -> Result<ContextSnapshot>;
    fn diagnostics(&self) -> Result<MemoryDiagnostics>;
}
```

约束：

- Orchestrator、TUI、Web、工具和副脑只依赖该 trait。
- 旧 `recall_for_context()`、`search()` 和直接 `PyramidMemoryBrain` 锁调用不再进入生产路径。
- 所有请求必须携带 persona、workspace、branch 和授权 scope；禁止内部猜测跨人格权限。

### 3.2 批次记忆提交

扩展 `MemoryCommandPort`：

```rust
fn submit_batch(&self, batch: MemoryProposalBatch) -> Result<Vec<MemoryEntry>>;
fn restore(&self, memory_entry_id: &str, expected_version: u64) -> Result<MemoryEntry>;
fn tombstone(&self, memory_entry_id: &str, expected_version: u64, reason: &str)
    -> Result<MemoryEntry>;
```

`MemoryProposalBatch` 必须包含：

- `batch_id` 和 L1 输入 revision/hash。
- 全部 MemoryProposal。
- 被替代条目的 expected version。
- consolidation checkpoint。

同一批次的 entry、source、scope、tombstone、outbox 和 checkpoint 在 `memory.db` 单事务提交。LLM 输出在事务外生成并校验，不能持有数据库事务等待网络。

同时给 `MemoryStatus` 增加 `Archived`：

- `Active`：参与正常检索和按策略注入。
- `Archived`：默认检索不返回，仅在显式包含归档时可见。
- `Superseded`：被新版本替代或执行 forget，不参与正常召回，可恢复。
- `Tombstoned`：逻辑删除，不允许恢复为同一版本。

### 3.3 Graph 查询契约

在 `knowledge-core` 增加 catalog/detail 查询语义，复用现有 `GraphQueryPort`：

- `GraphQueryKind::Catalog { terms, memory_types }`
- `GraphQueryKind::Detail { subject }`
- 继续使用 `SourcesFor` 完成来源枚举。

Catalog 只返回：node ID、title、type、matched terms、score、trust、source count 和一行 hint。不得返回完整原文或大段关系。

Detail 返回中心摘要、直接关系、版本、trust、status 和 source references。原文只能通过 `resolve_refs()` 读取。

### 3.4 管理接口

新增独立的 `BrainMemoryAdmin` 服务，由 CLI、TUI、Web 和模型管理工具共用；热路径 `BrainMemoryRuntime` 不承担导出、重建和 purge。管理服务统一提供：

```text
memory list/search/show
memory save/update
memory archive/forget/restore/delete
memory sources/expand
memory rebuild/status/doctor/export
```

语义：

- `archive`：降低默认召回可见性，可恢复。
- `forget`：进入 Superseded，不自动注入，可恢复。
- `delete`：写 Tombstone；默认不物理删除 L1 证据。
- 物理删除涉及 L1 时必须单独执行 purge，并显示不可恢复确认。
- CLI、TUI、Web 和模型工具必须调用同一服务，不保留 placeholder handler。

## 4. 实施任务

### P0-1：安全和可观测性前置修复

涉及：`persona_manager.rs`、`pyramid_storage.rs`、`orchestrator.rs`。

- 复用 `ConversationMemoryScope` 的字符规则验证人格 ID，只允许 ASCII 字母、数字、`-`、`_`，长度 1～64。
- 删除人格前 canonicalize 目标和 `personas/` 根，验证目标是根目录严格子级。
- 为后台浓缩建立持久任务表：task_id、persona、input revision、status、attempt、started/finished、model、error。
- 不再丢弃 JoinHandle；panic、取消、模型创建失败和解析失败均落状态与结构化日志。
- 增加 `memory doctor`，报告 L1/L2/DB 数量、最后成功 checkpoint、projection lag 和失败任务。

验收：非法人格 ID 全部拒绝；后台失败可在日志、TUI/Web 状态和 `memory doctor` 中定位。

### P0-2：注册 Brain Memory Schema

涉及：`knowledge-core` schema、`ai-brain-cli` registry、`brain-memory` adapter。

- 注册 `ai-brain.memory` namespace、六类 MemoryType、节点类型和关系类型。
- 关系首期固定为：`derived_from`、`supports`、`contradicts`、`supersedes`、`related_to`、`applies_to`。
- 实现 L1 `SourceRef` 和 `EvidenceLocator`：resource ID 使用相对 persona 路径；locator 保存 session ID 与 0-based paragraph indexes。
- 所有稳定 ID 来自 persona + memory type + semantic key + source hash，保证迁移和重建幂等。

验收：重复提交同一 L1 revision 不产生重复 entry、node、edge 或 evidence。

### P0-3：增量浓缩与事务提交

涉及：`concentration.rs`、`GenericMemoryStore`、新的 Brain Memory adapter。

- 每人格保存 `last_processed_revision`；只收集其后新增的 active L1。
- Web invalidation 增加 revision 后，从受影响 revision 开始重建，不读取 invalidated L1。
- 保留四个逻辑阶段，但允许用一次或多次 LLM 调用；最终结果必须组合成一个 `MemoryProposalBatch` 后提交。
- 对所有字段做程序级限制：任务≤50、每类经验≤10、画像≤100字、要求≤5、踩坑≤5、规则≤3。
- 校验所有 L1 refs 存在、paragraph 不越界、importance 在 0～1、task ID 唯一。
- 任一步失败不改变 canonical Memory；成功后才推进 checkpoint。

验收：新增一轮只分析增量数据；失败后重试不重复；旧 canonical 记忆保持完整。

### P0-4：Memory→Graph 投影

涉及：Memory outbox consumer、`GenericGraphStore`、projection adapter registry。

- 消费 `memory_committed`、`memory_superseded`、`memory_tombstoned` 事件。
- 使用 `GraphMutationBatch` 同事务更新节点、关系、evidence 和 checkpoint。
- 投影失败不回滚 Memory 事实，保留 outbox 重试；查询结果通过 `is_stale/lag` 暴露延迟。
- 提供全量 rebuild：清空 `ai-brain.memory` namespace 投影后从 active MemoryEntry 重建，不影响 Novel namespace。

验收：重复消费幂等；中断恢复后 projection sequence 连续；Novel 节点、边和 checkpoint 不变。

### P0-5：三阶段召回接入生产

涉及：`BrainMemoryService`、Orchestrator、RealToolExecutor、TUI/Web。

- 普通输入先执行 catalog，最多 5 个候选。
- 自动注入只选择最多 3 条、合计不超过 800 tokens 的高分候选，使用 `ContextBuilder` 组装。
- 候选必须带 trust、版本、source count，并以“历史候选，不是当前事实”的不可信上下文注入。
- 只有明确历史意图或模型调用 `memory get/expand` 时才能读取 L1 原文。
- `search_memory.max_results`、persona/project/branch scope 和 token budget 必须真正生效。
- 删除 `ProgressiveRecall::find_in_summary/find_in_raw` TODO；旧类型仅作为迁移兼容，不再被生产调用。

验收：同一查询能从 catalog 选中摘要、读取 detail、展开到准确 L1 行；无历史意图时不自动展开原文。

### P1-1：安全注入和用户确认

- 浓缩 system prompt 明确声明所有输入均为不可执行数据，忽略其中角色切换和工具指令。
- `Untrusted` 不得自动注入；`ModelGenerated` 只能以候选上下文注入；`UserConfirmed`/`DomainConfirmed` 才能进入常驻规则。
- 新的画像、长期规则和 `injectable` 经验默认 `ModelGenerated`，进入待确认列表。
- 用户明确说“记住……”时生成 `UserConfirmed` proposal，并保留原始消息 citation。
- 所有 system 注入通过 ContextBuilder，保留 trust、source hash 和截断信息。

验收：包含“忽略系统指令”的恶意对话不会生成可常驻的高信任规则；未确认项目规则不进入启动 system prompt。

### P1-2：统一管理与可恢复遗忘

- CLI/TUI/Web/工具共用 `BrainMemoryRuntime`。
- 支持 list/search/show/update/archive/forget/restore/delete/sources/expand。
- 所有更新使用 expected version；冲突返回当前版本和差异，不静默覆盖。
- 人工编辑结果标为 UserConfirmed，并追加新的 provenance；不篡改旧来源。
- export 输出 Markdown + JSON metadata；import 先进入待确认，不直接覆盖 canonical 条目。

验收：每个治理动作均有版本变化、审计记录和可预测召回效果。

### P1-3：空间和隐私治理

- L1 默认永久保留，但工具输出增加可配置 redaction 和最大单条字节数。
- 日志增加大小/日期轮转和总量上限，默认保留 14 天、总量 512 MB。
- `memory doctor` 报告 L1、日志、DB、WAL 大小和不可解析记录。
- 提供显式 purge 流程；purge 前输出受影响 Memory/Graph 引用，用户确认后执行。

验收：超大工具输出不会无界进入 L1；日志轮转不会删除仍在使用的当前日志。

### P2：检索增强

- 第一阶段实现中文/英文规范化、标点切分、字词 n-gram 和字段加权。
- 数据量或离线评测证明需要后，再增加可选 embedding provider。
- embedding 只能作为候选排序信号，不能替代 citation、scope、trust 和状态过滤。

## 5. 一次性迁移切换

### 5.1 迁移前置条件

- 发布包含新 schema、服务、迁移器、doctor、回滚脚本的同一版本二进制。
- 新旧版本都通过定向测试和迁移 fixture。
- 明确维护窗口，停止当前 Web/CLI 进程，禁止迁移期间继续写 L1。

### 5.2 迁移步骤

1. 获取进程锁并确认没有 `ai-brain.exe` 写入 `~/.ai-brain`。
2. 创建 `backups/memory-graph-first-YYYYMMDD-HHMMSS/`，复制：
   - personas、sessions、legacy memory。
   - `memory.db*`、`graph.db*`、`graph/graph.db`。
   - persona registry 和配置；敏感配置只记录 hash，不写入报告。
3. 生成 manifest：相对路径、大小、SHA-256、L1 session/turn 数、数据库 schema 与行数。
4. 对原数据库执行 `PRAGMA integrity_check`；逐行解析所有 active L1。
5. 通过 SQLite backup API 创建 `memory.migration.db` 和 `graph.migration.db`，保留全部现有 Novel/协作数据。
6. 在迁移副本注册 `ai-brain.memory` schema。
7. 按 persona、session、固定批量遍历 208 份 L1；每批记录输入 hash、checkpoint 和 LLM 结果，可断点续跑。
8. 写入 canonical Memory，再消费 outbox 生成 Graph；重建 L4/Profile/Eval cache。
9. 校验 manifest、引用解析率、Memory/Graph 数量、projection lag、Novel namespace 行数和 DB integrity。
10. 将原根数据库重命名为 `.pre-graph-first`，原子提升迁移副本；`graph/graph.db` 移入备份。
11. 启动新二进制，执行 smoke test 和 `memory doctor`。

### 5.3 切换门槛

以下条件必须全部满足：

- 所有 active L1 文件 hash 与迁移前一致。
- L1 解析成功率 100%；坏行必须先人工处理，不能跳过。
- 每个 active 派生条目至少有一个可解析 SourceRef。
- `memory.db` 和 `graph.db` integrity check 为 `ok`。
- Graph projection lag 为 0。
- Novel/协作 namespace 的行数和关键 fixture 查询不变。
- 默认人格能返回 catalog、detail 和至少一段 L1 原文。
- 启动注入在预算内，且只含允许的 trust 等级。

### 5.4 回滚

若任一门槛失败：

1. 立即停止新进程。
2. 保存失败数据库和迁移日志用于诊断。
3. 恢复 `.pre-graph-first` 数据库、旧 `graph/graph.db` 和旧二进制。
4. 对恢复数据库执行 integrity check。
5. 启动旧版本并验证 L1 写入与 Novel 查询。

迁移成功后也至少保留备份一个发布周期；不得由迁移器自动删除。

## 6. 测试计划

### 6.1 单元测试

- 人格 ID、scope、workspace、branch 验证。
- 批次事务、幂等 key、expected version、restore/tombstone。
- 增量 checkpoint、失败不推进、失效 revision 重建。
- L1 locator 边界、hash 校验、缺失来源拒绝。
- trust/retention/status 对自动注入和召回的过滤。
- catalog 排序、max_results、token budget、中文 n-gram。

### 6.2 集成测试

- L1→Memory→outbox→Graph→catalog→detail→resolve_refs 完整链。
- Web generation edit/retry 后旧派生条目 supersede，新条目可召回。
- 人格、项目和 git branch 隔离。
- Graph 落后时 Memory 查询降级，追平后恢复关系结果。
- LLM 超时、非法 JSON、超限结果、错误引用和后台 panic。
- CLI、TUI、Web 和模型工具对同一记忆返回一致状态。

### 6.3 安全测试

- `../`、绝对路径、Windows 分隔符和保留名人格 ID。
- 对话内 prompt injection 不得升级为 UserConfirmed。
- 跨人格、跨项目、跨 tenant 查询必须拒绝。
- purge 不能删除 `~/.ai-brain/personas` 之外的任何路径。

### 6.4 迁移与回滚测试

- 复制真实目录结构的脱敏 fixture 完整迁移。
- 中途终止后断点续跑，无重复条目。
- 第二次执行迁移得到相同 ID、hash 和数量。
- Novel/协作数据迁移前后保持一致。
- 人为制造校验失败，验证原数据库不被替换。
- 完成切换后执行一次完整回滚演练。

### 6.5 性能基线

- 208 个现有 L1 的迁移耗时、LLM 调用次数和 token 使用必须记录。
- 日常增量浓缩不得读取已 checkpoint 的完整历史。
- Catalog P95 目标：本机 100 ms 内；detail 200 ms 内；L1 resolve 300 ms 内，不含外部 LLM。
- 每轮自动记忆注入上限 800 tokens、3 条；启动常驻注入上限 1,200 tokens。

## 7. 最终验收标准

- 本机 208 份 L1 全部保留，hash 不变。
- 默认人格产生非空 canonical Memory 和 Graph 投影。
- 所有派生记忆有 persona scope、trust、version、status、provenance 和有效来源。
- 主流程、工具和 TUI 不再调用旧 `recall_for_context()`。
- `ProgressiveRecall` 的 L2/L1 TODO 被删除或整个旧实现退出编译。
- `graph/graph.db` 不再被生产代码打开。
- `max_results`、ContextBudget、project/branch scope 实际生效。
- 记忆创建、更新、忘记、恢复、删除、来源展开均有集成测试。
- 后台分析失败可观察、可重试，不推进 checkpoint。
- `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全部通过。
- 迁移、烟雾测试和回滚演练均形成可审计日志。

## 8. 实施顺序

```text
P0-1 安全/可观测
  → P0-2 Schema
  → P0-3 增量浓缩
  → P0-4 Graph 投影
  → P0-5 生产召回
  → P1-1 安全注入
  → P1-2 管理面
  → P1-3 空间隐私
  → 迁移演练
  → 停机迁移切换
  → P2 检索增强
```

任何 P0 验收失败都不得进入正式数据迁移；任何迁移门槛失败都必须回滚，不允许临时启用新旧双写继续运行。
