//! SQLite-backed graph store.
//!
//! The store is intentionally small at this stage: it provides a durable
//! node/edge layer plus keyword recall over node props. Higher-level tools can
//! build domain-specific extraction and ranking on top of this API.

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use crate::error::{BrainGraphError, Result, ToolResult};
use crate::schema::{
    CatalogEntry, CatalogSearchResult, DetailLevel, DomainInfo, Edge, GraphType, NeighborDirection,
    NeighborSummary, Node, NodeDetail, ScoredNode, SubGraph, TraceDirection, TraceResult,
    TraceStep,
};

#[derive(Debug, Clone)]
pub struct RecallQuery {
    pub graph_type: Option<GraphType>,
    pub keywords: Vec<String>,
    pub detail_level: DetailLevel,
    pub limit: usize,
}

impl RecallQuery {
    #[must_use]
    pub fn new(keywords: Vec<String>) -> Self {
        Self {
            graph_type: None,
            keywords,
            detail_level: DetailLevel::WithSummary,
            limit: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CatalogQuery {
    pub graph_type: Option<GraphType>,
    pub keywords: Vec<String>,
    pub limit: usize,
}

impl CatalogQuery {
    #[must_use]
    pub fn new(keywords: Vec<String>) -> Self {
        Self {
            graph_type: Some(GraphType::Memory),
            keywords,
            limit: 10,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TraceQuery {
    pub root_id: String,
    pub direction: TraceDirection,
    pub max_depth: usize,
    pub limit: usize,
}

impl TraceQuery {
    #[must_use]
    pub fn new(root_id: impl Into<String>) -> Self {
        Self {
            root_id: root_id.into(),
            direction: TraceDirection::Both,
            max_depth: 2,
            limit: 20,
        }
    }
}

pub struct GraphStore {
    conn: Connection,
}

impl GraphStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(BrainGraphError::from)?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(BrainGraphError::from)?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        self.conn
            .execute_batch(
                r"
                PRAGMA foreign_keys = ON;
                PRAGMA busy_timeout = 5000;

                CREATE TABLE IF NOT EXISTS nodes (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    graph_type TEXT NOT NULL,
                    props TEXT NOT NULL,
                    importance REAL NOT NULL,
                    created_at INTEGER NOT NULL,
                    last_accessed INTEGER NOT NULL,
                    superseded INTEGER NOT NULL DEFAULT 0
                );

                CREATE INDEX IF NOT EXISTS idx_nodes_graph_type
                    ON nodes(graph_type);
                CREATE INDEX IF NOT EXISTS idx_nodes_superseded
                    ON nodes(superseded);

                CREATE TABLE IF NOT EXISTS edges (
                    src TEXT NOT NULL,
                    dst TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    props TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    weight REAL NOT NULL,
                    PRIMARY KEY (src, dst, kind),
                    FOREIGN KEY(src) REFERENCES nodes(id) ON DELETE CASCADE,
                    FOREIGN KEY(dst) REFERENCES nodes(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src);
                CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);
                ",
            )
            .map_err(BrainGraphError::from)
    }

    pub fn upsert_node(&self, node: &Node) -> Result<()> {
        validate_score(node.importance, "importance")?;
        self.conn
            .execute(
                r"
                INSERT INTO nodes (
                    id, kind, graph_type, props, importance, created_at, last_accessed, superseded
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    graph_type = excluded.graph_type,
                    props = excluded.props,
                    importance = excluded.importance,
                    created_at = excluded.created_at,
                    last_accessed = excluded.last_accessed,
                    superseded = excluded.superseded
                ",
                params![
                    node.id,
                    serde_json::to_string(&node.kind)?,
                    serde_json::to_string(&node.graph_type)?,
                    serde_json::to_string(&node.props)?,
                    node.importance,
                    node.created_at,
                    node.last_accessed,
                    i64::from(node.superseded),
                ],
            )
            .map_err(BrainGraphError::from)?;
        Ok(())
    }

    pub fn insert_edge(&self, edge: &Edge) -> Result<()> {
        validate_score(edge.weight, "weight")?;
        self.conn
            .execute(
                r"
                INSERT INTO edges (src, dst, kind, props, created_at, weight)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(src, dst, kind) DO UPDATE SET
                    props = excluded.props,
                    created_at = excluded.created_at,
                    weight = excluded.weight
                ",
                params![
                    edge.src,
                    edge.dst,
                    serde_json::to_string(&edge.kind)?,
                    serde_json::to_string(&edge.props)?,
                    edge.created_at,
                    edge.weight,
                ],
            )
            .map_err(BrainGraphError::from)?;
        Ok(())
    }

    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.conn
            .query_row(
                r"
                SELECT id, kind, graph_type, props, importance, created_at, last_accessed, superseded
                FROM nodes
                WHERE id = ?1
                ",
                [id],
                row_to_node,
            )
            .optional()
            .map_err(BrainGraphError::from)?
            .transpose()
    }

    pub fn mark_superseded(&self, id: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute("UPDATE nodes SET superseded = 1 WHERE id = ?1", [id])
            .map_err(BrainGraphError::from)?;
        Ok(changed > 0)
    }

    pub fn list_domains(&self) -> Result<Vec<DomainInfo>> {
        let mut stmt = self
            .conn
            .prepare(
                r"
                SELECT
                    n.graph_type,
                    COUNT(DISTINCT n.id) AS node_count,
                    COUNT(DISTINCT e.src || char(31) || e.dst || char(31) || e.kind) AS edge_count,
                    MAX(n.last_accessed) AS last_updated
                FROM nodes n
                LEFT JOIN edges e ON e.src = n.id OR e.dst = n.id
                WHERE n.superseded = 0
                GROUP BY n.graph_type
                ORDER BY n.graph_type
                ",
            )
            .map_err(BrainGraphError::from)?;

        let rows = stmt
            .query_map([], |row| {
                let graph_type_json: String = row.get(0)?;
                let graph_type: GraphType =
                    serde_json::from_str(&graph_type_json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                Ok(DomainInfo {
                    description: domain_description(&graph_type),
                    graph_type,
                    node_count: row.get::<_, i64>(1)? as usize,
                    edge_count: row.get::<_, i64>(2)? as usize,
                    last_updated: row.get(3)?,
                })
            })
            .map_err(BrainGraphError::from)?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(BrainGraphError::from)
    }

    pub fn search_catalog(&self, query: &CatalogQuery) -> Result<ToolResult<CatalogSearchResult>> {
        if query.limit == 0 {
            return Err(BrainGraphError::InvalidInput("limit must be > 0".into()));
        }

        let candidates = self.load_candidates(query.graph_type.as_ref())?;
        let searched_nodes = candidates.len();
        let searched_edges = self.count_edges()?;
        let normalized_keywords = normalize_keywords(&query.keywords);

        let mut entries = Vec::new();
        for node in candidates {
            let Some(title) = catalog_title(&node) else {
                continue;
            };
            let matched_keywords = catalog_matched_keywords(&node, &normalized_keywords);
            if !normalized_keywords.is_empty() && matched_keywords.is_empty() {
                continue;
            }

            entries.push(CatalogEntry {
                node_id: node.id.clone(),
                title,
                catalog_type: string_prop(&node, "catalog_type"),
                score: catalog_score(&node, matched_keywords.len(), normalized_keywords.len()),
                matched_keywords,
                hint: string_prop(&node, "catalog_hint"),
            });
        }

        if entries.is_empty() {
            return Ok(ToolResult::Empty {
                searched_nodes,
                searched_edges,
                hint: Some("未命中 catalog，可尝试更宽泛关键词或检查 graph_type".into()),
            });
        }

        entries.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let total_found = entries.len();
        let truncated = entries.len() > query.limit;
        entries.truncate(query.limit);

        Ok(ToolResult::Ok {
            data: CatalogSearchResult {
                entries,
                total_found,
                truncated,
            },
        })
    }

    pub fn get_node_detail(&self, id: &str) -> Result<ToolResult<NodeDetail>> {
        let Some(center) = self.get_node(id)? else {
            return Ok(ToolResult::Empty {
                searched_nodes: 0,
                searched_edges: self.count_edges()?,
                hint: Some(format!("节点不存在: {id}")),
            });
        };

        if center.superseded {
            return Ok(ToolResult::Empty {
                searched_nodes: 1,
                searched_edges: self.count_edges()?,
                hint: Some(format!("节点已被 superseded: {id}")),
            });
        }

        self.touch_nodes(std::iter::once(center.id.as_str()))?;
        let (upstream, downstream) = self.direct_neighbors(&center.id)?;
        Ok(ToolResult::Ok {
            data: NodeDetail {
                source_refs: source_refs(&center),
                center,
                upstream,
                downstream,
            },
        })
    }

    pub fn trace_memory(&self, query: &TraceQuery) -> Result<ToolResult<TraceResult>> {
        if query.max_depth == 0 {
            return Err(BrainGraphError::InvalidInput(
                "max_depth must be > 0".into(),
            ));
        }
        if query.limit == 0 {
            return Err(BrainGraphError::InvalidInput("limit must be > 0".into()));
        }

        let Some(root) = self.get_node(&query.root_id)? else {
            return Ok(ToolResult::Empty {
                searched_nodes: 0,
                searched_edges: self.count_edges()?,
                hint: Some(format!("根节点不存在: {}", query.root_id)),
            });
        };
        if root.superseded {
            return Ok(ToolResult::Empty {
                searched_nodes: 1,
                searched_edges: self.count_edges()?,
                hint: Some(format!("根节点已被 superseded: {}", query.root_id)),
            });
        }

        let mut queue = VecDeque::from([(root.id.clone(), 0usize)]);
        let mut visited = HashSet::from([root.id.clone()]);
        let mut touched = HashSet::from([root.id.clone()]);
        let mut steps = Vec::new();
        let mut truncated = false;

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= query.max_depth {
                continue;
            }

            let edges = self.connected_edges(&current_id, &query.direction)?;
            let edge_count = edges.len();
            for (edge_index, edge) in edges.into_iter().enumerate() {
                let (neighbor_id, direction) = edge_neighbor(&edge, &current_id);
                let Some(neighbor) = self.get_node(neighbor_id)? else {
                    continue;
                };
                if neighbor.superseded {
                    continue;
                }

                steps.push(TraceStep {
                    depth: depth + 1,
                    from_node_id: current_id.clone(),
                    to_node_id: neighbor.id.clone(),
                    title: display_title(&neighbor),
                    kind: neighbor.kind,
                    graph_type: neighbor.graph_type,
                    edge_kind: edge.kind,
                    direction,
                    weight: edge.weight,
                });
                touched.insert(neighbor.id.clone());

                if steps.len() >= query.limit {
                    truncated = edge_index + 1 < edge_count
                        || self.has_more_trace_edges(&neighbor.id, depth + 1, query)?;
                    queue.clear();
                    break;
                }

                if visited.insert(neighbor.id.clone()) {
                    queue.push_back((neighbor.id, depth + 1));
                }
            }
        }

        self.touch_nodes(touched.iter().map(String::as_str))?;
        Ok(ToolResult::Ok {
            data: TraceResult {
                root,
                steps,
                truncated,
                max_depth: query.max_depth,
            },
        })
    }

    pub fn recall(&self, query: &RecallQuery) -> Result<ToolResult<SubGraph>> {
        if query.limit == 0 {
            return Err(BrainGraphError::InvalidInput("limit must be > 0".into()));
        }

        let candidates = self.load_candidates(query.graph_type.as_ref())?;
        let searched_nodes = candidates.len();
        let searched_edges = self.count_edges()?;
        let normalized_keywords = normalize_keywords(&query.keywords);

        let mut scored = Vec::new();
        for node in candidates {
            let matched_keywords = matched_keywords(&node, &normalized_keywords);
            if !normalized_keywords.is_empty() && matched_keywords.is_empty() {
                continue;
            }
            let score = score_node(&node, matched_keywords.len(), normalized_keywords.len());
            scored.push(ScoredNode {
                node,
                score,
                matched_keywords,
                detail_level: query.detail_level.clone(),
            });
        }

        if scored.is_empty() {
            return Ok(ToolResult::Empty {
                searched_nodes,
                searched_edges,
                hint: Some("未命中节点，可尝试更宽泛的关键词或检查 graph_type".into()),
            });
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let total_found = scored.len();
        let truncated = scored.len() > query.limit;
        scored.truncate(query.limit);
        self.touch_nodes(scored.iter().map(|item| item.node.id.as_str()))?;

        let selected_ids: HashSet<String> =
            scored.iter().map(|item| item.node.id.clone()).collect();
        let edges = self.edges_between(&selected_ids)?;
        let drill_hints = if truncated {
            vec![format!(
                "还有 {} 个节点未返回，可增加 limit 或缩小关键词后继续下钻",
                total_found - scored.len()
            )]
        } else {
            Vec::new()
        };

        Ok(ToolResult::Ok {
            data: SubGraph {
                nodes: scored,
                edges,
                total_found,
                truncated,
                drill_hints,
            },
        })
    }

    fn load_candidates(&self, graph_type: Option<&GraphType>) -> Result<Vec<Node>> {
        let sql = if graph_type.is_some() {
            r"
            SELECT id, kind, graph_type, props, importance, created_at, last_accessed, superseded
            FROM nodes
            WHERE superseded = 0 AND graph_type = ?1
            "
        } else {
            r"
            SELECT id, kind, graph_type, props, importance, created_at, last_accessed, superseded
            FROM nodes
            WHERE superseded = 0
            "
        };
        let mut stmt = self.conn.prepare(sql).map_err(BrainGraphError::from)?;
        if let Some(graph_type) = graph_type {
            let graph_type_json = serde_json::to_string(graph_type)?;
            let rows = stmt
                .query_map([graph_type_json], row_to_node)
                .map_err(BrainGraphError::from)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .collect::<Result<Vec<_>>>()
        } else {
            let rows = stmt
                .query_map([], row_to_node)
                .map_err(BrainGraphError::from)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .collect::<Result<Vec<_>>>()
        }
    }

    fn edges_between(&self, selected_ids: &HashSet<String>) -> Result<Vec<Edge>> {
        if selected_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare("SELECT src, dst, kind, props, created_at, weight FROM edges")
            .map_err(BrainGraphError::from)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Edge {
                    src: row.get(0)?,
                    dst: row.get(1)?,
                    kind: from_json_column(row, 2)?,
                    props: from_json_column(row, 3)?,
                    created_at: row.get(4)?,
                    weight: row.get(5)?,
                })
            })
            .map_err(BrainGraphError::from)?;

        let mut edges = Vec::new();
        for row in rows {
            let edge = row.map_err(BrainGraphError::from)?;
            if selected_ids.contains(&edge.src) && selected_ids.contains(&edge.dst) {
                edges.push(edge);
            }
        }
        Ok(edges)
    }

    fn touch_nodes<'a>(&self, ids: impl Iterator<Item = &'a str>) -> Result<()> {
        let now = now_ms();
        for id in ids {
            self.conn
                .execute(
                    "UPDATE nodes SET last_accessed = ?1 WHERE id = ?2",
                    params![now, id],
                )
                .map_err(BrainGraphError::from)?;
        }
        Ok(())
    }

    fn count_edges(&self) -> Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get::<_, i64>(0))
            .map(|count| count as usize)
            .map_err(BrainGraphError::from)
    }

    fn direct_neighbors(
        &self,
        center_id: &str,
    ) -> Result<(Vec<NeighborSummary>, Vec<NeighborSummary>)> {
        let mut stmt = self
            .conn
            .prepare(
                r"
                SELECT src, dst, kind, props, created_at, weight
                FROM edges
                WHERE src = ?1 OR dst = ?1
                ORDER BY weight DESC, created_at DESC
                ",
            )
            .map_err(BrainGraphError::from)?;
        let rows = stmt
            .query_map([center_id], |row| {
                Ok(Edge {
                    src: row.get(0)?,
                    dst: row.get(1)?,
                    kind: from_json_column(row, 2)?,
                    props: from_json_column(row, 3)?,
                    created_at: row.get(4)?,
                    weight: row.get(5)?,
                })
            })
            .map_err(BrainGraphError::from)?;

        let mut upstream = Vec::new();
        let mut downstream = Vec::new();
        for row in rows {
            let edge = row.map_err(BrainGraphError::from)?;
            let (neighbor_id, direction) = if edge.src == center_id {
                (&edge.dst, NeighborDirection::Downstream)
            } else {
                (&edge.src, NeighborDirection::Upstream)
            };
            let Some(neighbor) = self.get_node(neighbor_id)? else {
                continue;
            };
            if neighbor.superseded {
                continue;
            }
            let title = display_title(&neighbor);
            let summary = NeighborSummary {
                node_id: neighbor.id,
                kind: neighbor.kind,
                graph_type: neighbor.graph_type,
                title,
                edge_kind: edge.kind,
                direction: direction.clone(),
                weight: edge.weight,
            };
            match direction {
                NeighborDirection::Upstream => upstream.push(summary),
                NeighborDirection::Downstream => downstream.push(summary),
            }
        }

        Ok((upstream, downstream))
    }

    fn connected_edges(&self, center_id: &str, direction: &TraceDirection) -> Result<Vec<Edge>> {
        let sql = match direction {
            TraceDirection::Upstream => {
                r"
                SELECT src, dst, kind, props, created_at, weight
                FROM edges
                WHERE dst = ?1
                ORDER BY weight DESC, created_at DESC
                "
            }
            TraceDirection::Downstream => {
                r"
                SELECT src, dst, kind, props, created_at, weight
                FROM edges
                WHERE src = ?1
                ORDER BY weight DESC, created_at DESC
                "
            }
            TraceDirection::Both => {
                r"
                SELECT src, dst, kind, props, created_at, weight
                FROM edges
                WHERE src = ?1 OR dst = ?1
                ORDER BY weight DESC, created_at DESC
                "
            }
        };
        let mut stmt = self.conn.prepare(sql).map_err(BrainGraphError::from)?;
        let rows = stmt
            .query_map([center_id], |row| {
                Ok(Edge {
                    src: row.get(0)?,
                    dst: row.get(1)?,
                    kind: from_json_column(row, 2)?,
                    props: from_json_column(row, 3)?,
                    created_at: row.get(4)?,
                    weight: row.get(5)?,
                })
            })
            .map_err(BrainGraphError::from)?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(BrainGraphError::from)
    }

    fn has_more_trace_edges(
        &self,
        current_id: &str,
        current_depth: usize,
        query: &TraceQuery,
    ) -> Result<bool> {
        if current_depth >= query.max_depth {
            return Ok(false);
        }
        Ok(!self
            .connected_edges(current_id, &query.direction)?
            .is_empty())
    }
}

