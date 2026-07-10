//! 知识图谱类型定义（见设计文档 3.1-3.7）
//!
//! 5 类 NodeKind × 4+Custom GraphType × 11 种 EdgeKind。
//! 二维分类：NodeKind（纵向基础类型）× GraphType（横向域）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 节点基础类型（5 类）
///
/// - Memory: 金字塔代理节点（L1/L2/L3/L4）
/// - Concept: 抽象概念
/// - Entity: 具体实体（人物/项目/技术栈/角色）
/// - Tool: 工具/Skill/MCP
/// - Code: 代码节点（File/Function/Class/Module）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Memory,
    Concept,
    Entity,
    Tool,
    Code,
}

/// 图谱域（4 个固定域 + Custom 兜底）
///
/// 同一 Entity 在不同域是**独立节点**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphType {
    Memory,
    Code,
    Novel,
    Video,
    /// 兜底自定义域（MVP 数据层支持，不开放创建工具）
    Custom(String),
}

/// 边类型（11 种：6 通用语义 + 4 代码专属 + 1 工具调用）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    // === 通用语义边 ===
    /// (Concept/Entity) -[:MentionedIn]-> (Memory)
    MentionedIn,
    /// 通用兜底关联
    RelatedTo,
    /// 同类相似
    SimilarTo,
    /// 因果
    CausedBy,
    /// 依赖
    DependsOn,
    /// 派生
    DerivedFrom,

    // === 代码专属边 ===
    /// Function -[:Calls]-> Function
    Calls,
    /// File/Class -[:Contains]-> Function/Class
    Contains,
    /// File -[:Imports]-> File/Module
    Imports,
    /// File -[:Defines]-> Function/Class/Variable
    Defines,

    // === 工具调用边 ===
    /// Memory -[:Invokes]-> Tool
    Invokes,
}

/// 图谱节点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// 节点 ID: `{graph_type}_{kind}_{uuid8}`
    pub id: String,
    pub kind: NodeKind,
    pub graph_type: GraphType,
    /// 领域特定字段（见设计 3.6 推荐字段表）
    pub props: HashMap<String, Value>,
    /// 重要度 0.0-1.0
    pub importance: f64,
    /// 创建时间（Unix ms）
    pub created_at: i64,
    /// 最后访问时间（Unix ms）
    pub last_accessed: i64,
    /// 是否已被取代（true 的节点默认不返回）
    pub superseded: bool,
}

/// 图谱边
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub src: String,
    pub dst: String,
    pub kind: EdgeKind,
    pub props: HashMap<String, Value>,
    pub created_at: i64,
    /// 权重 0.0-1.0
    pub weight: f64,
}

/// 查询返回的详情级别（控制 token 预算）
///
/// - Brief: ~15 tokens（仅 name/id）
/// - WithSummary: ~50 tokens（name + summary）
/// - Full: ~150 tokens（完整 props）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DetailLevel {
    Brief,
    WithSummary,
    Full,
}

/// 带评分的节点（查询结果项）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredNode {
    pub node: Node,
    /// 综合评分 0.0-1.0
    pub score: f64,
    /// 命中的关键词列表
    pub matched_keywords: Vec<String>,
    pub detail_level: DetailLevel,
}

/// 子图（recall/drill 的返回结构）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraph {
    /// 已按 score 降序排列、已按 token 预算裁剪
    pub nodes: Vec<ScoredNode>,
    /// **仅** nodes 列表内节点之间的边
    pub edges: Vec<Edge>,
    /// 去重前的总匹配数（用于判断是否有更多数据）
    pub total_found: usize,
    /// 是否因 token 预算被截断
    pub truncated: bool,
    /// 下钻提示（例如"还有 N 个节点未返回，可 drill 获取"）
    pub drill_hints: Vec<String>,
}

/// 域信息（list_domains 返回项）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainInfo {
    pub graph_type: GraphType,
    pub node_count: usize,
    pub edge_count: usize,
    pub last_updated: Option<i64>,
    pub description: String,
}

/// Catalog-first search result item.
///
/// This is the low-context discovery surface for broad keywords. It avoids
/// returning summaries, full props, edge lists, or source text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub node_id: String,
    pub title: String,
    pub catalog_type: Option<String>,
    pub matched_keywords: Vec<String>,
    pub score: f64,
    pub hint: Option<String>,
}

/// Catalog search response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSearchResult {
    pub entries: Vec<CatalogEntry>,
    pub total_found: usize,
    pub truncated: bool,
}

/// Direction of a directly connected neighbor relative to the center node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NeighborDirection {
    Upstream,
    Downstream,
}

/// Compact neighbor returned by `get_node_detail`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeighborSummary {
    pub node_id: String,
    pub kind: NodeKind,
    pub graph_type: GraphType,
    pub title: String,
    pub edge_kind: EdgeKind,
    pub direction: NeighborDirection,
    pub weight: f64,
}

/// Detail view for one selected node.
///
/// Source references are returned as unresolved JSON values. Resolving them to
/// raw memory paragraphs belongs to the memory layer, not this storage crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDetail {
    pub center: Node,
    pub upstream: Vec<NeighborSummary>,
    pub downstream: Vec<NeighborSummary>,
    pub source_refs: Vec<Value>,
}

/// Trace direction for relationship traversal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceDirection {
    Upstream,
    Downstream,
    Both,
}

/// One compact step in a memory trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    pub depth: usize,
    pub from_node_id: String,
    pub to_node_id: String,
    pub title: String,
    pub kind: NodeKind,
    pub graph_type: GraphType,
    pub edge_kind: EdgeKind,
    pub direction: NeighborDirection,
    pub weight: f64,
}

/// Budget-limited trace result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceResult {
    pub root: Node,
    pub steps: Vec<TraceStep>,
    pub truncated: bool,
    pub max_depth: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_serde_roundtrip() {
        for kind in [
            NodeKind::Memory,
            NodeKind::Concept,
            NodeKind::Entity,
            NodeKind::Tool,
            NodeKind::Code,
        ] {
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
        for e in [
            EdgeKind::MentionedIn,
            EdgeKind::Calls,
            EdgeKind::Invokes,
            EdgeKind::RelatedTo,
        ] {
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
            props: HashMap::from([("name".into(), serde_json::json!("红冲逻辑"))]),
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
