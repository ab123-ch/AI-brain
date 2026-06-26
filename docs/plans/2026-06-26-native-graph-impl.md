# 智脑原生知识图谱能力实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 新增 `brain-graph` crate + 三脑集成，让智脑拥有原生知识图谱能力（7 个工具、5 类节点、11 种边、4 个图谱域）

**Architecture:** 独立 crate `brain-graph/`（SQLite WAL 存储 + 11 个内部模块）+ `tools/graph_tools.rs` 7 个工具包装 + 三脑集成（主脑 system prompt 强驱动、记忆脑四步分析同步写入、评估脑因果链）。最终一致 + 软依赖 + 主脑自决策降级。

**Tech Stack:** Rust 2021、rusqlite 0.31 (bundled)、serde、uuid、chrono、thiserror、tempfile (dev)

**关联设计文档：** `docs/plans/2026-06-26-native-graph-design.md`

---

## 执行约定

- **TDD 五步**：每任务 = 写失败测试 → 验证红 → 实现 → 验证绿 → commit
- **测试 setup**：统一用 `tempfile + 真实 SQLite`，不用 mock 库
- **测试命令**：`cargo test -p brain-graph <test_name>`
- **commit 风格**：`feat(graph): <动作>` / `test(graph): <动作>` / `refactor(graph): <动作>`
- **Phase 之间允许跨会话**，每 Phase 结束跑一次 `cargo test -p brain-graph --all`

---

## Phase 1：基础层（无依赖）

### Task 1.1：schema.rs 类型定义

**Files:**
- Create: `rust/crates/brain-graph/Cargo.toml`
- Create: `rust/crates/brain-graph/src/lib.rs`
- Create: `rust/crates/brain-graph/src/schema.rs`

**Step 1: 创建 Cargo.toml**

```toml
[package]
name = "brain-graph"
version = "0.1.0"
edition = "2021"

[dependencies]
rusqlite = { version = "0.31", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
log = "0.4"
thiserror = "1"

[dev-dependencies]
tempfile = "3"
```

注册到 workspace：编辑 `rust/Cargo.toml`，在 `[workspace] members` 加入 `"crates/brain-graph"`。

