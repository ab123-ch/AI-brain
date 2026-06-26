# 智脑原生知识图谱能力设计文档

| 项 | 值 |
|---|---|
| 日期 | 2026-06-26 |
| 版本 | v1.0 |
| 状态 | 设计已确认，待 writing-plans |
| 范围 | 新增 `brain-graph` crate + 三脑集成 |
| 关联 | v2 三脑架构（main/memory/eval）+ 金字塔记忆系统 |

---

## 1. 概述

### 1.1 背景与目标

智脑 v2 当前已有三脑架构（主脑 + 记忆脑 + 评估脑）+ 四层金字塔记忆系统（L1 全量 → L2 摘要 → L3 经验 → L4 潜意识）。金字塔是**纵向的线**，但缺少**横向的网**——不同任务、不同经验、不同概念之间的语义关联无法表达。

**目标**：新增原生知识图谱能力，作为三脑**公共内置工具集**，让所有脑都能查询、维护、利用图谱数据。

**核心价值**：
1. 让主脑优先用图谱召回（替代直接 Read/Grep）
2. 让记忆脑在四步分析时建立实体-概念-记忆的网络
3. 让评估脑追溯回答质量问题的因果链
4. 让多类业务场景（记忆/代码/小说/视频）有独立的图谱域

### 1.2 核心设计原则

1. **一次调用拿到全貌**：多关键词批量模糊匹配，单次返回混合子图
2. **默认省 token**：每节点 ~15 tokens，整包默认 ≤ 4000 tokens
3. **可下钻**：不够再 drill_down 拿详情
4. **主脑优先用图谱**：强驱动性，靠 system prompt 引导
5. **工具只报结果**：成功/空/失败三态，降级由主脑自决策
6. **数据完整性靠兜底**：不强一致，靠启动 reconciliation + 周期补建
7. **三脑架构强依赖、运行时软依赖**：核心路径必经图谱、DB 不可用时降级

### 1.3 决策汇总表

| # | 决策项 | 选择 |
|---|---|---|
| 1 | 范围 | 四脑共享实体图谱（中档） |
| 2 | 三脑架构 | 主脑 + 记忆脑 + 评估脑（无进化脑，用人格系统） |
| 3 | 节点 NodeKind | Memory / Concept / Entity / Tool / Code（5 类） |
| 4 | GraphType 域 | Memory / Code / Novel / Video + Custom（兜底） |
| 5 | Schema 风格 | 混合（NodeKind 枚举 + props HashMap） |
| 6 | 边类型 EdgeKind | MentionedIn / DependsOn / DerivedFrom / SimilarTo / CausedBy / RelatedTo / Calls / Contains / Imports / Defines / Invokes（11 种） |
| 7 | 存储 | SQLite（WAL 模式），节点只存引用不存原文 |
| 8 | 写入策略 | 三者混合（规则自动 + LLM 抽 + 主脑显式） |
| 9 | 一致性 | 最终一致（不强事务、不阻塞） |
| 10 | 失败处理 | 工具返回三态（Ok/Empty/Err），主脑自决策 |
| 11 | 工具集 | 7 个：graph_recall / graph_drill / graph_list_domains / graph_add_concept / graph_add_entity / graph_add_code_node / graph_link |
| 12 | Token 控制 | 默认 4000、可配置、贪心装填 |
| 13 | 强驱动性 | system prompt 引导主脑优先用图谱 |
| 14 | crate 位置 | `rust/crates/brain-graph/`（独立 crate） |

---

## 2. 整体架构

### 2.1 三脑架构

```
        ┌─────────────────────────────────────┐
        │   brain-graph (三脑公共强依赖)        │
        │   SQLite: ~/.ai-brain/graph.db      │
        └────────┬────────────────────────────┘
                 │ 三脑核心路径必经（架构强依赖）
                 │
   ┌─────────────┼─────────────┐
   ▼             ▼             ▼
 主脑          记忆脑        评估脑
 (主动召回)   (四步分析时   (评估完成
 优先查图谱   自动写入图谱)  写入因果链)
```

### 2.2 强依赖关系（架构层 vs 运行时）

| 层面 | 强/弱 | 说明 |
|---|---|---|
| 架构层 | 强依赖 | 三脑的核心代码路径**设计上必经图谱** |
| 数据完整性 | 强依赖 | 三脑产生的数据**最终写入图谱** |
| 启动 | **软依赖** | DB 不可用 → 降级运行，不阻塞启动 |
| 写入一致性 | **最终一致** | 分开写、可异步、不强求事务 |
| 读取 | **软依赖** | 查询失败可降级直查金字塔 |

### 2.3 三脑各自的强依赖点

**主脑**：
- 收到用户消息 → 必先调 `graph_recall`（强驱动，system prompt 引导）
- 探索代码后（Read/Grep 完成）→ 必调 `graph_add_code_node`
- 任务完成 → 必调 `graph_link` 关联 Memory/Concept/Entity