fn row_to_node(row: &Row<'_>) -> rusqlite::Result<Result<Node>> {
    Ok(Ok(Node {
        id: row.get(0)?,
        kind: from_json_column(row, 1)?,
        graph_type: from_json_column(row, 2)?,
        props: from_json_column(row, 3)?,
        importance: row.get(4)?,
        created_at: row.get(5)?,
        last_accessed: row.get(6)?,
        superseded: row.get::<_, i64>(7)? != 0,
    }))
}

fn from_json_column<T: serde::de::DeserializeOwned>(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn validate_score(value: f64, field: &str) -> Result<()> {
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(BrainGraphError::InvalidInput(format!(
            "{field} must be between 0.0 and 1.0"
        )))
    }
}

fn normalize_keywords(keywords: &[String]) -> Vec<String> {
    keywords
        .iter()
        .map(|kw| kw.trim().to_lowercase())
        .filter(|kw| !kw.is_empty())
        .collect()
}

fn matched_keywords(node: &Node, keywords: &[String]) -> Vec<String> {
    if keywords.is_empty() {
        return Vec::new();
    }
    let haystack = node_haystack(node);
    keywords
        .iter()
        .filter(|kw| haystack.contains(kw.as_str()))
        .cloned()
        .collect()
}

fn node_haystack(node: &Node) -> String {
    let mut parts = vec![node.id.clone()];
    for value in node.props.values() {
        collect_value_text(value, &mut parts);
    }
    parts.join(" ").to_lowercase()
}