**Step 2: 写失败测试（schema.rs 末尾）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_serde_roundtrip() {
        for kind in [NodeKind::Memory, NodeKind::Concept, NodeKind::Entity, NodeKind::Tool, NodeKind::Code] {
            let j = serde_json::to_string(&kind).unwrap();
            let back: NodeKind = serde_json::from_str(&j).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn graph_type_custom_roundtrip() {
        let g = GraphType::Custom("game".into());
        let j = serde_json::to_string(&g).unwrap();
        let back: GraphType = serde_json::from_str(&j).unwrap();
        assert_eq!(g, back);
    }

    #[test]
    fn edge_kind_all_variants() {
        for e in [EdgeKind::MentionedIn, EdgeKind::Calls, EdgeKind::Invokes, EdgeKind::RelatedTo] {
            let j = serde_json::to_string(&e).unwrap();
            let back: EdgeKind = serde_json::from_str(&j).unwrap();
            assert_eq!(e, back);
        }
    }

    #[test]
    fn node_full_roundtrip() {
        let n = Node {
            id: "memory_concept_abc12345".into(),
            kind: NodeKind::Concept,
            graph_type: GraphType::Memory,
            props: HashMap::from([("name".into(), json!("红冲逻辑"))]),
            importance: 0.85,
            created_at: 1719340800000,
            last_accessed: 1719340800000,
            superseded: false,
        };
        let j = serde_json::to_string(&n).unwrap();
        let back: Node = serde_json::from_str(&j).unwrap();
        assert_eq!(n.id, back.id);
        assert_eq!(back.kind, NodeKind::Concept);
    }

    #[test]
    fn subgraph_with_scored_nodes() {
        let sg = SubGraph {
            nodes: vec![],
            edges: vec![],
            total_found: 0,
            truncated: false,
            drill_hints: vec![],
        };
        let j = serde_json::to_string(&sg).unwrap();
        assert!(j.contains("\"total_found\":0"));
    }
}
```

**Step 3: 运行测试，验证失败**

```bash
cargo test -p brain-graph schema::tests
```
Expected: FAIL（类型未定义）

**Step 4: 写实现**

在 `schema.rs` 写出第 3.1-3.7 节的所有类型定义（NodeKind / GraphType / EdgeKind / Node / Edge / SubGraph / ScoredNode / DetailLevel / DomainInfo）。在 `lib.rs` 加 `pub mod schema;`。

**Step 5: 验证通过 + commit**

```bash
cargo test -p brain-graph schema::tests
git add rust/crates/brain-graph/ rust/Cargo.toml
git commit -m "feat(graph): 初始化 brain-graph crate + schema 类型定义"
```

---

### Task 1.2：error.rs 错误类型 + ToolResult 三态

**Files:**
- Create: `rust/crates/brain-graph/src/error.rs`
- Modify: `rust/crates/brain-graph/src/lib.rs`（加 `pub mod error;`）

**Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_ok_serializes_with_status_tag() {
        let r: ToolResult<i32> = ToolResult::Ok { data: 42 };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"status\":\"ok\""));
        assert!(j.contains("42"));
    }

    #[test]
    fn tool_result_empty_serializes_with_hint() {
        let r: ToolResult<i32> = ToolResult::Empty {
            searched_nodes: 100,
            searched_edges: 50,
            hint: Some("试试别名".into()),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"status\":\"empty\""));
        assert!(j.contains("试试别名"));
    }

    #[test]
    fn tool_result_err_serializes_kind() {
        let r: ToolResult<i32> = ToolResult::Err {
            kind: ErrorKind::Timeout,
            message: "查询超时".into(),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"status\":\"error\""));
        assert!(j.contains("Timeout"));
    }

    #[test]
    fn brain_graph_error_from_rusqlite_busy() {
        // 构造 busy 错误比较复杂，仅测 Display
        let e = BrainGraphError::DbLocked("test".into());
        assert!(e.to_string().contains("锁"));
    }
}
```

**Step 2: 运行验证失败**

```bash
cargo test -p brain-graph error::tests
```

**Step 3: 写实现**

`error.rs` 包含：
- `BrainGraphError` enum（thiserror 派生，见设计 7.4）
- `ToolResult<T>` enum（带 `#[serde(tag = "status")]`）
- `ErrorKind` enum
- `impl From<rusqlite::Error> for BrainGraphError`

**Step 4-5: 验证 + commit**

```bash
cargo test -p brain-graph error::tests
git commit -m "feat(graph): 错误类型 + ToolResult 三态序列化"
```

---

### Task 1.3：id.rs ID 生成

**Files:**
- Create: `rust/crates/brain-graph/src/id.rs`
- Modify: `lib.rs`（加 `pub mod id;`）

**Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_has_correct_prefix() {
        let id = gen_node_id(GraphType::Memory, NodeKind::Concept);
        assert!(id.starts_with("memory_concept_"));
        assert_eq!(id.len(), "memory_concept_".len() + 8);
    }

    #[test]
    fn id_code_file_prefix() {
        let id = gen_node_id(GraphType::Code, NodeKind::Code);
        assert!(id.starts_with("code_code_"));
    }

    #[test]
    fn id_custom_domain() {
        let id = gen_node_id(GraphType::Custom("game".into()), NodeKind::Entity);
        assert!(id.starts_with("custom(game)_entity_"));
    }

    #[test]
    fn id_unique() {
        let a = gen_node_id(GraphType::Memory, NodeKind::Concept);
        let b = gen_node_id(GraphType::Memory, NodeKind::Concept);
        assert_ne!(a, b);
    }
}
```

**Step 2-5: 验证失败 → 实现 → 验证通过 → commit**

实现 `gen_node_id` 见设计 3.10。

```bash
git commit -m "feat(graph): ID 生成器"
```

---

## Phase 2：存储层

### Task 2.1：migrations.rs schema 初始化

**Files:**
- Create: `rust/crates/brain-graph/src/migrations.rs`

**Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn make_conn() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn init_schema_creates_tables() {
        let conn = make_conn();
        init_schema(&conn).unwrap();
        // nodes 表存在
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
        // edges 表存在
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn init_schema_idempotent() {
        let conn = make_conn();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();  // 不报错
    }

    #[test]
    fn init_schema_sets_wal() {
        // in-memory 不支持 WAL，但 PRAGMA 语句不应报错
        let conn = make_conn();
        init_schema(&conn).unwrap();
    }

    #[test]
    fn schema_version_recorded() {
        let conn = make_conn();
        init_schema(&conn).unwrap();
        let v: String = conn.query_row(
            "SELECT value FROM schema_meta WHERE key='version'", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(v, "1");
    }
}
```

**Step 2-5: 实现 + commit**

实现 `init_schema(conn)`：执行 PRAGMA + CREATE TABLE IF NOT EXISTS + INSERT version（见设计 3.9）。

```bash
git commit -m "feat(graph): SQLite schema 初始化 + WAL 配置"
```

---

### Task 2.2：storage.rs Node CRUD

**Files:**
- Create: `rust/crates/brain-graph/src/storage.rs`

**Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_storage() -> Storage {
        let conn = Connection::open_in_memory().unwrap();
        crate::migrations::init_schema(&conn).unwrap();
        Storage::new(conn)
    }

    fn sample_node(id: &str, kind: NodeKind, gt: GraphType) -> Node {
        Node {
            id: id.into(),
            kind,
            graph_type: gt,
            props: HashMap::from([("name".into(), json!("测试"))]),
            importance: 0.5,
            created_at: 1719340800000,
            last_accessed: 1719340800000,
            superseded: false,
        }
    }

    #[test]
    fn insert_and_get_node() {
        let s = make_storage();
        let n = sample_node("test_001", NodeKind::Concept, GraphType::Memory);
        s.insert_node(&n).unwrap();
        let got = s.get_node("test_001").unwrap().unwrap();
        assert_eq!(got.kind, NodeKind::Concept);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let s = make_storage();
        assert!(s.get_node("missing").unwrap().is_none());
    }

    #[test]
    fn mark_superseded_hides_in_default_query() {
        let s = make_storage();
        s.insert_node(&sample_node("test_002", NodeKind::Concept, GraphType::Memory)).unwrap();
        s.mark_superseded("test_002").unwrap();
        let got = s.get_node("test_002").unwrap().unwrap();
        assert!(got.superseded);
    }

    #[test]
    fn touch_accessed_updates_timestamp() {
        let s = make_storage();
        s.insert_node(&sample_node("test_003", NodeKind::Concept, GraphType::Memory)).unwrap();
        let old = s.get_node("test_003").unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        s.touch_accessed("test_003").unwrap();
        let new = s.get_node("test_003").unwrap().unwrap();
        assert!(new.last_accessed > old.last_accessed);
    }

    #[test]
    fn delete_node_cascades_edges() {
        let s = make_storage();
        s.insert_node(&sample_node("a", NodeKind::Concept, GraphType::Memory)).unwrap();
        s.insert_node(&sample_node("b", NodeKind::Concept, GraphType::Memory)).unwrap();
        s.insert_edge(&Edge {
            src: "a".into(), dst: "b".into(), kind: EdgeKind::RelatedTo,
            props: HashMap::new(), created_at: 0, weight: 0.5,
        }).unwrap();
        s.delete_node("a").unwrap();
        let edges = s.get_edges_of("b", Direction::Incoming).unwrap();
        assert!(edges.is_empty());
    }
}
```

**Step 2-5: 实现 + commit**

`Storage` struct + `Mutex<Connection>` + Node CRUD 方法。

```bash
git commit -m "feat(graph): storage Node CRUD 原语"
```

---

### Task 2.3：storage.rs Edge CRUD + 方向查询

**Files:** Modify: `storage.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn insert_and_get_edge_outgoing() {
    let s = make_storage();
    s.insert_node(&sample_node("a", NodeKind::Concept, GraphType::Memory)).unwrap();
    s.insert_node(&sample_node("b", NodeKind::Memory, GraphType::Memory)).unwrap();
    s.insert_edge(&Edge {
        src: "a".into(), dst: "b".into(), kind: EdgeKind::MentionedIn,
        props: HashMap::new(), created_at: 0, weight: 0.7,
    }).unwrap();
    let edges = s.get_edges_of("a", Direction::Outgoing).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].kind, EdgeKind::MentionedIn);
}

#[test]
fn get_edges_incoming() {
    // 类似上面，查 b 的 incoming
}

#[test]
fn delete_edge_by_triple() {
    // insert → delete(src,dst,kind) → 查不到
}

#[test]
fn insert_duplicate_edge_replaces() {
    // INSERT OR REPLACE 行为
}
```

**Step 2-5: 实现 + commit**

```bash
git commit -m "feat(graph): storage Edge CRUD + 方向查询"
```

---

### Task 2.4：storage.rs fuzzy_match + count

**Files:** Modify: `storage.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn fuzzy_match_finds_by_partial_props() {
    let s = make_storage();
    // 插入 3 个节点
    s.insert_node(&Node {
        id: "n1".into(), kind: NodeKind::Concept, graph_type: GraphType::Memory,
        props: HashMap::from([("name".into(), json!("红冲逻辑"))]),
        importance: 0.5, created_at: 0, last_accessed: 0, superseded: false,
    }).unwrap();
    // ...
    let result = s.fuzzy_match_nodes(
        &["红冲".into()], &GraphType::Memory, None, 100
    ).unwrap();
    assert!(!result.is_empty());
    assert!(result.iter().any(|n| n.id == "n1"));
}

#[test]
fn fuzzy_match_filters_by_graph_type() {
    // Memory 域查询不返回 Code 域节点
}

#[test]
fn fuzzy_match_filters_by_node_kind() {
    // node_kinds 过滤生效
}

#[test]
fn fuzzy_match_skips_superseded() {
    // superseded=true 的不返回
}

#[test]
fn count_by_domain() {
    // 插 3 个 Memory + 2 个 Code → count_by_domain 返回 (3, ...)
}

#[test]
fn count_total() {
    // 节点总数
}
```

**Step 2-5: 实现**

`fuzzy_match_nodes`：构造 SQL `WHERE graph_type=? AND superseded=0 AND (props LIKE ? OR ...)`，按 limit 返回。

```bash
git commit -m "feat(graph): storage 模糊匹配 + 域统计"
```

---

### Task 2.5：props_schema.rs 字段校验

**Files:** Create: `props_schema.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn memory_requires_layer_ref_id_summary() {
    let mut node = sample_node("x", NodeKind::Memory, GraphType::Memory);
    node.props = HashMap::new();  // 缺所有必填
    assert!(validate(&node).is_err());

    node.props.insert("layer".into(), json!("L2"));
    assert!(validate(&node).is_err());

    node.props.insert("ref_id".into(), json!("task-001"));
    assert!(validate(&node).is_err());

    node.props.insert("summary_brief".into(), json!("测试"));
    assert!(validate(&node).is_ok());
}

#[test]
fn concept_requires_name() { /* ... */ }

#[test]
fn entity_requires_name_and_type() { /* ... */ }

#[test]
fn tool_requires_name_and_kind() { /* ... */ }

#[test]
fn code_requires_path_and_kind() { /* ... */ }