**记忆脑**：
- 四步分析 Step1（抽实体）→ 必调 `graph_add_concept/entity`
- 四步分析 Step2（建关系）→ 必调 `graph_link`
- 写 L2/L3/L4 时 → 必镜像写入对应 Memory 节点
- importance 衰减 → 必同步到图谱节点 importance 字段

**评估脑**：
- 评估完成 → 必调 `graph_link` 写入因果链
  `(Answer) -[:CausedBy]-> (ToolCall) -[:DerivedFrom]-> (Memory) -[:MentionedIn]-> (PromptSegment)`
- 发现踩坑 → 必建 Concept(Pitfall) + 关联 Memory

### 2.4 失败处理策略（修订：工具不替主脑决策）

**核心原则**：工具只返回结果（成功/空/失败），降级由主脑自主判断。

| 场景 | 处理 |
|---|---|
| 图谱 DB 文件损坏 | 警告日志 + **降级运行**（图谱工具不注册，主脑看不到自然走其他工具） |
| 写入失败（磁盘满/锁） | 日志记录 + **跳过本次写入**（下次有机会再补） |
| 查询超时 | 返回 `ToolResult::Err{Timeout}` + 主脑自主换工具 |
| 节点不存在 | 返回 `ToolResult::Err{NotFound}` + 主脑自主决策 |

### 2.5 crate 位置

```
rust/crates/
├── brain-graph/              # 新增，独立 crate
│   ├── src/                  # 见第 4 节模块清单
│   └── tests/
└── tools/
    └── src/graph_tools.rs    # 7 个工具包装到 RealToolExecutor
```

### 2.6 存储位置

```
~/.ai-brain/
├── graph.db                  # 新增：SQLite 图谱（WAL 模式）
├── graph.db-wal              # WAL 日志
├── graph.db-shm              # 共享内存
└── personas/
    └── {persona_id}/
        └── pyramid/          # 现有金字塔（不动）
```

> 图谱 DB 与 persona 解耦（单一 DB 服务所有人格），人格信息放节点的 `props.persona_id` 字段。

---

## 3. 数据模型

### 3.1 NodeKind 枚举（5 类基础类型）

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Memory,    // 金字塔代理节点（L1/L2/L3/L4）
    Concept,   // 抽象概念
    Entity,    // 具体实体（人物/项目/技术栈/角色）
    Tool,      // 工具/Skill/MCP
    Code,      // 代码节点（File/Function/Class/Module）
}
```

### 3.2 GraphType 枚举（4 + Custom 兜底）

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphType {
    Memory,   // 记忆域
    Code,     // 代码域
    Novel,    // 小说域
    Video,    // 视频域
    Custom(String),  // 兜底（MVP 数据层支持，不开放创建工具）
}
```

**二维分类**：NodeKind（纵向基础类型）× GraphType（横向域）。同一 Entity 在不同域是**独立节点**。

### 3.3 EdgeKind 枚举（11 种）

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    // === 通用语义边 ===
    MentionedIn,    // (Concept/Entity) -[:MentionedIn]-> (Memory)
    RelatedTo,      // 通用兜底关联
    SimilarTo,      // 同类相似
    CausedBy,       // 因果
    DependsOn,      // 依赖
    DerivedFrom,    // 派生

    // === 代码专属边 ===
    Calls,          // Function -[:Calls]-> Function
    Contains,       // File/Class -[:Contains]-> Function/Class
    Imports,        // File -[:Imports]-> File/Module
    Defines,        // File -[:Defines]-> Function/Class/Variable

    // === 工具调用边 ===
    Invokes,        // Memory -[:Invokes]-> Tool
}
```

### 3.4 Node 结构

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,                    // {graph_type}_{kind}_{uuid8}
    pub kind: NodeKind,
    pub graph_type: GraphType,
    pub props: HashMap<String, Value>, // 领域特定字段
    pub importance: f64,               // 0.0-1.0
    pub created_at: i64,               // Unix ms
    pub last_accessed: i64,
    pub superseded: bool,
}
```

### 3.5 Edge 结构

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub src: String,
    pub dst: String,
    pub kind: EdgeKind,
    pub props: HashMap<String, Value>,
    pub created_at: i64,
    pub weight: f64,                   // 0.0-1.0
}
```

### 3.6 各 NodeKind 的推荐 props 字段（约定非强制）

```rust
// Memory（金字塔代理）
{
    "layer": "L1" | "L2" | "L3" | "L4",     // 必填
    "ref_id": "task-033",                    // 必填
    "summary_brief": "ICS2 申报需求开发",     // 必填
    "task_type": "Coding",                   // L2/L3 必填
    "tags": ["EDI", "ICS2"],                 // 可选
}

// Concept
{
    "name": "红冲逻辑",                      // 必填
    "aliases": ["红冲", "资费红冲"],         // 推荐
    "description": "退舱时费用红冲处理",     // 推荐
}