fn collect_value_text(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
        Value::String(text) => output.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                collect_value_text(item, output);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                output.push(key.clone());
                collect_value_text(value, output);
            }
        }
    }
}

fn catalog_title(node: &Node) -> Option<String> {
    string_prop(node, "catalog_title")
        .or_else(|| string_prop(node, "title"))
        .or_else(|| string_prop(node, "name"))
}

fn display_title(node: &Node) -> String {
    catalog_title(node).unwrap_or_else(|| node.id.clone())
}

fn string_prop(node: &Node, key: &str) -> Option<String> {
    node.props
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn number_prop(node: &Node, key: &str) -> Option<f64> {
    node.props.get(key).and_then(Value::as_f64)
}

fn catalog_matched_keywords(node: &Node, keywords: &[String]) -> Vec<String> {
    if keywords.is_empty() {
        return Vec::new();
    }

    let mut parts = Vec::new();
    if let Some(title) = catalog_title(node) {
        parts.push(title);
    }
    if let Some(catalog_type) = string_prop(node, "catalog_type") {
        parts.push(catalog_type);
    }
    if let Some(hint) = string_prop(node, "catalog_hint") {
        parts.push(hint);
    }
    if let Some(Value::Array(items)) = node.props.get("catalog_keywords") {
        for item in items {
            if let Some(keyword) = item.as_str() {
                parts.push(keyword.to_owned());
            }
        }
    }

    let haystack = parts.join(" ").to_lowercase();
    keywords
        .iter()
        .filter(|keyword| haystack.contains(keyword.as_str()))
        .cloned()
        .collect()
}

fn catalog_score(node: &Node, matched_count: usize, keyword_count: usize) -> f64 {
    let rank_hint = number_prop(node, "catalog_rank_hint").unwrap_or(node.importance);
    let keyword_score = if keyword_count == 0 {
        0.0
    } else {
        matched_count as f64 / keyword_count as f64
    };
    ((node.importance * 0.45) + (rank_hint * 0.25) + (keyword_score * 0.3)).clamp(0.0, 1.0)
}

fn source_refs(node: &Node) -> Vec<Value> {
    let mut refs = Vec::new();
    for key in ["l1_refs", "source_refs", "refs"] {
        match node.props.get(key) {
            Some(Value::Array(items)) => refs.extend(items.iter().cloned()),
            Some(value) => refs.push(value.clone()),
            None => {}
        }
    }
    refs
}

fn edge_neighbor<'a>(edge: &'a Edge, center_id: &str) -> (&'a str, NeighborDirection) {
    if edge.src == center_id {
        (&edge.dst, NeighborDirection::Downstream)
    } else {
        (&edge.src, NeighborDirection::Upstream)
    }
}