#[test]
fn recommended_fields_returns_table() {
    let fields = recommended_fields(NodeKind::Memory);
    assert!(fields.iter().any(|(k, _)| *k == "layer"));
}
```

**Step 2-5: 实现**

`validate(node) -> BrainGraphResult<()>` + `recommended_fields(kind) -> &[(&str, &str)]`。

```bash
git commit -m "feat(graph): props 字段校验 + 推荐字段表"
```

---

## Phase 3：业务层

### Task 3.1：scorer.rs 评分公式 + time_decay

**Files:** Create: `scorer.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn time_decay_within_7_days() {
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(time_decay(now), 1.0);
    assert_eq!(time_decay(now - 3 * 86_400_000), 1.0);
}

#[test]
fn time_decay_8_to_30_days() {
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(time_decay(now - 10 * 86_400_000), 0.9);
}

#[test]
fn time_decay_31_to_90_days() {
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(time_decay(now - 60 * 86_400_000), 0.7);
}

#[test]
fn time_decay_over_90_days() {
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(time_decay(now - 100 * 86_400_000), 0.5);
}

#[test]
fn score_combines_four_factors() {
    let node = sample_node("x", NodeKind::Memory, GraphType::Memory);
    let scored = score_node(&node, &["测试".into()]);
    assert!(scored.score >= 0.0 && scored.score <= 1.0);
}

#[test]
fn kind_weight_memory_highest() {
    // Memory 权重 1.0，Code 0.9，Concept 0.8，Entity 0.7，Tool 0.6
}
```

**Step 2-5: 实现 + commit**

`time_decay(last_accessed) -> f64` + `score_node(node, keywords) -> ScoredNode` + `kind_weight(kind)`。

```bash
git commit -m "feat(graph): 评分公式 + 时间衰减"
```

---

### Task 3.2：scorer.rs pack_to_budget 装填

**Files:** Modify: `scorer.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn pack_to_budget_greedy_by_score() {
    let scored = vec![
        ScoredNode { score: 0.9, detail_level: DetailLevel::Full, .. },
        ScoredNode { score: 0.5, detail_level: DetailLevel::Brief, .. },
    ];
    let packed = pack_to_budget(scored, 200);
    // 高分在前
    assert_eq!(packed[0].score, 0.9);
}

#[test]
fn pack_to_budget_respects_limit() {
    // 构造大量节点，预算小时只装下部分
}

#[test]
fn pack_to_budget_empty_when_zero_budget() {
    let packed = pack_to_budget(vec![...], 0);
    assert!(packed.is_empty());
}

#[test]
fn detail_level_thresholds() {
    // 高分（>0.8）→ Full，中分（>0.4）→ WithSummary，低分 → Brief
}

#[test]
fn estimate_tokens_correct() {
    assert_eq!(estimate_tokens(&DetailLevel::Brief), 15);
    assert_eq!(estimate_tokens(&DetailLevel::WithSummary), 50);
    assert_eq!(estimate_tokens(&DetailLevel::Full), 150);
}
```

**Step 2-5: 实现 + commit**

```bash
git commit -m "feat(graph): 贪心装填到 token 预算"
```

---

### Task 3.3：writer.rs 写入 API（add_node/edge/link）

**Files:** Create: `writer.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn add_node_validates_required_fields() {
    let g = make_test_graph();
    let bad_node = Node { /* 缺必填 */ };
    let r = g.add_node(bad_node);
    assert!(matches!(r, ToolResult::Err { kind: ErrorKind::InvalidInput, .. }));
}

#[test]
fn add_node_returns_id_on_success() {
    let g = make_test_graph();
    let node = make_valid_concept_node("测试", GraphType::Memory);
    let r = g.add_node(node.clone());
    assert!(matches!(r, ToolResult::Ok(_)));
    assert_eq!(r.unwrap(), node.id);
}

#[test]
fn add_edge_rejects_domain_mismatch() {
    let g = make_test_graph();
    let n1 = make_valid_concept_node("a", GraphType::Memory);
    let n2 = make_valid_code_file("path", GraphType::Code);
    g.add_node(n1.clone()).unwrap();
    g.add_node(n2.clone()).unwrap();
    let r = g.add_edge(Edge { src: n1.id, dst: n2.id, kind: EdgeKind::RelatedTo, .. });
    assert!(matches!(r, ToolResult::Err { kind: ErrorKind::DomainMismatch, .. }));
}

#[test]
fn add_edge_rejects_missing_nodes() {
    // src/dst 不存在
}

#[test]
fn link_is_convenience_for_add_edge() {
    // link("a", "b", kind, props) 等价于 add_edge(Edge{...})
}
```

`make_test_graph()` 在测试 mod 里定义（tempfile + open + init_schema）。

**Step 2-5: 实现 + commit**

`BrainGraph` struct + `add_node/edge/link` 方法（含域一致性校验）。

```bash
git commit -m "feat(graph): 写入 API + 域一致性校验"
```

---

### Task 3.4：writer.rs batch_write + mark_superseded

**Files:** Modify: `writer.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn batch_write_atomic_commit() {
    let g = make_test_graph();
    let ops = vec![
        WriteOp::AddNode(make_valid_concept_node("a", GraphType::Memory)),
        WriteOp::AddNode(make_valid_concept_node("b", GraphType::Memory)),
        WriteOp::AddEdge { ... },
    ];
    assert!(matches!(g.batch_write(ops), ToolResult::Ok(())));
}

