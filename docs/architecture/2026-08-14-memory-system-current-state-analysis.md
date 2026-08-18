# 智脑记忆系统现状分析

> 审计日期：2026-08-14  
> 审计对象：AI Brain Rust 运行时的对话长期记忆、人格记忆、图记忆与通用知识存储  
> 审计方法：设计文档对照、生产调用链静态审查、本机运行数据只读盘点、定向测试  
> 结论口径：严格区分“设计目标”“源码存在”“生产接线”“本机实际生效”

## 1. 执行摘要

智脑目前不是单一记忆系统，而是三条部分重叠的链路：

1. `brain-memory` 的人格隔离四层金字塔，负责对话原文、任务摘要、经验、潜意识和画像。
2. `brain-graph::GraphStore` 的旧式图记忆，接收 L2 摘要镜像，数据库位于 `~/.ai-brain/graph/graph.db`。
3. `GenericMemoryStore`、`GenericGraphStore` 和 `ContextBuilder` 组成的通用知识层，数据库位于根目录 `memory.db`、`graph.db`，目前主要服务 Novel 投影和协作上下文。

架构目标有价值：L1 保留证据、上层逐步浓缩、人格隔离、自动注入、按需下钻、评估反馈和图关系都已经有对应模块。但生产现状存在核心断裂：

- 本机默认人格有 208 个 L1 JSONL，L2、L3 均为空，L4 和 Profile 不存在。
- 主流程使用的 `ProgressiveRecall` 只实现 L4 触发词和不完整的 L3 匹配，L2、L1 下钻仍是 TODO。
- 更完整的 `recall_graph_memory_context()` 只有定义和测试调用，没有生产调用。
- 分析任务虽在每次成功查询后计数触发，但模型创建失败和后台任务退出缺乏可靠状态与告警。
- 普通派生 JSON 直接覆盖写，和通用 SQLite 层已有的 WAL、事务、版本、来源、信任与 outbox 能力形成明显能力倒挂。

因此，当前生产效果应定性为：**L1 原始会话持久化已工作，长期记忆的派生、检索和治理未形成闭环**。

## 2. 审计范围与证据

### 2.1 代码范围

| 子系统 | 主要文件 | 审计重点 |
|---|---|---|
| 金字塔入口 | `brain-memory/src/pyramid_memory_brain.rs` | 写入、浓缩、失效、注入、召回、图镜像 |
| 四层存储 | `raw_pool.rs`、`summary_pool.rs`、`abstract_layer.rs`、`subconscious_pool.rs` | 数据格式、覆盖策略、容量与来源 |
| 召回 | `progressive_recall.rs` | L4→L1 下钻、结果限制、匹配质量 |
| 分析 | `concentration.rs`、`prompts.rs` | 全量读取、LLM 调用、错误与提交边界 |
| 人格与文件 | `persona_manager.rs`、`pyramid_storage.rs` | 隔离、路径验证、原子性 |
| 运行时接线 | `ai-brain-cli/src/orchestrator.rs` | 启动注入、逐轮召回、保存、后台分析 |
| 管理入口 | `command/memory_cmd.rs`、`tui/app.rs`、`real_tool_executor.rs` | 命令是否真实可用、CRUD 能力 |
| 通用知识层 | `brain-memory/src/generic.rs`、`brain-graph/src/generic.rs`、`knowledge-core/src/context.rs` | 事务、作用域、来源、投影、预算 |

### 2.2 运行时快照

审计时观察到运行中的 Web 进程使用：

```text
D:\rustObject\AI-brain\rust\target\release\ai-brain.exe web --addr 127.0.0.1:8080
```

`~/.ai-brain` 只读统计：

| 指标 | 结果 |
|---|---:|
| 总文件 | 317 |
| 总大小 | 约 278.3 MB |
| 日志 | 约 222.0 MB |
| JSON | 约 46.3 MB |
| SQLite/WAL/SHM | 约 8.26 MB |
| JSONL | 约 1.68 MB |
| 默认人格 L1 文件 | 208 |
| 默认人格 L2 文件 | 0 |
| 默认人格 L3 文件 | 0 |
| 默认人格 L4 | 不存在 |
| 默认人格 Profile | 不存在 |

数据库入口同时存在：

| 路径 | 大小 | 当前职责 |
|---|---:|---|
| `~/.ai-brain/memory.db` | 102,400 B | 通用 MemoryStore，主要含 Novel/通用知识数据 |
| `~/.ai-brain/graph.db` | 135,168 B | 通用 GenericGraphStore |
| `~/.ai-brain/graph/graph.db` | 53,248 B | 金字塔 L2 镜像使用的旧 GraphStore |