fn score_node(node: &Node, matched_count: usize, keyword_count: usize) -> f64 {
    let keyword_score = if keyword_count == 0 {
        1.0
    } else {
        matched_count as f64 / keyword_count as f64
    };
    (node.importance * 0.6) + (keyword_score * 0.4)
}

fn domain_description(graph_type: &GraphType) -> String {
    match graph_type {
        GraphType::Memory => "memory graph".into(),
        GraphType::Code => "code graph".into(),
        GraphType::Novel => "novel graph".into(),
        GraphType::Video => "video graph".into(),
        GraphType::Custom(name) => format!("custom graph: {name}"),
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::gen_node_id;
    use crate::schema::{EdgeKind, NodeKind};
    use std::collections::HashMap;

    fn node(graph_type: GraphType, kind: NodeKind, name: &str, importance: f64) -> Node {
        let now = now_ms();
        Node {
            id: gen_node_id(graph_type.clone(), kind.clone()),
            kind,
            graph_type,
            props: HashMap::from([
                ("name".into(), serde_json::json!(name)),
                (
                    "summary".into(),
                    serde_json::json!(format!("{name} summary")),
                ),
            ]),
            importance,
            created_at: now,
            last_accessed: now,
            superseded: false,
        }
    }

    fn catalog_node(title: &str, keywords: &[&str], importance: f64) -> Node {
        let mut node = node(GraphType::Memory, NodeKind::Memory, title, importance);
        node.props
            .insert("catalog_title".into(), serde_json::json!(title));
        node.props
            .insert("catalog_keywords".into(), serde_json::json!(keywords));
        node.props
            .insert("catalog_type".into(), serde_json::json!("business_logic"));
        node.props
            .insert("catalog_rank_hint".into(), serde_json::json!(importance));
        node
    }

    fn link(store: &GraphStore, src: &Node, dst: &Node, kind: EdgeKind, weight: f64) {
        store
            .insert_edge(&Edge {
                src: src.id.clone(),
                dst: dst.id.clone(),
                kind,
                props: HashMap::new(),
                created_at: now_ms(),
                weight,
            })
            .unwrap();
    }

    #[test]
    fn upsert_and_get_node_roundtrip() {
        let store = GraphStore::in_memory().unwrap();
        let original = node(GraphType::Memory, NodeKind::Concept, "红冲逻辑", 0.9);

        store.upsert_node(&original).unwrap();
        let loaded = store.get_node(&original.id).unwrap().unwrap();

        assert_eq!(loaded.id, original.id);
        assert_eq!(loaded.kind, NodeKind::Concept);
        assert_eq!(loaded.graph_type, GraphType::Memory);
        assert_eq!(loaded.props.get("name"), original.props.get("name"));
    }

    #[test]
    fn insert_edge_and_recall_subgraph() {
        let store = GraphStore::in_memory().unwrap();
        let a = node(GraphType::Code, NodeKind::Code, "GraphStore", 0.8);
        let b = node(GraphType::Code, NodeKind::Code, "SQLite schema", 0.7);
        store.upsert_node(&a).unwrap();
        store.upsert_node(&b).unwrap();
        store
            .insert_edge(&Edge {
                src: a.id.clone(),
                dst: b.id.clone(),
                kind: EdgeKind::DependsOn,
                props: HashMap::new(),
                created_at: now_ms(),
                weight: 0.75,
            })
            .unwrap();

        let mut query = RecallQuery::new(vec!["graphstore".into(), "sqlite".into()]);
        query.graph_type = Some(GraphType::Code);
        query.limit = 10;

        let result = store.recall(&query).unwrap();
        let ToolResult::Ok { data } = result else {
            panic!("expected recall result");
        };
        assert_eq!(data.nodes.len(), 2);
        assert_eq!(data.edges.len(), 1);
        assert_eq!(data.total_found, 2);
    }

    #[test]
    fn recall_empty_reports_search_stats() {
        let store = GraphStore::in_memory().unwrap();
        store
            .upsert_node(&node(GraphType::Memory, NodeKind::Entity, "Gemini", 0.5))
            .unwrap();

        let result = store
            .recall(&RecallQuery::new(vec!["不存在的关键词".into()]))
            .unwrap();

        let ToolResult::Empty {
            searched_nodes,
            searched_edges,
            hint,
        } = result
        else {
            panic!("expected empty result");
        };
        assert_eq!(searched_nodes, 1);
        assert_eq!(searched_edges, 0);
        assert!(hint.is_some());
    }

    #[test]
    fn search_catalog_returns_low_context_entries() {
        let store = GraphStore::in_memory().unwrap();
        let a = catalog_node("红冲资费生成逻辑解释", &["红冲", "资费", "生成逻辑"], 0.9);
        let b = catalog_node("普通退款流程", &["退款"], 0.7);
        store.upsert_node(&a).unwrap();
        store.upsert_node(&b).unwrap();

        let result = store
            .search_catalog(&CatalogQuery::new(vec!["红冲".into(), "资费".into()]))
            .unwrap();

        let ToolResult::Ok { data } = result else {
            panic!("expected catalog result");
        };
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.total_found, 1);
        assert_eq!(data.entries[0].node_id, a.id);
        assert_eq!(data.entries[0].title, "红冲资费生成逻辑解释");
        assert_eq!(
            data.entries[0].catalog_type.as_deref(),
            Some("business_logic")
        );
        assert_eq!(data.entries[0].matched_keywords, vec!["红冲", "资费"]);
    }

    #[test]
    fn get_node_detail_returns_compact_neighbors_and_unresolved_refs() {
        let store = GraphStore::in_memory().unwrap();
        let mut center = catalog_node("红冲资费推送 BMS 异步调用", &["红冲", "BMS"], 0.9);
        center.props.insert(
            "l1_refs".into(),
            serde_json::json!([
                {
                    "session": "sess-001",
                    "file": "personas/default/pyramid/l1-raw/sess-001.jsonl",
                    "paragraphs": [8, 9]
                }
            ]),
        );
        let upstream = catalog_node("红冲资费生成逻辑解释", &["红冲", "资费"], 0.8);
        let downstream = catalog_node("BMS 推送失败重试", &["BMS", "重试"], 0.7);

        store.upsert_node(&center).unwrap();
        store.upsert_node(&upstream).unwrap();
        store.upsert_node(&downstream).unwrap();
        store
            .insert_edge(&Edge {
                src: upstream.id.clone(),
                dst: center.id.clone(),
                kind: EdgeKind::DependsOn,
                props: HashMap::new(),
                created_at: now_ms(),
                weight: 0.8,
            })
            .unwrap();
        store
            .insert_edge(&Edge {
                src: center.id.clone(),
                dst: downstream.id.clone(),
                kind: EdgeKind::DerivedFrom,
                props: HashMap::new(),
                created_at: now_ms(),
                weight: 0.7,
            })
            .unwrap();

        let result = store.get_node_detail(&center.id).unwrap();

        let ToolResult::Ok { data } = result else {
            panic!("expected detail result");
        };
        assert_eq!(data.center.id, center.id);
        assert_eq!(data.upstream.len(), 1);
        assert_eq!(data.downstream.len(), 1);
        assert_eq!(data.upstream[0].title, "红冲资费生成逻辑解释");
        assert_eq!(data.downstream[0].title, "BMS 推送失败重试");
        assert_eq!(data.source_refs.len(), 1);
        assert_eq!(data.source_refs[0]["session"], "sess-001");
    }

    #[test]
    fn trace_memory_follows_direction_and_depth() {
        let store = GraphStore::in_memory().unwrap();
        let root = catalog_node("红冲资费推送 BMS 异步调用", &["红冲", "BMS"], 0.9);
        let upstream_1 = catalog_node("红冲资费生成逻辑解释", &["红冲", "资费"], 0.8);
        let upstream_2 = catalog_node("修改红冲原费用逻辑", &["红冲", "原费用"], 0.7);
        let downstream = catalog_node("BMS 推送失败重试", &["BMS", "重试"], 0.6);

        for node in [&root, &upstream_1, &upstream_2, &downstream] {
            store.upsert_node(node).unwrap();
        }
        link(&store, &upstream_2, &upstream_1, EdgeKind::DerivedFrom, 0.9);
        link(&store, &upstream_1, &root, EdgeKind::DependsOn, 0.8);
        link(&store, &root, &downstream, EdgeKind::CausedBy, 0.7);

        let mut query = TraceQuery::new(root.id.clone());
        query.direction = TraceDirection::Upstream;
        query.max_depth = 2;
        query.limit = 10;
        let result = store.trace_memory(&query).unwrap();

        let ToolResult::Ok { data } = result else {
            panic!("expected trace result");
        };
        assert_eq!(data.root.id, root.id);
        assert_eq!(data.steps.len(), 2);
        assert!(!data.truncated);
        assert_eq!(data.steps[0].title, "红冲资费生成逻辑解释");
        assert_eq!(data.steps[0].direction, NeighborDirection::Upstream);
        assert_eq!(data.steps[0].depth, 1);
        assert_eq!(data.steps[1].title, "修改红冲原费用逻辑");
        assert_eq!(data.steps[1].depth, 2);
    }

    #[test]
    fn trace_memory_respects_limit_and_marks_truncated() {
        let store = GraphStore::in_memory().unwrap();
        let root = catalog_node("红冲入口", &["红冲"], 0.9);
        let a = catalog_node("红冲分支 A", &["红冲"], 0.8);
        let b = catalog_node("红冲分支 B", &["红冲"], 0.7);

        for node in [&root, &a, &b] {
            store.upsert_node(node).unwrap();
        }
        link(&store, &root, &a, EdgeKind::RelatedTo, 0.9);
        link(&store, &root, &b, EdgeKind::RelatedTo, 0.8);

        let mut query = TraceQuery::new(root.id);
        query.direction = TraceDirection::Downstream;
        query.max_depth = 1;
        query.limit = 1;
        let result = store.trace_memory(&query).unwrap();

        let ToolResult::Ok { data } = result else {
            panic!("expected trace result");
        };
        assert_eq!(data.steps.len(), 1);
        assert!(data.truncated);
    }

    #[test]
    fn list_domains_counts_active_nodes_and_edges() {
        let store = GraphStore::in_memory().unwrap();
        let a = node(GraphType::Memory, NodeKind::Concept, "记忆脑", 0.8);
        let b = node(GraphType::Memory, NodeKind::Entity, "用户偏好", 0.6);
        store.upsert_node(&a).unwrap();
        store.upsert_node(&b).unwrap();
        store
            .insert_edge(&Edge {
                src: a.id.clone(),
                dst: b.id.clone(),
                kind: EdgeKind::RelatedTo,
                props: HashMap::new(),
                created_at: now_ms(),
                weight: 0.5,
            })
            .unwrap();

        let domains = store.list_domains().unwrap();

        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].graph_type, GraphType::Memory);
        assert_eq!(domains[0].node_count, 2);
        assert_eq!(domains[0].edge_count, 1);
    }

    #[test]
    fn superseded_nodes_are_hidden_from_recall() {
        let store = GraphStore::in_memory().unwrap();
        let node = node(GraphType::Memory, NodeKind::Concept, "旧设计", 0.8);
        store.upsert_node(&node).unwrap();
        assert!(store.mark_superseded(&node.id).unwrap());

        let result = store
            .recall(&RecallQuery::new(vec!["旧设计".into()]))
            .unwrap();

        assert!(matches!(result, ToolResult::Empty { .. }));
    }
}