#[test]
fn batch_write_rollback_on_failure() {
    // 第二个 op 失败 → 第一个也回滚
}

#[test]
fn mark_superseded_idempotent() {
    // 重复调用不报错
}
```

**Step 2-5: 实现**

`WriteOp` enum + 事务包裹 + rollback。

```bash
git commit -m "feat(graph): 批量写入事务 + mark_superseded"
```

---

### Task 3.5：query.rs recall 主逻辑

**Files:** Create: `query.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn recall_returns_scored_subgraph() {
    let g = seed_graph_with_test_data();
    let r = g.recall(&QueryOptions {
        keywords: vec!["测试".into()],
        graph_type: GraphType::Memory,
        ..Default::default()
    });
    match r {
        ToolResult::Ok(sg) => {
            assert!(!sg.nodes.is_empty());
            // 按分数降序
            for w in sg.nodes.windows(2) {
                assert!(w[0].score >= w[1].score);
            }
        }
        _ => panic!("期望 Ok"),
    }
}

#[test]
fn recall_returns_empty_when_no_match() {
    let g = make_test_graph();
    let r = g.recall(&QueryOptions {
        keywords: vec!["完全不存在的关键词".into()],
        graph_type: GraphType::Memory,
        ..Default::default()
    });
    assert!(matches!(r, ToolResult::Empty { .. }));
}

#[test]
fn recall_truncates_to_token_budget() {
    // 插入 100 节点，预算 100 tokens → 只装下少数
}

#[test]
fn recall_loads_internal_edges_only() {
    // 边的两端都在 packed 节点里
}

#[test]
fn recall_generates_drill_hints() {
    // total_found > packed.len() → 有 hint
}

#[test]
fn recall_filters_by_graph_type() {
    // Memory 域查询不返回 Code 节点
}
```

**Step 2-5: 实现**

`QueryOptions` + `recall()` 编排 storage + scorer。

```bash
git commit -m "feat(graph): recall 主力查询 + token 预算裁剪"
```

---

### Task 3.6：query.rs alias 扩展

**Files:** Modify: `query.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn recall_expands_aliases() {
    let g = make_test_graph();
    // 插入 Concept: name="红冲逻辑", aliases=["红冲","资费红冲"]
    // 关联回 task-001（但 task 不包含 "红冲逻辑" 字面）
    let r = g.recall(&QueryOptions {
        keywords: vec!["红冲".into()],
        graph_type: GraphType::Memory,
        ..Default::default()
    });
    // 应该通过别名匹配找到 Concept + 关联 task
}

#[test]
fn alias_expansion_disabled_when_flag_false() {
    // alias_expansion=false → 不扩展
}
```

**Step 2-5: 实现**

`expand_aliases(keywords, graph_type) -> Vec<String>`。

```bash
git commit -m "feat(graph): 别名扩展查询"
```

---

### Task 3.7：query.rs drill + list_domains

**Files:** Modify: `query.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn drill_bfs_depth_1() {
    let g = seed_chain_a_b_c();  // a-b-c 链
    let r = g.drill("a", 1);
    match r {
        ToolResult::Ok(sg) => {
            // 节点 a, b（一跳邻居）
            assert!(sg.nodes.iter().any(|n| n.node.id == "a"));
            assert!(sg.nodes.iter().any(|n| n.node.id == "b"));
            assert!(!sg.nodes.iter().any(|n| n.node.id == "c"));  // 两跳外
        }
        _ => panic!(),
    }
}

#[test]
fn drill_bfs_depth_2() {
    // depth=2 → 包含 c
}

#[test]
fn drill_missing_node_returns_error() {
    let g = make_test_graph();
    let r = g.drill("nonexistent", 1);
    assert!(matches!(r, ToolResult::Err { kind: ErrorKind::NotFound, .. }));
}

#[test]
fn list_domains_includes_fixed_four() {
    let g = make_test_graph();
    let r = g.list_domains();
    let ds = r.unwrap();
    assert_eq!(ds.len(), 4);  // Memory, Code, Novel, Video
    assert!(ds.iter().any(|d| d.graph_type == GraphType::Memory));
}

#[test]
fn list_domains_shows_zero_for_empty() {
    let g = make_test_graph();
    let ds = g.list_domains().unwrap();
    let novel = ds.iter().find(|d| d.graph_type == GraphType::Novel).unwrap();
    assert_eq!(novel.node_count, 0);
}