// Entity（Memory 域）
{
    "name": "陈老板",                        // 必填
    "entity_type": "Person"|"Project"|"TechStack"|"Product"|"Company"|"Role",
    "aliases": ["老陈"],                     // 可选
}

// Entity（Novel 域，角色）
{
    "name": "苏墨",                          // 必填
    "entity_type": "Role",
    "novel_id": "dark_city",
    "novel_role": "主角"|"反派"|"配角",
    "first_chapter": 5,
}

// Tool
{
    "name": "brain_plugin",                  // 必填
    "tool_kind": "Builtin"|"Skill"|"MCP",
    "namespace": "plugin-name:skill-name",   // Skill/MCP 专用
}

// Code
{
    "path": "rust/crates/brain-memory/src/...", // 必填
    "code_kind": "File"|"Function"|"Class"|"Module",
    "language": "Rust"|"TypeScript"|"Java",
    "signature": "fn recall(...)",              // Function 推荐
    "line_range": "40-118",                     // 可选
}
```

### 3.7 SubGraph / ScoredNode / DomainInfo

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraph {
    pub nodes: Vec<ScoredNode>,    // 已排序、已裁剪
    pub edges: Vec<Edge>,          // 仅 nodes 列表内节点之间的边
    pub total_found: usize,
    pub truncated: bool,
    pub drill_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredNode {
    pub node: Node,
    pub score: f64,
    pub matched_keywords: Vec<String>,
    pub detail_level: DetailLevel,
}

pub enum DetailLevel {
    Brief,        // ~15 tokens
    WithSummary,  // ~50 tokens
    Full,         // ~150 tokens
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainInfo {
    pub graph_type: GraphType,
    pub node_count: usize,
    pub edge_count: usize,
    pub last_updated: Option<i64>,
    pub description: String,
}
```