这说明当前运行进程持续生成 L1，但派生层没有形成实际数据；同时两套图存储并存。

## 3. 当前架构与真实数据流

### 3.1 设计目标

```text
用户/助手/工具轨迹
       │
       ▼
L1 Raw Pool                  每人格 JSONL，原始证据
       │ Step 1
       ▼
L2 Summary Pool              任务摘要、标签、importance、L1Ref
       │ Step 2
       ▼
L3 Abstract Layer            类型经验、关键词索引、injectable
       │ Step 3
       ▼
L4 Subconscious              触发词、叙事
       │
       ├── 启动注入主脑
       ├── Profile/EvalInfo 注入主脑和评估脑
       └── L2 镜像到 graph/graph.db
```

### 3.2 生产写入链

`orchestrator.rs:1456-1466` 在 v2 查询成功后，将 `MainBrainOutput.turns` 写入记忆脑。Web 编辑/重试路径调用 `store_turns_scoped()`，普通路径调用 `store_turns()`。

`pyramid_memory_brain.rs:582-604` 最终委托 `RawPool::append_turn()`，追加到：

```text
personas/{persona_id}/pyramid/l1-raw/{session_or_generation}.jsonl
```

优点：

- 完整保留用户、助手和可选工具输出。
- Web generation 拥有独立 L1 文件，可以精确失效。
- L1 不依赖派生层成功，分析失败不会丢失原始会话。

问题：

- L1 无容量、保留期、加密或敏感字段过滤。
- JSONL 追加没有文件锁和 `sync_all()`；多进程写同一文件的边界不明确。
- 工具输出可能包含凭据、文件正文或外部隐私数据，并被长期保留。

### 3.3 Web 分支失效链

`conversation_memory.rs:118-189` 是当前最可靠的记忆一致性实现：

- 先原子写入 `derived_stale=true` 和 revision。
- 将失效 generation 的 L1 文件移动到 `l1-invalidated`。
- 派生层在重建并确认 revision 未变化后才清除 stale。
- 遇到无法精确删除的 legacy unscoped 数据时保持 fail-closed。

`PyramidMemoryBrain::auto_inject()`、`recall()`、`load_profile()` 等都会在 stale 时返回空结果，避免旧分支继续影响模型。

不足是该事务保护只覆盖“分支失效状态”，没有覆盖 L2/L3/L4/Profile 的普通重生成。

### 3.4 四步浓缩链

`pyramid_memory_brain.rs:607-647` 负责计数和启动 `ConcentrationEngine`。默认人格的 `analysis_interval` 为 5。

`concentration.rs:84-163` 顺序执行：

1. L1→L2 任务拆分。
2. L2→L3 经验抽象。
3. L3→L4 触发词与叙事。
4. Profile 与 EvalInfo。

关键实现事实：

- `concentration.rs:166-174` 每次读取当前人格的所有 L1 会话，再整体序列化。
- 每一步是独立 LLM 调用。
- Step 1 失败会终止；Step 2～4 失败只记录错误并继续。
- L2、L3、L4 和 Profile 各自覆盖写，没有跨层事务。
- L2 最多 50 项、L3 每类最多 10 条、Profile 100 字主要依赖 prompt；只有 L4 在存储层硬截断为 50 个触发词和 500 字。

运行时问题：

- `orchestrator.rs:2482-2527` 丢弃 `tokio::spawn` 的 JoinHandle。
- `create_analyzer_llm_with_config()` 返回 `Option`，周期分析分支在创建失败时不记录原因。
- 没有持久化任务状态、失败次数、输入 revision 或最后成功 checkpoint。
- 随 L1 增长，每五轮重新发送全部历史，成本和上下文占用线性上升。

### 3.5 启动注入链

`orchestrator.rs:895-941` 在启动时拼接：

- 人格 system prompt。
- L4 潜意识。
- 用户画像。
- L3 `injectable=true` 的经验。

随后通过 `MainBrain::inject_memory_context()` 进入 system prompt。派生层为空或 stale 时不注入。

风险：

- 原始对话直接放进分析调用的 user prompt，`prompts.rs:1002-1106` 没有明确声明“输入仅为不可执行数据”。
- 模型生成的画像、潜意识和经验会进入更高权限的 system context。
- 没有用户确认、信任阈值、citation 展示或内容安全策略。
- 自动注入没有统一使用 `ContextBuilder` 的 token budget。