#[test]
fn list_domains_includes_custom_if_data_exists() {
    // 插入 Custom 域节点 → list_domains 返回 5 项
}
```

**Step 2-5: 实现**

`drill(node_id, depth)` BFS + `list_domains()` 固定 4 域 + Custom 扫描。

```bash
git commit -m "feat(graph): drill BFS + list_domains"
```

---

### Task 3.8：extractor.rs 金字塔镜像

**Files:** Create: `extractor.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn mirror_task_summary_creates_l2_node() {
    let summary = TaskSummary {
        task_id: "task-001".into(),
        task_type: TaskType::Coding,
        task_name: "TUI 修复".into(),
        summary: "修复了鼠标捕获问题".into(),
        l1_refs: vec![],
        tags: vec!["TUI".into()],
        importance: 0.85,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let node = mirror_task_summary(&summary);
    assert_eq!(node.kind, NodeKind::Memory);
    assert_eq!(node.graph_type, GraphType::Memory);
    assert_eq!(node.props.get("layer").unwrap(), &json!("L2"));
    assert_eq!(node.props.get("ref_id").unwrap(), &json!("task-001"));
    assert!((node.importance - 0.85).abs() < 0.01);
}

#[test]
fn mirror_tool_call_creates_tool_node() { /* ... */ }

#[test]
fn mirror_read_file_creates_code_file_node() {
    let node = mirror_read_file("path/to/file.rs", "Rust");
    assert_eq!(node.kind, NodeKind::Code);
    assert_eq!(node.graph_type, GraphType::Code);
    assert!(node.props.get("path").unwrap().as_str().unwrap().contains("file.rs"));
}
```

**Step 2-5: 实现 + commit**

`mirror_*` 函数返回 Node（不直接写 DB）。

```bash
git commit -m "feat(graph): 金字塔镜像抽取器"
```

---

## Phase 4：对外门面

### Task 4.1：lib.rs BrainGraph 整合

**Files:** Modify: `lib.rs`（重写为对外门面）

**Step 1: 写失败测试**

```rust
#[test]
fn brain_graph_open_creates_db_file() {
    let tmp = tempfile::tempdir().unwrap();
    let g = BrainGraph::open(tmp.path(), GraphConfig::default()).unwrap();
    assert!(tmp.path().join("graph.db").exists());
}

#[test]
fn brain_graph_open_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let _g1 = BrainGraph::open(tmp.path(), GraphConfig::default()).unwrap();
    let _g2 = BrainGraph::open(tmp.path(), GraphConfig::default()).unwrap();  // 不报错
}

#[test]
fn end_to_end_add_then_recall() {
    let tmp = tempfile::tempdir().unwrap();
    let g = BrainGraph::open(tmp.path(), GraphConfig::default()).unwrap();
    let n = make_valid_concept_node("测试概念", GraphType::Memory);
    g.add_node(n).unwrap();
    let r = g.recall(&QueryOptions {
        keywords: vec!["测试".into()],
        graph_type: GraphType::Memory,
        ..Default::default()
    });
    assert!(matches!(r, ToolResult::Ok(_)));
}

#[test]
fn graph_config_default_budget_4000() {
    assert_eq!(GraphConfig::default().default_max_tokens, 4000);
}
```

**Step 2-5: 实现**

`lib.rs` 整合 schema/error/storage/query/writer/extractor，定义 `BrainGraph`、`GraphConfig`，转发公共 API。

```bash
git commit -m "feat(graph): BrainGraph 对外门面 + GraphConfig"
```

---

## Phase 5：工具层

### Task 5.1：tools/graph_tools.rs graph_recall + graph_drill + graph_list_domains

**Files:** Create: `rust/crates/tools/src/graph_tools.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn graph_recall_tool_executes_and_returns_subgraph() {
    let graph = make_seeded_graph();
    let tool = GraphRecallTool::new(graph);
    let input = json!({
        "keywords": ["测试"],
        "graph_type": "Memory"
    });
    let out = tool.execute(input);
    assert_eq!(out["status"], "ok");
    assert!(out["nodes"].is_array());
}

#[test]
fn graph_recall_tool_invalid_input_returns_err() {
    let graph = make_test_graph();
    let tool = GraphRecallTool::new(graph);
    let input = json!({});  // 缺 keywords
    let out = tool.execute(input);
    assert_eq!(out["status"], "error");
}

#[test]
fn graph_drill_tool_returns_neighbors() { /* ... */ }

#[test]
fn graph_list_domains_tool_returns_four() {
    let graph = make_test_graph();
    let tool = GraphListDomainsTool::new(graph);
    let out = tool.execute(json!({}));
    let domains = out["data"].as_array().unwrap();
    assert_eq!(domains.len(), 4);
}
```

**Step 2-5: 实现**

3 个工具 struct + 实现 `Tool` trait（name/description/parameters_schema/execute）。

`Tool` trait 在 `tools/src/lib.rs` 已有定义，按现有 `ReadTool/GrepTool` 实现风格走。

```bash
git commit -m "feat(graph): graph_recall + drill + list_domains 工具包装"
```

---

### Task 5.2：tools/graph_tools.rs 4 个写入工具

**Files:** Modify: `graph_tools.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn graph_add_concept_returns_node_id() {
    let graph = make_test_graph();
    let tool = GraphAddConceptTool::new(graph);
    let input = json!({ "name": "红冲逻辑", "graph_type": "Memory" });
    let out = tool.execute(input);
    assert_eq!(out["status"], "ok");
    assert!(out["data"]["id"].as_str().unwrap().starts_with("memory_concept_"));
}

#[test]
fn graph_add_concept_missing_name_returns_err() { /* ... */ }

#[test]
fn graph_add_entity_returns_node_id() { /* ... */ }

#[test]
fn graph_add_code_node_returns_node_id() { /* ... */ }