### 3.8 ToolResult 三态

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ToolResult<T> {
    #[serde(rename = "ok")]
    Ok { data: T },

    #[serde(rename = "empty")]
    Empty {
        searched_nodes: usize,
        searched_edges: usize,
        hint: Option<String>,
    },

    #[serde(rename = "error")]
    Err { kind: ErrorKind, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorKind {
    DbLocked,
    DbCorrupted,
    Timeout,
    InvalidInput,
    DomainMismatch,    // 边两端节点不同域
    NotFound,          // drill 时节点不存在
}
```

### 3.9 SQLite Schema DDL

```sql
CREATE TABLE nodes (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    graph_type TEXT NOT NULL,
    props TEXT NOT NULL DEFAULT '{}',
    importance REAL NOT NULL DEFAULT 0.5,
    created_at INTEGER NOT NULL,
    last_accessed INTEGER NOT NULL,
    superseded INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_nodes_graph_type ON nodes(graph_type);
CREATE INDEX idx_nodes_kind_type ON nodes(kind, graph_type);
CREATE INDEX idx_nodes_importance ON nodes(importance DESC);
CREATE INDEX idx_nodes_last_accessed ON nodes(last_accessed DESC);

CREATE TABLE edges (
    src TEXT NOT NULL,
    dst TEXT NOT NULL,
    kind TEXT NOT NULL,
    props TEXT NOT NULL DEFAULT '{}',
    weight REAL NOT NULL DEFAULT 0.5,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (src, dst, kind),
    FOREIGN KEY (src) REFERENCES nodes(id),
    FOREIGN KEY (dst) REFERENCES nodes(id)
);

CREATE INDEX idx_edges_src ON edges(src, kind);
CREATE INDEX idx_edges_dst ON edges(dst, kind);

CREATE TABLE schema_meta (
    key TEXT PRIMARY KEY,
    value TEXT
);
INSERT INTO schema_meta VALUES ('version', '1');

-- PRAGMA
-- journal_mode = WAL
-- synchronous = NORMAL
-- busy_timeout = 5000
```

### 3.10 ID 生成规则

```rust
fn gen_node_id(graph_type: GraphType, kind: NodeKind) -> String {
    let prefix = format!("{:?}_{}", graph_type, kind).to_lowercase();
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &uuid[..8])
}

// 示例
"memory_concept_a1b2c3d4"
"code_file_e5f6g7h8"
"novel_entity_i9j0k1l2"
```

**节点 ID vs ref_id**：
- 节点 ID 是图谱内部全局唯一
- Memory 节点的 `props.ref_id` 指向金字塔实体（如 task-033）
- 图谱不存原文，遵循"节点只存引用"原则

---

## 4. 组件模块

### 4.1 模块清单

```
rust/crates/brain-graph/
├── Cargo.toml
├── src/
│   ├── lib.rs              # 对外 API + 重新导出
│   ├── schema.rs           # 类型定义
│   ├── error.rs            # ToolResult / ErrorKind / BrainGraphError
│   ├── storage.rs          # SQLite CRUD 原语（唯一 SQL 层）
│   ├── query.rs            # graph_recall / drill / list_domains
│   ├── scorer.rs           # 评分 + 装填（纯函数）
│   ├── writer.rs           # 写入 API
│   ├── props_schema.rs     # 各 kind 推荐字段 + 校验
│   ├── id.rs               # ID 生成
│   ├── migrations.rs       # SQLite schema 初始化
│   └── extractor.rs        # 规则抽取器（镜像金字塔/工具调用）
└── tests/
    ├── storage_test.rs
    ├── query_test.rs
    ├── scorer_test.rs
    └── integration_test.rs
```

```
rust/crates/tools/src/graph_tools.rs   # 7 个工具包装到 RealToolExecutor
```

### 4.2 各模块职责

| 模块 | 职责 |
|---|---|
| `lib.rs` | 对外门面，定义 `BrainGraph` 主结构，重新导出公共类型 |
| `schema.rs` | 纯类型定义（Node/Edge/NodeKind/EdgeKind/GraphType/SubGraph 等） |
| `error.rs` | `BrainGraphError` + `ToolResult<T>` + `ErrorKind` |
| `storage.rs` | SQLite CRUD 原语，所有 SQL 操作的唯一入口，内部互斥锁保护连接 |
| `query.rs` | `recall` / `drill` / `list_domains` 业务逻辑（编排 storage + scorer） |
| `scorer.rs` | `score_node` + `pack_to_budget` + `time_decay`（纯函数） |
| `writer.rs` | `add_node` / `add_edge` / `link` / `batch_write`（含校验） |
| `props_schema.rs` | `validate(node)` + `recommended_fields(kind)` |
| `id.rs` | `gen_node_id` |
| `migrations.rs` | `init_schema(conn)` + WAL 配置 + 版本管理 |
| `extractor.rs` | `mirror_task_summary` / `mirror_tool_call` / `mirror_read_file` 等（返回 Node，不直接写） |

### 4.3 主 API（BrainGraph）

```rust
pub struct BrainGraph {
    db: Mutex<rusqlite::Connection>,
    config: GraphConfig,
}

impl BrainGraph {
    // === 生命周期 ===
    pub fn open(base_dir: &Path, config: GraphConfig) -> Result<Self, BrainGraphError>;

    // === 查询（主脑用，通过工具调用） ===
    pub fn recall(&self, opts: &QueryOptions) -> ToolResult<SubGraph>;
    pub fn drill(&self, node_id: &str, depth: usize) -> ToolResult<SubGraph>;
    pub fn list_domains(&self) -> ToolResult<Vec<DomainInfo>>;

    // === 写入（三脑用） ===
    pub fn add_node(&self, node: Node) -> ToolResult<String>;
    pub fn add_edge(&self, edge: Edge) -> ToolResult<()>;
    pub fn link(&self, src: &str, dst: &str, kind: EdgeKind, props: HashMap<String, Value>) -> ToolResult<()>;
    pub fn batch_write(&self, ops: Vec<WriteOp>) -> ToolResult<()>;  // 事务包裹

    // === 维护 ===
    pub fn mark_superseded(&self, node_id: &str) -> ToolResult<()>;
    pub fn touch(&self, node_id: &str);
    pub fn stats(&self) -> ToolResult<GraphStats>;
}
```

### 4.4 模块依赖关系

```
lib.rs
  ↓
┌────────────────────────────────────┐
│ schema.rs  ← error.rs              │
│    ↑                               │
│ storage.rs (依赖 schema/error)     │
│    ↑                               │
│    ├──── query.rs    (依赖 storage, scorer, props_schema) │
│    ├──── writer.rs   (依赖 storage, props_schema, id)     │
│    └──── extractor.rs (依赖 schema, id)                   │
│                                    │
│ migrations.rs (独立)               │
│ id.rs (独立)                       │
│ props_schema.rs (依赖 schema)      │
│ scorer.rs (依赖 schema)            │
└────────────────────────────────────┘

tools/graph_tools.rs (依赖 brain-graph crate)
```

**关键**：storage.rs 是唯一 SQL 层；query.rs/writer.rs 是业务编排；extractor.rs 不直接写 DB，返回 Node 给调用方。

### 4.5 Cargo.toml 依赖

```toml
[package]
name = "brain-graph"
version = "0.1.0"
edition = "2021"

[dependencies]
rusqlite = { version = "0.31", features = ["bundled"] }  # bundled 自带 SQLite
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
log = "0.4"
thiserror = "1"

[dev-dependencies]
tempfile = "3"
```

---

## 5. 工具集

### 5.1 7 个工具签名

| 工具 | 签名 | 必填参数 |
|---|---|---|
| `graph_recall` | `(keywords: Vec<String>, graph_type: GraphType, max_tokens?: usize)` | keywords, graph_type |
| `graph_drill` | `(node_id: String, depth?: usize)` | node_id |
| `graph_list_domains` | `() -> Vec<DomainInfo>` | 无 |
| `graph_add_concept` | `(name: String, graph_type: GraphType, aliases?: Vec<String>)` | name, graph_type |
| `graph_add_entity` | `(name: String, entity_type: String, graph_type: GraphType)` | name, entity_type, graph_type |
| `graph_add_code_node` | `(path: String, code_kind: String, language?: String)` | path, code_kind |
| `graph_link` | `(src_id: String, dst_id: String, edge_kind: EdgeKind, props?: HashMap)` | src_id, dst_id, edge_kind |

### 5.2 工具描述（用于 LLM 调用）

```
graph_recall:
  "批量模糊查询知识图谱。多关键词 OR 匹配，返回混合子图（Memory/Code/Concept/Entity
   节点 + 关联边）。优先用此工具召回信息，比 Read/Grep 更高效。"

graph_drill:
  "下钻某节点的邻居。BFS 遍历 depth 跳（默认 1），返回子图。
   用于 graph_recall 后想看具体节点周边关系时。"

graph_list_domains:
  "列出当前图谱所有域及其节点/边统计。不确定查哪个域时先用此工具。"

graph_add_concept:
  "添加抽象概念节点（如'红冲逻辑'）。记忆脑四步分析时自动调用，
   主脑也可显式调用补全。"

graph_add_entity:
  "添加具体实体节点（人物/项目/技术栈/角色）。"

graph_add_code_node:
  "添加代码节点（File/Function/Class/Module）。主脑探索代码后应主动调用。"

graph_link:
  "通用边写入。建立两个节点的语义关系。两端节点必须同域。"
```

---

## 6. 数据流（端到端场景）

### 6.1 场景 A：主脑召回（最常见路径）

**用户输入**："之前我记得有调用过亿通的外部接口的，好像是什么欧盟的 ics2 啥的，你看下这个接口是什么在调用的"

```
1. 主脑拆词：["ics2", "欧盟", "亿通", "外部接口"]
2. 主脑调用：
   graph_recall({
     keywords: ["ics2", "欧盟", "亿通", "外部接口"],
     graph_type: "Memory",
     max_tokens: 4000
   })

3. brain-graph 内部：
   3.1 别名扩展：
       "ics2" → Concept("ICS2 申报", aliases=["ics2","ICS-2"])
       "亿通" → Entity("亿通", aliases=["Yito","亿通国际"])
       扩展后 keywords += ["ICS-2", "Yito", "亿通国际"]

   3.2 fuzzy_match_nodes（SQL LIKE）：
       匹配到 8 个候选节点（Memory/Concept/Entity 混合）

   3.3 评分（4 因子）：
       score = keyword_match × importance × time_decay × kind_weight
       - Node 1 (Memory task-045): score=0.92
       - Node 2 (Concept ICS2):    score=0.85
       - Node 3 (Entity 亿通):     score=0.78
       - ... 共 8 个

   3.4 贪心装填到 4000 tokens：
       - Node 1 (Full, 150) → total=150
       - Node 2 (WithSummary, 50) → total=200
       - Node 3 (WithSummary, 50) → total=250
       - Node 4-8 (Brief, ~15 each) → total=325
       全部装下（< 4000）

   3.5 加载内部边（仅 packed 节点间）：
       (Concept ICS2) -[:MentionedIn]-> (Memory task-045)
       (Entity 亿通) -[:MentionedIn]-> (Memory task-045)
       (Memory task-045) -[:DerivedFrom]-> (Memory exp-ICS2申报规则)
       (Entity Ship-Core) -[:MentionedIn]-> (Memory task-045)

   3.6 生成 drill_hints：
       ["Memory task-045 还有 3 个 Code 节点未返回，可 drill 获取代码详情"]

   3.7 异步 touch_accessed

4. 工具返回 SubGraph（JSON）

5. 主脑决策：
   - 信息够 → 直接回答
   - 不够 → graph_drill(task-045, depth=2) 或 Read 具体文件
```

### 6.2 场景 B：主脑探索代码后写入

```
1. 主脑调用 Read("progressive_recall.rs") → 获得文件内容
2. 主脑识别这是新代码：
   graph_add_code_node({
     path: "rust/crates/brain-memory/src/progressive_recall.rs",
     code_kind: "File",
     language: "Rust"
   })
   → 工具内部去重检查 → 创建节点 → 返回 node_id

3. 主脑识别出函数：
   graph_add_code_node({path: same, code_kind: "Function", name: "find_in_summary"})
   graph_add_code_node({path: same, code_kind: "Function", name: "find_in_raw"})

4. 主脑建立包含关系：
   graph_link({
     src: <file_node_id>,
     dst: <func_node_id>,
     edge_kind: "Contains"
   })

5. 工具内部校验域一致（两端 Code）→ 写入边
```

### 6.3 场景 C：记忆脑四步分析时自动写入

```
记忆脑现有流程：
  Step 1: 分析对话 → 抽取关键信息
  Step 2: 分类/抽象 → 写入 L2/L3/L4
  Step 3, 4: ...

新增图谱集成：
  Step 2 完成时（写入 L2 TaskSummary）：
    1. 镜像 Memory 节点：
       extractor::mirror_task_summary(&summary) → Node
       graph.add_node(node)

    2. 写入 LLM 抽取的实体（Step 1 顺便抽取）：
       for concept in llm_response.concepts {
           graph.add_concept(concept.name, concept.aliases, Memory)
       }
       for entity in llm_response.entities {
           graph.add_entity(entity.name, entity.entity_type, Memory)
       }

    3. 建立 MentionedIn 边：
       graph.link(c.node_id, task_node_id, EdgeKind::MentionedIn, ...)

失败处理：任一步失败 → 日志 + 跳过（不阻塞四步分析）

启动时 reconciliation（兜底）：
  pyramid_tasks = list_all_l2_task_ids()
  graph_memory_refs = list_memory_node_refs()
  missing = pyramid_tasks - graph_memory_refs
  for task_id in missing {
      extractor::mirror_task_summary(&load(task_id)) → graph.add_node(...)
  }
```

### 6.4 场景 D：评估脑写入因果链

```
评估脑完成评估 → 输出 EvaluationResult { quality_score, issues }

新增图谱写入：
  // 因果链节点
  answer_node  = graph.add_concept("answer_<session_id>")
  tool_node    = graph.add_tool("graph_recall")  // 已存在则不重建
  pitfall_node = graph.add_concept("pitfall_<auto>", props{
    "issue": "工具失败未告知用户",
    "category": "LazyBehavior"
  })

  // 因果链边
  graph.link(answer_node, tool_node,    EdgeKind::Invokes)
  graph.link(answer_node, pitfall_node, EdgeKind::CausedBy)
  graph.link(pitfall_node, memory_node, EdgeKind::MentionedIn)

下次类似问题：
  主脑 graph_recall 看到 Concept("pitfall_xxx") → 主动避免同类错误
```

### 6.5 场景 E：智脑启动

```
Orchestrator::new():
  let graph = match BrainGraph::open(&base_dir, config) {
      Ok(g) => {
          migrations::init_schema(&g.db)?;
          reconciliation::sync_from_pyramid(&g)?;
          Some(Arc::new(g))
      }
      Err(e) => {
          log::warn!("图谱 DB 不可用: {e}");
          None
      }
  };

  let mut tool_registry = ToolRegistry::new();
  tool_registry.register(ReadTool);
  tool_registry.register(GrepTool);

  // 仅当 DB 可用时注册图谱工具
  if let Some(g) = &graph {
      tool_registry.register(GraphRecallTool::new(g.clone()));
      // ... 共 7 个工具
  }

  let main = MainBrain::new(tool_registry, system_prompt_with_graph);
  let memory = MemoryBrain::new(graph.clone());
  let eval = EvalBrain::new(graph.clone());
```

**关键**：DB 不可用时主脑看不到图谱工具，自然走 Read/Grep。

### 6.6 场景 F：主脑优先图谱模式（核心）

```
用户："progressive_recall.rs 改了 find_in_summary 会影响什么？"

主脑思考：先查图谱（不是直接 Read）
  ↓
  graph_recall({
    keywords: ["find_in_summary", "progressive_recall"],
    graph_type: "Code"
  })
  ↓
  返回：
  - Code(Function: find_in_summary)
  - Code(File: progressive_recall.rs)
  - edges: (File) -[:Contains]-> (Function)
  - drill_hints: "find_in_summary 被 3 个函数调用"

主脑判断：需要看谁调用 → drill
  ↓
  graph_drill({node_id: <find_in_summary_id>, depth: 2})
  ↓
  返回：
  - Code(Function: recall) -[:Calls]-> find_in_summary
  - Code(Impl: ProgressiveRecall::recall) -[:Contains]-> recall

主脑判断：影响半径清楚 → 直接回答
  不需要 Read 文件

或者：
  主脑判断图谱信息不全 → Read 文件 → 回写图谱
```

### 6.7 关键时序保证

| 操作 | 时延目标（10k 节点） |
|---|---|
| `graph_recall` 一次查询 | < 100ms |
| `graph_add_node` 单写 | < 10ms |
| `graph_drill` depth=2 | < 50ms |
| 启动初始化（含 reconciliation） | < 1s |

---

## 7. 错误处理 + 测试策略

### 7.1 错误分类 + 应对

| ErrorKind | 触发原因 | 工具返回 message |
|---|---|---|
| `DbLocked` | SQLite 写锁占用（5s 超时） | "数据库锁占用，建议稍后重试或换用 Read/Grep" |
| `DbCorrupted` | 文件损坏、schema 不匹配 | "图谱存储损坏，需修复或继续无图谱工作" |
| `Timeout` | 查询 > 2s | "查询超时，建议精简关键词或换工具" |
| `InvalidInput` | 必填字段缺失、ID 格式错 | 字段名+原因 |
| `DomainMismatch` | 边两端节点不同域 | "两端域不一致：src=Memory dst=Code" |
| `NotFound` | drill 的节点 ID 不存在 | "节点 ID 不存在，可能已被 superseded" |

### 7.2 三脑容忍度

| 脑 | 容忍策略 |
|---|---|
| 主脑 | 100% 容忍。失败自主换工具（Read/Grep），不重试、不阻塞 |
| 记忆脑 | 容忍但有兜底。写入失败跳过，下次启动 reconciliation 补建 |
| 评估脑 | 容忍。因果链写入失败跳过，不阻塞评估报告 |

### 7.3 数据完整性兜底

```
1. 启动 reconciliation：
   pyramid_tasks - graph_memory_refs → 补建缺失

2. 周期性 reconciliation（每 100 次写入触发）：
   扫描金字塔 L2/L3/L4 → 对比图谱 Memory 节点 → 补建差异

3. importance 同步：
   金字塔 importance 衰减 → 同步到图谱 node.importance

4. superseded 同步：
   金字塔 mark_superseded → 同步到图谱 node.superseded=1
```

### 7.4 SQLite 错误映射

```rust
impl From<rusqlite::Error> for BrainGraphError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::SqliteFailure(err, _) => match err.code {
                ErrorCode::DatabaseBusy => BrainGraphError::DbLocked("busy".into()),
                ErrorCode::DatabaseLocked => BrainGraphError::DbLocked("locked".into()),
                ErrorCode::DatabaseCorrupt => BrainGraphError::Corrupted("corrupt".into()),
                _ => BrainGraphError::Sqlite(e),
            },
            _ => BrainGraphError::Sqlite(e),
        }
    }
}
```

### 7.5 测试策略

**TDD 原则**：每个公共 API 至少 3 类测试（happy path / edge case / error path）。

**Mock 策略**：**不用 mock 库**，全部用真实 SQLite + tempfile。
理由：SQLite 自带零成本、tempfile 自动清理、避免 mock 失真、in-memory SQLite ~μs 级。

**标准测试 setup**：
```rust
fn make_test_graph() -> (BrainGraph, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let graph = BrainGraph::open(tmp.path(), GraphConfig::default()).unwrap();
    migrations::init_schema(&graph.db).unwrap();
    (graph, tmp)
}
```

### 7.6 测试用例清单

#### storage.rs（~15 个）
- insert_and_get_node
- update_node_props
- mark_superseded_hides_from_query
- touch_accessed_updates_timestamp
- delete_node_cascades_edges（FK 自动删边）
- fuzzy_match_finds_by_partial_props
- fuzzy_match_filters_by_graph_type
- fuzzy_match_filters_by_node_kind
- count_by_domain
- ... 等

#### scorer.rs（~10 个，纯函数最易测）
- score_combines_four_factors
- time_decay_7_days / 30_days / 90_days
- pack_to_budget_greedy
- pack_to_budget_respects_limit
- pack_to_budget_empty_when_zero_budget
- detail_level_thresholds
- ... 等

#### query.rs（~12 个）
- recall_returns_scored_subgraph
- recall_expands_aliases
- recall_returns_empty_when_no_match
- recall_truncates_to_token_budget
- recall_generates_drill_hints
- recall_loads_internal_edges_only
- drill_bfs_depth_1 / 2
- drill_missing_node_returns_error
- list_domains_includes_fixed_four
- list_domains_includes_custom
- list_domains_shows_zero_for_empty
- ... 等

#### writer.rs（~10 个）
- add_node_validates_required_fields
- add_node_rejects_invalid_graph_type_combo
- add_edge_rejects_domain_mismatch
- add_edge_rejects_missing_nodes
- link_convenience_method
- batch_write_atomic_commit
- batch_write_rollback_on_failure
- mark_superseded_idempotent
- ... 等

#### props_schema.rs（~8 个）
- 各 kind 必填字段校验
- unknown_kind_passes
- recommended_fields_returns_table
- ... 等

#### extractor.rs（~6 个）
- mirror_task_summary_creates_l2_node
- mirror_trigger_creates_l4_node
- mirror_experience_creates_l3_node
- mirror_tool_call_creates_tool_node
- mirror_read_file_creates_code_file_node
- mirror_idempotent（重复不重建）

#### migrations.rs（~4 个）
- init_schema_creates_tables
- init_schema_idempotent
- init_schema_sets_wal_mode
- schema_version_recorded

#### tools/graph_tools.rs（~14 个，每工具 2 个）
- 每个工具的 happy path + error path

#### 集成测试（~8 个端到端）
- end_to_end_recall_finds_related_concepts
- end_to_end_code_explore_and_drill
- end_to_end_memory_sync_after_l2_write
- end_to_end_reconciliation_at_startup
- end_to_end_causal_chain_after_eval
- end_to_end_alias_expansion
- end_to_end_token_budget_truncation
- end_to_end_graph_type_isolation（Memory 查询不返回 Code 节点）

**总测试数估计：~85 个**

### 7.7 TDD 实施顺序（writing-plans 输入）

```
Phase 1：基础层（无依赖）
  1. schema.rs 类型 + serde 测试
  2. error.rs 错误类型转换
  3. id.rs ID 生成