### 3.6 主流程召回链

`orchestrator.rs:1389-1429` 每轮调用：

```rust
mem.recall_for_context(&input_owned, 3)
```

`pyramid_memory_brain.rs:1043-1075` 又调用 `ProgressiveRecall::recall(query, PyramidLayer::Summary)`，但：

- `_max` 参数被忽略。
- 所有结果被伪装成 `MemoryLayer::Raw`。
- importance、confidence 被写成固定值。
- 错误被折叠为空结果。

`progressive_recall.rs:89-177` 的实际能力：

- L4：对触发词做小写子串匹配。
- L3：判断 L4 命中文本是否包含某个 experience pattern；它没有使用 L3 的关键词索引，也没有直接匹配用户 query。
- L2：`TODO`，始终返回空。
- L1：`TODO`，始终返回空。

因此“渐进式召回 L4→L3→L2→L1”目前只是接口形状，不是完整功能。

### 3.7 图记忆链

`pyramid_memory_brain.rs:676-702` 将 L2 摘要镜像为旧 `GraphStore` 节点和关系；`708-778` 实现了较合理的显式历史召回：

- 只有输入包含“之前、上次、记得、历史”等意图才召回。
- 先做 catalog 搜索。
- 再读取节点详情和 L1 来源片段。
- 注入文本明确提醒“候选记忆不是必然事实”。

但全 workspace 调用检索显示，`recall_graph_memory_context()` 除定义外只有单元测试调用。生产主流程、TUI 和 `search_memory` 工具仍走不完整的 `recall_for_context()`。

### 3.8 通用知识层

`brain-memory/src/generic.rs` 的 `GenericMemoryStore` 已具备：

- SQLite WAL。
- 单写线程和事务。
- idempotency key。
- tenant、namespace、owner scope 和 visibility scopes。
- provenance、trust、retention、version、status。
- supersede、tombstone 和 outbox。

`brain-graph/src/generic.rs` 已具备：

- 幂等 `GraphMutationBatch`。
- projection checkpoint。
- 节点、边、evidence link 和 tombstone 的同事务提交。
- tenant/namespace/scope 权限过滤。

`knowledge-core/src/context.rs:317-449` 的 `ContextBuilder` 已具备 memory、graph、optional、total 和 item 数预算，并在 ContextBlock 中保留来源、版本、hash、trust 和截断状态。

当前不足：这些能力没有成为对话人格记忆的统一后端，生产写入主要来自 Novel 投影，ContextBuilder 主要用于协作任务快照。

### 3.9 管理面

TUI 对 `:memory stats/recall/save/daily` 有实际处理，但 recall 仍走不完整链路；手动 save 只是在 L1 追加一条 `[手动保存]` 用户消息。

`ai-brain-cli/src/command/memory_cmd.rs:7-35` 的通用 command registry 仍返回 placeholder。系统没有统一的：

- 单条查看和来源展开。
- 更新、归档、忘记、恢复、删除。
- 导入、导出、重建、完整性检查。
- 分析任务状态和失败诊断。

删除整个人格是目前唯一粗粒度物理遗忘方式。

## 4. 问题清单

### P0：核心功能或安全断裂

| ID | 问题 | 代码证据 | 影响 |
|---|---|---|---|
| MEM-P0-01 | 生产派生记忆为空 | 本机 L1=208，L2/L3=0，L4/Profile 缺失 | 启动注入和长期召回基本无数据 |
| MEM-P0-02 | L2/L1 渐进召回未实现 | `progressive_recall.rs:168-177` | 无法从触发词下钻到摘要和原文 |
| MEM-P0-03 | 完整图召回未接生产链 | `pyramid_memory_brain.rs:708`；仅测试调用 | 已有证据召回能力无法被用户使用 |
| MEM-P0-04 | 人格 ID 未做路径验证 | `persona_manager.rs:76-99,103-123` | 特制 ID 可能使递归删除越出人格目录 |

### P1：可靠性、成本和治理缺口