#[test]
fn graph_link_creates_edge() {
    // 先 add 两个节点，再 link，验证 edge 存在
}

#[test]
fn graph_link_domain_mismatch_returns_err() { /* ... */ }
```

**Step 2-5: 实现 + commit**

```bash
git commit -m "feat(graph): 4 个写入工具包装"
```

---

### Task 5.3：注册到 RealToolExecutor

**Files:** Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

**Step 1: 写失败测试（集成测试）**

```rust
#[test]
fn orchestrator_registers_graph_tools_when_db_available() {
    let tmp = tempfile::tempdir().unwrap();
    let orch = Orchestrator::with_base_dir(tmp.path()).unwrap();
    let tool_names: Vec<_> = orch.tool_registry.list_names();
    assert!(tool_names.contains(&"graph_recall"));
    assert!(tool_names.contains(&"graph_drill"));
    assert!(tool_names.contains(&"graph_list_domains"));
    // ... 共 7 个
}

#[test]
fn orchestrator_skips_graph_tools_when_db_unavailable() {
    // 模拟 DB 不可用（目录权限错）→ tool_registry 不含 graph_*
}
```

**Step 2-5: 实现**

在 `Orchestrator::new()` 里：
1. `BrainGraph::open()` → `Option<Arc<BrainGraph>>`
2. 如果 Some → 注册 7 个工具
3. 注入到 MemoryBrain/EvalBrain

```bash
git commit -m "feat(graph): Orchestrator 集成 + 工具注册"
```

---

## Phase 6：三脑集成

### Task 6.1：MainBrain system prompt 强驱动

**Files:** Modify: `rust/crates/brain-main/src/prompts.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn system_prompt_mentions_graph_recall_priority() {
    let prompt = build_main_brain_prompt();
    assert!(prompt.contains("graph_recall"));
    assert!(prompt.contains("优先"));  // "优先用图谱"语义
}

#[test]
fn system_prompt_describes_tool_failure_handling() {
    let prompt = build_main_brain_prompt();
    // 教 LLM 工具失败时自主换工具
    assert!(prompt.contains("empty") || prompt.contains("error"));
    assert!(prompt.contains("换工具") || prompt.contains("Read"));
}
```

**Step 2: 修改 prompt**

在 `build_main_brain_prompt()` 加入：

```
[知识图谱使用]
1. 收到用户消息，先判断是否需要回忆/查找信息
2. 优先调用 graph_recall（批量关键词查询）
3. 工具可能返回：
   - status=ok → 直接用数据回答
   - status=empty → 换关键词，或用 Read/Grep
   - status=error → 工具会告知原因，自主判断换工具
4. 探索代码（Read/Grep 完成）后，调 graph_add_code_node 把发现写入图谱
5. 不要假设工具一定可用或一定成功
```

**Step 3-5: 验证 + commit**

```bash
cargo test -p brain-main prompts::
git commit -m "feat(brain-main): system prompt 加入图谱强驱动引导"
```

---

### Task 6.2：MemoryBrain 四步分析集成

**Files:** Modify: `rust/crates/brain-memory/src/pyramid_memory_brain.rs`

**Step 1: 写失败测试**

```rust
#[test]
fn memory_brain_mirrors_l2_to_graph_on_write() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = Arc::new(BrainGraph::open(tmp.path(), GraphConfig::default()).unwrap());
    let brain = MemoryBrain::with_graph(graph.clone());

    let summary = make_sample_task_summary();
    brain.write_l2_summary(&summary).unwrap();

    // 验证图谱有对应 Memory 节点
    let r = graph.recall(&QueryOptions {
        keywords: vec![summary.task_id.clone()],
        graph_type: GraphType::Memory,
        ..Default::default()
    });
    assert!(matches!(r, ToolResult::Ok(_)));
}

#[test]
fn memory_brain_continues_on_graph_write_failure() {
    // 模拟 graph 写入失败 → 四步分析不阻塞
}
```

**Step 2-5: 实现**

`MemoryBrain::with_graph(graph)` + `write_l2_summary()` 内部调 `extractor::mirror_task_summary` + `graph.add_node`（失败日志+跳过）。

```bash
git commit -m "feat(brain-memory): 四步分析同步镜像图谱"
```

---

### Task 6.3：EvalBrain 因果链写入

**Files:** Modify: `rust/crates/brain-eval/src/...`（找到评估完成 hook）

**Step 1: 写失败测试**

```rust
#[test]
fn eval_brain_writes_causal_chain_on_eval_complete() {
    let graph = Arc::new(make_test_graph());
    let eval = EvalBrain::with_graph(graph.clone());

    let result = EvaluationResult {
        quality_score: 0.4,
        issues: vec!["工具失败未告知用户".into()],
    };
    eval.write_causal_chain(&result, session_id, tool_calls).unwrap();

    // 验证图谱有 Pitfall Concept + CausedBy 边
    let r = graph.recall(&QueryOptions {
        keywords: vec!["pitfall".into()],
        graph_type: GraphType::Memory,
        ..Default::default()
    });
    assert!(matches!(r, ToolResult::Ok(_)));
}
```

**Step 2-5: 实现 + commit**

```bash
git commit -m "feat(brain-eval): 评估完成写入因果链"
```

---

## Phase 7：端到端测试

### Task 7.1：集成测试套件

**Files:** Create: `rust/crates/brain-graph/tests/integration_test.rs`

**Step 1: 写 8 个端到端测试**

```rust
#[test]
fn end_to_end_recall_finds_related_concepts() {
    // 建图：Concept("ICS2") -[:MentionedIn]-> Memory(task-045)
    //       Entity("亿通") -[:MentionedIn]-> Memory(task-045)
    // recall(["ics2","亿通"], Memory) → 应找到 3 节点 + 2 边
}