Phase 2：存储层
  4. migrations.rs schema 初始化
  5. storage.rs CRUD 原语
  6. props_schema.rs 字段校验

Phase 3：业务层
  7. scorer.rs 评分 + 装填
  8. writer.rs 写入 API
  9. query.rs 查询 + drill + list_domains
  10. extractor.rs 规则抽取

Phase 4：对外门面
  11. lib.rs BrainGraph 整合

Phase 5：工具层
  12. tools/graph_tools.rs 7 个工具包装

Phase 6：三脑集成
  13. Orchestrator 注入 + 启动初始化
  14. MainBrain system prompt 修改（强驱动性）
  15. MemoryBrain 四步分析集成
  16. EvalBrain 因果链集成

Phase 7：端到端
  17. 集成测试套件
  18. 性能基准
```

---

## 8. 路线图

### 8.1 MVP 范围（本次设计实施）

**包含**：
- 固定 4 域（Memory / Code / Novel / Video）+ Custom 数据层兜底
- 5 类 NodeKind + 11 种 EdgeKind
- 7 个工具集
- 三脑集成（主脑 system prompt、记忆脑四步分析、评估脑因果链）
- SQLite WAL 存储
- 启动 + 周期 reconciliation
- ~85 个测试

**不包含**：
- "创建自定义域"工具（如 `graph_create_domain("game_design", schema)`）
- 每域独立 schema 定制（如 Novel 域强制 Character 必须有 first_chapter）
- 域级访问控制
- 跨域边的特殊语义
- 老数据迁移（当前无图谱数据，无需迁移）

### 8.2 后续演进

```
v1（MVP）：本次设计
  固定 4 域 + Custom 兜底
  每域 NodeKind 共用 5 类
  领域特殊属性全放 props