| ID | 问题 | 代码证据 | 影响 |
|---|---|---|---|
| MEM-P1-01 | 周期分析失败不可观测 | `orchestrator.rs:2482-2527,2596-2600` | 无法解释为何长期没有派生数据 |
| MEM-P1-02 | 每次分析读取全部 L1 | `concentration.rs:166-174` | 成本、延迟和上下文随历史增长 |
| MEM-P1-03 | 派生层非原子、多文件分步覆盖 | `pyramid_storage.rs:155-177` | 崩溃时层级不一致或 JSON 截断 |
| MEM-P1-04 | 长期提示污染 | `prompts.rs:1098-1106` + 启动注入 | 对话中的指令可能被固化到 system context |
| MEM-P1-05 | 缺少单条治理 | command/TUI/工具接口 | 无法可靠更正、忘记、恢复和审计记忆 |
| MEM-P1-06 | L1 工具输出永久保存 | `raw_pool.rs:40-54` | 敏感数据、磁盘与合规风险 |
| MEM-P1-07 | 自动注入绕过统一预算 | `orchestrator.rs:895-929` | 注入体积和信任策略不统一 |

### P2：架构债务和体验问题

| ID | 问题 | 影响 |
|---|---|---|
| MEM-P2-01 | `memory.db`、`graph.db`、`graph/graph.db` 多轨并存 | 备份、迁移、一致性和故障定位复杂 |
| MEM-P2-02 | 旧交接文档称 ProgressiveRecall 已完成，但源码仍有 TODO | 评审和项目状态失真 |
| MEM-P2-03 | 关键词子串匹配，无中文分词、FTS 或语义检索 | 同义表达召回率低 |
| MEM-P2-04 | 通用命令与 TUI 行为不同 | CLI/API/模型看到的能力不一致 |
| MEM-P2-05 | 日志约 222 MB，占总目录约 80% | 缺少日志轮转和空间治理 |

## 5. 与 DSH 记忆插件的能力对照

| 方案 | 存储与召回 | 治理 | 相比智脑当前状态 |
|---|---|---|---|
| [dsh-memory](https://github.com/Jesse-njx/dsh-memory) | Markdown，索引注入，正文和原始 session citation 按需展开 | list/show/edit/delete、蒸馏审计 | 架构较窄，但证据与可审计性明显更成熟 |
| [dsh-mneme](https://github.com/modusensus/dsh-mneme) | SQLite + Markdown 镜像，全文/可选向量检索 | CRUD、forget、archive、冲突合并、Web UI | 当前综合可用性和治理能力更强 |
| [dsh-auto-memory](https://github.com/Aik358/dsh-auto-memory) | user/project/daily Markdown，每轮有界注入 | 可见工具、日历、外部记忆导入 | 简单透明，但证据、冲突和事务较弱 |
| [dsh-memory-evolve](https://github.com/csyangwen/dsh-memory-evolve) | 五轨记忆、项目/全局/每日、git 分支过滤 | 建议确认、归档、同步、丰富 UI | 覆盖最广，但复杂度和安全面也最大 |

智脑最值得吸收的不是某个插件整体，而是：

- `dsh-memory` 的 citation-first 与原文展开。
- `dsh-mneme` 的 CRUD/forget、冲突裁决和人工可编辑能力。
- `memory-evolve` 的用户确认与 git 分支作用域。
- `dsh-auto-memory` 的透明、可迁移和用户可直接审查。

## 6. 验证状态

执行：

```powershell
cargo test -p brain-memory -p brain-graph -p knowledge-core
```

结果：

```text
221 passed
0 failed
1 ignored
```

另有一个 `dead_code` warning：`ThresholdCompressor::build_decision_chain_prompt` 未被使用。

测试证明现有模块内部契约大体稳定，但不能证明生产链已经工作，原因包括：

- L2/L1 TODO 没有对应完成性测试。
- 图召回测试直接调用未接生产的函数。
- 浓缩测试使用 Mock LLM 和临时目录，没有覆盖真实运行配置与后台任务生命周期。
- 本机真实派生目录为空，是比单元测试更直接的运行证据。

## 7. 现状结论

当前系统应保留的核心资产：

- L1 原始证据和人格隔离。
- Web generation 失效的 revision-safe、fail-closed 机制。
- 通用 Memory/Graph 的事务、scope、trust、provenance、outbox 和 projection checkpoint。
- 图谱的 catalog/detail/source 三阶段思想。
- ContextBuilder 的预算和可追溯 ContextBlock。

当前不应继续扩建的部分：

- 把 L2/L3 JSON、旧 GraphStore 和 Generic Memory/Graph 同时维护为正式事实源。
- 每五轮全量重传全部 L1。
- 没有来源、确认、预算和失败状态的自动 system 注入。

整改应采用 Graph-first 收敛：L1 继续作为原始证据，根 `memory.db` 成为唯一派生事实源，根 `graph.db` 成为可重建投影，L4 只保留为有界缓存。