#[test]
fn end_to_end_code_explore_and_drill() {
    // 建 Code 图谱：File progressive_recall.rs -[:Contains]-> Function find_in_summary
    //              Function recall -[:Calls]-> Function find_in_summary
    // recall(["find_in_summary"], Code) → drill find_in_summary depth=2 → 找到 recall
}

#[test]
fn end_to_end_memory_sync_after_l2_write() {
    // MemoryBrain.write_l2_summary() → graph 自动镜像
    // recall task_id → 找到 Memory 节点
}

#[test]
fn end_to_end_reconciliation_at_startup() {
    // 手动插入金字塔 L2 task，但不写图谱
    // BrainGraph::open() 触发 reconciliation → 补建节点
}

#[test]
fn end_to_end_causal_chain_after_eval() {
    // EvalBrain 评估完成 → 因果链写入
    // recall pitfall → 找到 Concept + CausedBy 边
}

#[test]
fn end_to_end_alias_expansion() {
    // Concept("ICS2", aliases=["ics2","ICS-2"])
    // recall ["ics2"] → 命中（即使节点名是 ICS2）
}

#[test]
fn end_to_end_token_budget_truncation() {
    // 插入 50 个匹配节点，预算 200 tokens → 只返回 ~13 个 Brief
    // truncated=true，drill_hints 非空
}

#[test]
fn end_to_end_graph_type_isolation() {
    // 同时插入 Memory + Code 节点
    // recall Memory → 不返回 Code 节点
}
```

**Step 2-5: 实现辅助 setup 函数 + 跑测试 + commit**

```bash
cargo test -p brain-graph --test integration_test
git commit -m "test(graph): 8 个端到端集成测试"
```

---

### Task 7.2：性能基准测试

**Files:** Create: `rust/crates/brain-graph/tests/bench_test.rs`

**Step 1: 写基准**

```rust
#[test]
fn bench_recall_10k_nodes_under_100ms() {
    let (g, _tmp) = make_graph_with_seed(10_000);
    let start = std::time::Instant::now();
    let _ = g.recall(&QueryOptions {
        keywords: vec!["test".into()],
        graph_type: GraphType::Memory,
        ..Default::default()
    });
    let elapsed = start.elapsed();
    assert!(elapsed.as_millis() < 100, "P99 应 < 100ms，实际 {}ms", elapsed.as_millis());
}

#[test]
fn bench_drill_depth_2_under_50ms() { /* ... */ }

#[test]
fn bench_add_node_under_10ms() { /* ... */ }
```

**Step 2-5: 实现 seed 函数 + 跑基准 + commit**

```bash
cargo test -p brain-graph --test bench_test --release
git commit -m "test(graph): 性能基准测试"
```

---

## 完成验证

### 最终冒烟测试

```bash
# 全 workspace 测试
cargo test --workspace

# 单独跑图谱测试
cargo test -p brain-graph --all

# 单独跑工具测试
cargo test -p tools graph_

# 单独跑三脑集成测试
cargo test -p brain-main prompts::
cargo test -p brain-memory pyramid_memory_brain::
cargo test -p brain-eval causal_chain::

# clippy
cargo clippy --workspace --all-targets -- -D warnings

# fmt
cargo fmt --all -- --check
```

### 完成标志

- [ ] 7 个 Phase 全部 commit
- [ ] ~85 个测试全部通过
- [ ] cargo clippy 无警告
- [ ] cargo fmt 无差异
- [ ] 设计文档 + 实施文档已 commit
- [ ] Orchestrator 启动时正确注册图谱工具
- [ ] MainBrain system prompt 包含图谱引导

---

## 风险与备注

1. **rusqlite bundled** 增加 ~2MB 二进制体积，但避免系统依赖问题
2. **三脑注入顺序**：Orchestrator 必须先开 graph，再构造三脑（避免顺序错）
3. **tempfile 测试**：测试函数里必须保留 `_tmp` 变量防止目录被清理
4. **Memory 节点 ID 唯一性**：同 ref_id 多次写入会创建多个节点，需要业务层去重（按 props.ref_id 查询）
5. **persona_id 字段**：当前 MVP 不强约束，节点 props 可加可选 persona_id 字段，留待 v2 完善隔离

---

## 执行选择

**Plan complete and saved to `docs/plans/2026-06-26-native-graph-impl.md`.**

两种执行模式：

1. **Subagent-Driven（当前会话）**：我每个 Task 派一个新 subagent，任务间代码评审，快速迭代
2. **Parallel Session（独立会话）**：开新会话用 `executing-plans` skill，批量执行带检查点

**哪种？**