v2（后续）：
  graph_create_domain 工具
  每域可选 schema 约束
  域级配置文件

v3（远期）：
  跨域边语义
  域级插件（独立抽取器/查询器）
  域级权限控制
```

---

## 9. 附录

### 9.1 词汇表

| 术语 | 含义 |
|---|---|
| 图谱域（GraphType） | 业务领域隔离单元（Memory/Code/Novel/Video） |
| 节点类型（NodeKind） | 节点基础类型（Memory/Concept/Entity/Tool/Code） |
| 边类型（EdgeKind） | 节点间关系类型（11 种） |
| 结构节点 | 已有数据结构的镜像节点（金字塔 task/工具调用记录/Read 文件），规则可建 |
| 语义节点 | LLM 从自由文本抽取的实体/概念，必须 LLM 才能建 |
| 子图（SubGraph） | 查询返回结构（节点 + 边 + drill_hints） |
| 召回（recall） | 主力查询 API，多关键词批量模糊匹配 |
| 下钻（drill） | 查看某节点邻居（BFS） |
| 强驱动性 | system prompt 引导主脑优先用图谱 |
| 软依赖 | DB 不可用时降级运行 |
| reconciliation | 启动/周期同步金字塔到图谱的兜底机制 |

### 9.2 配置示例

```toml
# ~/.ai-brain/config.toml
[graph]
default_max_tokens = 4000
default_node_limits = { Memory = 8, Code = 5, Concept = 3, Entity = 3, Tool = 2 }
fuzzy_threshold = 0.6
enable_alias_expansion = true
time_decay_days = [7, 30, 90]  # 对应 1.0 / 0.9 / 0.7 / 0.5
timeout_ms = 2000
reconciliation_interval_writes = 100
```

### 9.3 参考资料

- 现有金字塔记忆系统：`rust/crates/brain-memory/src/pyramid_*.rs`
- 现有三脑架构：`docs/plans/2026-04-17-three-brain-architecture-redesign.md`
- RealToolExecutor 注册机制：`rust/crates/ai-brain-cli/src/orchestrator.rs`
- 市面调研：Microsoft GraphRAG / LightRAG / AGENTiGraph（见会话讨论）

---

## 设计确认记录

| 时间 | 章节 | 确认人 | 关键修订 |
|---|---|---|---|
| 2026-06-26 | 整体架构 | 用户 | 修订 1：三脑（无进化脑）；修订 2：强依赖语义；修订 3：工具不替主脑决策 |
| 2026-06-26 | 数据模型 | 用户 | 新增 GraphType 域 + graph_list_domains 工具 |
| 2026-06-26 | 组件模块 | 用户 | 11 个模块清单 + Cargo.toml 依赖 |
| 2026-06-26 | 数据流 | 用户 | 6 个端到端场景 |
| 2026-06-26 | 错误处理+测试 | 用户 | ~85 个测试 + TDD 7 Phase |

---

**下一步**：调用 `superpowers:writing-plans` skill 生成 TDD 实施计划。
