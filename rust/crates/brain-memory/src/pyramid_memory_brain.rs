//! 金字塔版记忆脑
//!
//! 基于 per-persona 金字塔存储的新 MemoryBrain。
//! 逐步替代旧 MemoryBrain，保持对外接口兼容。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use brain_core::types::{BrainId, KnowledgeSource, MemoryEntry, MemoryLayer, TurnRecord};
use brain_graph::error::ToolResult;
use brain_graph::schema::{Edge, EdgeKind, GraphType, Node, NodeKind};
use brain_graph::store::{CatalogQuery, GraphStore};
use serde_json::json;

use crate::abstract_layer::AbstractLayer;
use crate::concentration::{ConcentrationEngine, ConcentrationReport};
use crate::conversation_memory::{
    ConversationMemoryInvalidation, ConversationMemoryScope, ConversationMemoryStore,
};
use crate::error::Result;
use crate::persona_manager::PersonaManager;
use crate::profile_eval::{EvalInfoStore, ProfileStore};
use crate::progressive_recall::{InjectContext, ProgressiveRecall};
use crate::pyramid_storage::PyramidStorage;
use crate::raw_pool::RawPool;
use crate::subconscious_pool::SubconsciousPool;
use crate::summary_pool::SummaryPool;

use crate::pyramid_types::TaskSummary;

fn task_summary_to_graph_node(storage: &PyramidStorage, task: &TaskSummary) -> Node {
    let source_refs = task_summary_source_refs(storage, task);
    let mut props = HashMap::from([
        ("layer".into(), json!("task_summary")),
        ("task_id".into(), json!(&task.task_id)),
        ("task_type".into(), json!(format!("{:?}", task.task_type))),
        ("task_name".into(), json!(&task.task_name)),
        ("catalog_title".into(), json!(&task.task_name)),
        ("catalog_keywords".into(), json!(&task.tags)),
        ("catalog_type".into(), json!("task_summary")),
        ("summary".into(), json!(&task.summary)),
        ("l1_refs".into(), json!(&task.l1_refs)),
        ("source_refs".into(), json!(source_refs)),
    ]);
    props.insert(
        "catalog_hint".into(),
        json!(format!("L2 task summary: {}", task.task_id)),
    );

    Node {
        id: stable_l2_graph_node_id(&task.task_id),
        kind: NodeKind::Memory,
        graph_type: GraphType::Memory,
        props,
        importance: task.importance.clamp(0.0, 1.0),
        created_at: task.created_at.timestamp_millis(),
        last_accessed: task.updated_at.timestamp_millis(),
        superseded: false,
    }
}

fn task_summary_source_refs(
    storage: &PyramidStorage,
    task: &TaskSummary,
) -> Vec<serde_json::Value> {
    task.l1_refs
        .iter()
        .map(|reference| {
            let file = storage.l1_session_path(&reference.session);
            let relative_file = file
                .strip_prefix(storage.base_dir())
                .map(path_to_slash)
                .unwrap_or_else(|_| path_to_slash(&file));
            let mut line_numbers = reference
                .paragraphs
                .iter()
                .map(|paragraph| paragraph + 1)
                .collect::<Vec<_>>();
            line_numbers.sort_unstable();
            let line_start = line_numbers.first().copied();
            let line_end = line_numbers.last().copied();

            json!({
                "type": "l1_jsonl",
                "session": reference.session,
                "file": relative_file,
                "paragraphs": reference.paragraphs,
                "line_start": line_start,
                "line_end": line_end,
            })
        })
        .collect()
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn task_summary_graph_edges(tasks: &[TaskSummary]) -> Vec<Edge> {
    let mut edges = Vec::new();
    for (left_idx, left) in tasks.iter().enumerate() {
        for right in tasks.iter().skip(left_idx + 1) {
            let shared_tags = shared_normalized_values(&left.tags, &right.tags);
            let shared_sessions = shared_l1_sessions(left, right);

            if !shared_tags.is_empty() {
                edges.push(task_summary_edge(
                    left,
                    right,
                    EdgeKind::SimilarTo,
                    HashMap::from([
                        ("reason".into(), json!("shared_tags")),
                        ("shared_tags".into(), json!(shared_tags)),
                    ]),
                    edge_weight(shared_tags.len()),
                ));
            }

            if !shared_sessions.is_empty() {
                edges.push(task_summary_edge(
                    left,
                    right,
                    EdgeKind::RelatedTo,
                    HashMap::from([
                        ("reason".into(), json!("shared_l1_sessions")),
                        ("shared_l1_sessions".into(), json!(shared_sessions)),
                    ]),
                    edge_weight(shared_sessions.len()),
                ));
            }
        }
    }
    edges
}

fn task_summary_edge(
    left: &TaskSummary,
    right: &TaskSummary,
    kind: EdgeKind,
    props: HashMap<String, serde_json::Value>,
    weight: f64,
) -> Edge {
    let left_id = stable_l2_graph_node_id(&left.task_id);
    let right_id = stable_l2_graph_node_id(&right.task_id);
    let (src, dst) = if left_id <= right_id {
        (left_id, right_id)
    } else {
        (right_id, left_id)
    };
    Edge {
        src,
        dst,
        kind,
        props,
        created_at: chrono::Utc::now().timestamp_millis(),
        weight,
    }
}

fn shared_normalized_values(left: &[String], right: &[String]) -> Vec<String> {
    let right_values = right
        .iter()
        .filter_map(|value| normalize_non_empty(value))
        .collect::<HashSet<_>>();
    let mut shared = left
        .iter()
        .filter_map(|value| normalize_non_empty(value))
        .filter(|value| right_values.contains(value))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    shared.sort();
    shared
}

fn shared_l1_sessions(left: &TaskSummary, right: &TaskSummary) -> Vec<String> {
    let left_sessions = left
        .l1_refs
        .iter()
        .filter_map(|reference| normalize_non_empty(&reference.session))
        .collect::<Vec<_>>();
    let right_sessions = right
        .l1_refs
        .iter()
        .filter_map(|reference| normalize_non_empty(&reference.session))
        .collect::<Vec<_>>();
    shared_normalized_values(&left_sessions, &right_sessions)
}

fn normalize_non_empty(value: &str) -> Option<String> {
    let normalized = value.trim().to_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn edge_weight(shared_count: usize) -> f64 {
    match shared_count {
        0 => 0.5,
        1 => 0.6,
        2 => 0.7,
        3 => 0.8,
        4 => 0.9,
        _ => 1.0,
    }
}

fn stable_l2_graph_node_id(task_id: &str) -> String {
    let sanitized = task_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let stable = sanitized.trim_matches('_');
    let suffix = if stable.is_empty() { "unknown" } else { stable };
    format!("memory_memory_l2_{suffix}")
}

fn has_explicit_memory_recall_intent(query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return false;
    }
    [
        "之前",
        "上次",
        "以前",
        "历史",
        "记得",
        "回忆",
        "当时",
        "刚才",
        "曾经",
        "过去",
        "查记忆",
        "从记忆",
        "历史记录",
        "之前说",
        "上次说",
        "我们之前",
        "我们上次",
        "我们当时",
        "怎么处理过",
        "以前怎么",
        "之前怎么",
    ]
    .iter()
    .any(|needle| query.contains(needle))
}

fn graph_recall_keywords(query: &str) -> Vec<String> {
    let mut keywords = Vec::new();
    let mut current_ascii = String::new();
    let mut chinese = String::new();

    for ch in query.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            current_ascii.push(ch.to_ascii_lowercase());
        } else {
            if current_ascii.len() >= 2 {
                keywords.push(std::mem::take(&mut current_ascii));
            } else {
                current_ascii.clear();
            }
            if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
                chinese.push(ch);
            } else if !ch.is_whitespace() {
                chinese.push(' ');
            }
        }
    }
    if current_ascii.len() >= 2 {
        keywords.push(current_ascii);
    }

    let stop_words = [
        "之前",
        "上次",
        "以前",
        "历史",
        "记得",
        "回忆",
        "当时",
        "刚才",
        "曾经",
        "过去",
        "我们",
        "这个",
        "那个",
        "一下",
        "怎么",
        "什么",
        "有没有",
    ];
    let chars = chinese.chars().collect::<Vec<_>>();
    for width in 2..=4 {
        if chars.len() < width {
            continue;
        }
        for window in chars.windows(width) {
            let token = window.iter().collect::<String>().trim().to_string();
            if token.chars().count() >= 2
                && !token.contains(' ')
                && !stop_words.iter().any(|stop| token == *stop)
            {
                keywords.push(token);
            }
        }
    }

    keywords.sort();
    keywords.dedup();
    keywords.truncate(80);
    keywords
}

fn string_prop(props: &HashMap<String, serde_json::Value>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn resolve_memory_source_refs(
    storage: &PyramidStorage,
    raw_pool: &RawPool,
    refs: &[serde_json::Value],
) -> Vec<String> {
    let mut snippets = Vec::new();
    let mut resolved_legacy_refs = HashSet::new();
    for reference in refs.iter().take(6) {
        if let Some(file_snippets) = resolve_file_line_ref(storage, reference) {
            if let Some(key) = legacy_ref_key(reference) {
                resolved_legacy_refs.insert(key);
            }
            snippets.extend(file_snippets);
            continue;
        }

        let Some(session) = reference.get("session").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if legacy_ref_key(reference).is_some_and(|key| resolved_legacy_refs.contains(&key)) {
            continue;
        }
        let Ok(turns) = raw_pool.read_session(session) else {
            continue;
        };
        let paragraph_indexes = reference
            .get("paragraphs")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .filter_map(|idx| usize::try_from(idx).ok())
                    .collect::<Vec<_>>()
            })
            .filter(|items| !items.is_empty())
            .unwrap_or_else(|| (0..turns.len().min(4)).collect());

        for idx in paragraph_indexes.into_iter().take(4) {
            let Some(turn) = turns.get(idx) else {
                continue;
            };
            let mut content = format!(
                "- session={session}, paragraph={idx}, role={}: {}",
                turn.role,
                truncate_owned(&turn.content, 260)
            );
            if let Some(tool_output) = &turn.tool_output {
                content.push_str(&format!(
                    "\n  tool_output: {}",
                    truncate_owned(tool_output, 220)
                ));
            }
            snippets.push(content);
        }
    }
    snippets
}

fn legacy_ref_key(reference: &serde_json::Value) -> Option<String> {
    let session = reference
        .get("session")
        .and_then(serde_json::Value::as_str)?;
    let paragraphs = reference
        .get("paragraphs")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_u64)
                .map(|idx| idx.to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    Some(format!("{session}:{paragraphs}"))
}

fn resolve_file_line_ref(
    storage: &PyramidStorage,
    reference: &serde_json::Value,
) -> Option<Vec<String>> {
    let file = reference.get("file").and_then(serde_json::Value::as_str)?;
    let line_start = reference
        .get("line_start")
        .and_then(serde_json::Value::as_u64)
        .and_then(|line| usize::try_from(line).ok())?;
    let line_end = reference
        .get("line_end")
        .and_then(serde_json::Value::as_u64)
        .and_then(|line| usize::try_from(line).ok())
        .unwrap_or(line_start);
    if line_start == 0 {
        return None;
    }

    let file_path = {
        let path = PathBuf::from(file);
        if path.is_absolute() {
            path
        } else {
            storage.base_dir().join(path)
        }
    };
    let content = std::fs::read_to_string(&file_path).ok()?;
    let mut snippets = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        if line_no < line_start || line_no > line_end {
            continue;
        }
        snippets.push(format!(
            "- file={}, line={line_no}: {}",
            file,
            format_memory_jsonl_line(line)
        ));
    }
    if snippets.is_empty() {
        None
    } else {
        Some(snippets)
    }
}

fn format_memory_jsonl_line(line: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return truncate_owned(line, 320);
    };
    let role = value
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown");
    let content = value
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(line);
    let mut formatted = format!("role={role}: {}", truncate_owned(content, 260));
    if let Some(tool_output) = value.get("tool_output").and_then(serde_json::Value::as_str) {
        formatted.push_str(&format!(
            "\n  tool_output: {}",
            truncate_owned(tool_output, 220)
        ));
    }
    formatted
}

fn truncate_owned(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect::<String>() + "..."
}

/// 金字塔版记忆脑
pub struct PyramidMemoryBrain {
    config: PyramidMemoryBrainConfig,
    persona_manager: PersonaManager,
    storage: PyramidStorage,
    query_count: AtomicU32,
}

/// 金字塔版记忆脑配置
#[derive(Debug, Clone)]
pub struct PyramidMemoryBrainConfig {
    /// 存储根目录 (~/.ai-brain/)
    pub base_dir: PathBuf,
    /// 当前会话 ID
    pub session_id: String,
    /// 原生知识图谱 SQLite 路径。None 表示禁用自动镜像。
    pub graph_db_path: Option<PathBuf>,
}

impl Default for PyramidMemoryBrainConfig {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".into());
        let base_dir = PathBuf::from(home).join(".ai-brain");
        Self {
            graph_db_path: Some(base_dir.join("graph").join("graph.db")),
            base_dir,
            session_id: format!("sess-{}", chrono::Utc::now().timestamp()),
        }
    }
}

impl PyramidMemoryBrain {
    /// 创建金字塔版记忆脑
    pub fn new(config: PyramidMemoryBrainConfig) -> Result<Self> {
        let persona_manager = PersonaManager::load_or_create(&config.base_dir)?;
        let persona_id = persona_manager.active_id().to_string();
        let storage = PyramidStorage::new(config.base_dir.clone(), persona_id);
        storage.ensure_dirs()?;

        Ok(Self {
            config,
            persona_manager,
            storage,
            query_count: AtomicU32::new(0),
        })
    }

    fn conversation_memory_store(&self) -> ConversationMemoryStore {
        ConversationMemoryStore::new(self.storage.clone(), self.config.session_id.clone())
    }

    pub fn conversation_memory_is_stale(&self) -> bool {
        self.conversation_memory_store()
            .is_derived_stale()
            .unwrap_or(true)
    }

    /// Remove superseded Web generations from active L1 memory and suppress
    /// derived memory until it has been rebuilt from the remaining generations.
    pub fn invalidate_conversation_memory(
        &self,
        invalidation: &ConversationMemoryInvalidation,
    ) -> Result<usize> {
        let moved = self.conversation_memory_store().invalidate(invalidation)?;
        self.supersede_l2_graph_projection()?;
        let interval = self.persona_manager.analysis_interval();
        self.query_count
            .store(interval.saturating_sub(1), Ordering::Relaxed);
        Ok(moved)
    }

    fn supersede_l2_graph_projection(&self) -> Result<()> {
        let Some(db_path) = &self.config.graph_db_path else {
            return Ok(());
        };
        if !db_path.exists() {
            return Ok(());
        }
        let graph = GraphStore::open(db_path).map_err(|error| {
            crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
        })?;
        for task in SummaryPool::new(self.storage.clone()).load_all()? {
            graph
                .mark_superseded(&stable_l2_graph_node_id(&task.task_id))
                .map_err(|error| {
                    crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
                })?;
        }
        Ok(())
    }

    /// 存储对话轮次到 L1
    pub fn store_turn(&self, role: &str, content: &str, tool_output: Option<&str>) -> Result<()> {
        let pool = RawPool::new(self.storage.clone());
        pool.append_turn(&self.config.session_id, role, content, tool_output)
    }

    /// 批量存储对话轮次
    pub fn store_turns_batch(&self, turns: &[(&str, &str, Option<&str>)]) -> Result<()> {
        let pool = RawPool::new(self.storage.clone());
        for (role, content, tool_output) in turns {
            pool.append_turn(&self.config.session_id, role, content, *tool_output)?;
        }
        Ok(())
    }

    /// Store a Web user turn in its own generation-scoped L1 file so a later
    /// edit/retry can invalidate only that branch.
    pub fn store_turns_scoped(
        &self,
        turns: &[TurnRecord],
        scope: &ConversationMemoryScope,
    ) -> Result<()> {
        let session_id = scope.storage_session_id(&self.config.session_id)?;
        self.store_turns_in_session(turns, &session_id)
    }

    /// 检查是否应触发四步浓缩
    pub fn tick_and_should_analyze(&self) -> bool {
        let count = self.query_count.fetch_add(1, Ordering::Relaxed) + 1;
        let interval = self.persona_manager.analysis_interval();
        count % interval == 0
    }

    /// 执行四步浓缩
    pub async fn concentrate(
        &self,
        llm: &dyn crate::concentration::AnalysisLlm,
    ) -> ConcentrationReport {
        let rebuild = match self.conversation_memory_store().rebuild_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return ConcentrationReport {
                    step1_tasks: 0,
                    step2_types: 0,
                    step3_triggers: 0,
                    step3_narrative_chars: 0,
                    step4_profile_updated: false,
                    errors: vec![format!("conversation_memory_snapshot: {error}")],
                };
            }
        };
        if rebuild.legacy_unscoped {
            return ConcentrationReport {
                step1_tasks: 0,
                step2_types: 0,
                step3_triggers: 0,
                step3_narrative_chars: 0,
                step4_profile_updated: false,
                errors: vec![
                    "conversation_memory_rebuild: legacy unscoped memory remains fail-closed"
                        .into(),
                ],
            };
        }
        let engine = ConcentrationEngine::new(self.storage.clone(), self.config.session_id.clone())
            .with_active_l1_rebuild(rebuild.derived_stale);
        let mut report = engine.run(llm).await;
        if report.errors.is_empty() {
            let mut mirrored = 0;
            let committed =
                self.conversation_memory_store()
                    .commit_rebuild(rebuild.revision, || {
                        if report.step1_tasks > 0 {
                            mirrored = self.mirror_l2_to_graph()?;
                        }
                        Ok(())
                    });
            match committed {
                Ok(true) => {
                    if mirrored > 0 {
                        tracing::info!("L2 摘要已镜像到原生图谱: {mirrored} 个节点");
                    }
                }
                Ok(false) => report.errors.push(
                    "conversation_memory_rebuild: invalidation revision changed before commit"
                        .into(),
                ),
                Err(error) => report
                    .errors
                    .push(format!("conversation_memory_rebuild: {error}")),
            }
        }
        report
    }

    /// 将当前 L2 摘要池镜像到原生知识图谱。
    ///
    /// 这是 best-effort 索引层同步：不写入 L1 原文，只写任务摘要、标签和 L1 引用。
    pub fn mirror_l2_to_graph(&self) -> Result<usize> {
        let Some(db_path) = &self.config.graph_db_path else {
            return Ok(0);
        };
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let graph = GraphStore::open(db_path).map_err(|error| {
            crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
        })?;
        let summary_pool = SummaryPool::new(self.storage.clone());
        let tasks = summary_pool.load_all()?;
        for task in &tasks {
            let node = task_summary_to_graph_node(&self.storage, task);
            graph.upsert_node(&node).map_err(|error| {
                crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
            })?;
        }
        for edge in task_summary_graph_edges(&tasks) {
            graph.insert_edge(&edge).map_err(|error| {
                crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
            })?;
        }
        Ok(tasks.len())
    }

    /// 显式记忆意图召回。
    ///
    /// 只有用户输入包含“之前/上次/记得/历史”等明确历史意图时才查图谱，避免普通问题被弱相关记忆污染。
    pub fn recall_graph_memory_context(&self, query: &str, limit: usize) -> Result<Option<String>> {
        if self.conversation_memory_is_stale() {
            return Ok(None);
        }
        if !has_explicit_memory_recall_intent(query) {
            return Ok(None);
        }
        let Some(db_path) = &self.config.graph_db_path else {
            return Ok(None);
        };
        if !db_path.exists() {
            return Ok(None);
        }

        let keywords = graph_recall_keywords(query);
        if keywords.is_empty() {
            return Ok(None);
        }

        let graph = GraphStore::open(db_path).map_err(|error| {
            crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
        })?;
        let mut catalog_query = CatalogQuery::new(keywords);
        catalog_query.graph_type = Some(GraphType::Memory);
        catalog_query.limit = limit.clamp(1, 5);

        let ToolResult::Ok { data } = graph.search_catalog(&catalog_query).map_err(|error| {
            crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
        })?
        else {
            return Ok(None);
        };

        let raw_pool = RawPool::new(self.storage.clone());
        let mut sections = Vec::new();
        for (idx, entry) in data.entries.iter().take(limit).enumerate() {
            let ToolResult::Ok { data: detail } =
                graph.get_node_detail(&entry.node_id).map_err(|error| {
                    crate::error::MemoryError::Io(std::io::Error::other(error.to_string()))
                })?
            else {
                continue;
            };

            let summary = string_prop(&detail.center.props, "summary")
                .unwrap_or_else(|| "无摘要".to_string());
            let snippets =
                resolve_memory_source_refs(&self.storage, &raw_pool, &detail.source_refs);
            let snippet_text = if snippets.is_empty() {
                "未能读取到原始片段；请把该图谱节点只当作候选摘要。".to_string()
            } else {
                snippets.join("\n")
            };
            sections.push(format!(
                "{}. {}\n摘要：{}\n来源片段：\n{}",
                idx + 1,
                entry.title,
                truncate_owned(&summary, 240),
                truncate_owned(&snippet_text, 900),
            ));
        }

        if sections.is_empty() {
            return Ok(None);
        }

        Ok(Some(format!(
            "[历史记忆召回]\n用户输入包含明确历史/记忆意图。以下内容来自长期记忆图谱与原始记忆片段；只在与当前问题确实相关时使用，不要把候选记忆当作必然事实。\n\n{}",
            sections.join("\n\n")
        )))
    }

    /// 自动注入上下文（潜意识 + 画像 + injectable 经验）
    pub fn auto_inject(&self) -> Result<InjectContext> {
        if self.conversation_memory_is_stale() {
            return Ok(InjectContext {
                subconscious_text: String::new(),
                profile: String::new(),
                injectable_experiences: Vec::new(),
            });
        }
        let recall = ProgressiveRecall::new(self.storage.clone());
        recall.auto_inject()
    }

    /// 生成注入文本
    pub fn build_inject_text(&self) -> Result<String> {
        if self.conversation_memory_is_stale() {
            return Ok(String::new());
        }
        let recall = ProgressiveRecall::new(self.storage.clone());
        recall.build_inject_text()
    }

    /// 渐进式召回
    pub fn recall(
        &self,
        query: &str,
        max_depth: crate::pyramid_types::PyramidLayer,
    ) -> Result<Vec<crate::progressive_recall::RecallHit>> {
        if self.conversation_memory_is_stale() {
            return Ok(Vec::new());
        }
        let recall = ProgressiveRecall::new(self.storage.clone());
        recall.recall(query, max_depth)
    }

    /// 获取潜意识叙事
    pub fn load_subconscious_summary(&self) -> Result<String> {
        if self.conversation_memory_is_stale() {
            return Ok(String::new());
        }
        let pool = SubconsciousPool::new(self.storage.clone());
        pool.narrative()
    }

    /// 获取用户画像
    pub fn load_profile(&self) -> Result<String> {
        if self.conversation_memory_is_stale() {
            return Ok(String::new());
        }
        let store = ProfileStore::new(self.storage.clone());
        store.summary()
    }

    /// 获取评估信息注入文本
    pub fn load_eval_inject_text(&self) -> Result<String> {
        if self.conversation_memory_is_stale() {
            return Ok(String::new());
        }
        let store = EvalInfoStore::new(self.storage.clone());
        store.inject_text()
    }

    /// 获取用户画像摘要文本（用于注入评估脑 prompt）
    pub fn load_profile_summary(&self) -> Result<String> {
        if self.conversation_memory_is_stale() {
            return Ok(String::new());
        }
        let store = ProfileStore::new(self.storage.clone());
        store.summary()
    }

    /// 获取活跃踩坑记录（用于评估脑用户要求合规检查）
    ///
    /// 从 EvalInfo 中的 pitfalls 文本转换为 PitfallRecord 格式。
    pub fn load_active_pitfalls(&self) -> Vec<brain_core::types::PitfallRecord> {
        if self.conversation_memory_is_stale() {
            return Vec::new();
        }
        let store = EvalInfoStore::new(self.storage.clone());
        let Some(info) = store.load().ok().flatten() else {
            return Vec::new();
        };

        let now = chrono::Utc::now();
        info.pitfalls
            .into_iter()
            .enumerate()
            .map(|(i, desc)| brain_core::types::PitfallRecord {
                id: format!("pitfall-{}", i),
                category: brain_core::types::PitfallCategory::Other,
                description: desc,
                user_correction: None,
                occurred_at: now,
                occurrence_count: 1,
                superseded: false,
            })
            .collect()
    }

    /// 获取评估信息（转换为 EvalRequirement 格式供评估脑使用）
    ///
    /// 将 eval-info.json 中的 requirements + pitfalls + rules 合并为
    /// 评估脑可消费的 Vec<EvalRequirement>。
    pub fn load_eval_requirements(&self) -> Vec<brain_core::types::EvalRequirement> {
        if self.conversation_memory_is_stale() {
            return Vec::new();
        }
        let store = EvalInfoStore::new(self.storage.clone());
        let Some(info) = store.load().ok().flatten() else {
            return Vec::new();
        };

        let mut reqs = Vec::new();
        let now = chrono::Utc::now();

        for req in &info.requirements {
            reqs.push(brain_core::types::EvalRequirement {
                id: format!("eval-req-{}", reqs.len()),
                content: req.clone(),
                source: "记忆脑分析".into(),
                created_at: now,
                superseded: false,
            });
        }
        for pitfall in &info.pitfalls {
            reqs.push(brain_core::types::EvalRequirement {
                id: format!("eval-pitfall-{}", reqs.len()),
                content: format!("[已知踩坑] {pitfall}"),
                source: "记忆脑分析".into(),
                created_at: now,
                superseded: false,
            });
        }
        for rule in &info.rules {
            reqs.push(brain_core::types::EvalRequirement {
                id: format!("eval-rule-{}", reqs.len()),
                content: format!("[进化规则] {rule}"),
                source: "记忆脑分析".into(),
                created_at: now,
                superseded: false,
            });
        }

        reqs
    }

    /// 切换人格
    pub fn switch_persona(&mut self, persona_id: &str) -> Result<()> {
        self.persona_manager.switch(persona_id)?;
        self.storage = PyramidStorage::new(self.config.base_dir.clone(), persona_id);
        self.storage.ensure_dirs()?;
        self.query_count.store(0, Ordering::Relaxed);
        Ok(())
    }

    /// 获取当前人格信息
    pub fn active_persona(&self) -> &crate::persona_types::Persona {
        self.persona_manager.active()
    }

    /// 获取人格管理器
    pub fn persona_manager(&self) -> &PersonaManager {
        &self.persona_manager
    }

    /// 获取人格管理器（可变）
    pub fn persona_manager_mut(&mut self) -> &mut PersonaManager {
        &mut self.persona_manager
    }

    /// 获取存储层
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }

    /// 获取配置
    pub fn config(&self) -> &PyramidMemoryBrainConfig {
        &self.config
    }

    /// 获取 BrainId
    pub fn brain_id(&self) -> BrainId {
        BrainId::memory()
    }

    /// 记忆统计
    pub fn stats(&self) -> Result<PyramidMemoryStats> {
        let raw = RawPool::new(self.storage.clone());
        let summary_pool = crate::summary_pool::SummaryPool::new(self.storage.clone());
        let abstract_layer = AbstractLayer::new(self.storage.clone());
        let subconscious = SubconsciousPool::new(self.storage.clone());

        Ok(PyramidMemoryStats {
            l1_count: raw.count().unwrap_or(0),
            l2_count: summary_pool.load_all().unwrap_or_default().len() as u32,
            l3_count: abstract_layer.load_all().unwrap_or_default().len() as u32,
            l4_exists: subconscious.load()?.is_some(),
            active_persona: self.persona_manager.active_id().to_string(),
            session_id: self.config.session_id.clone(),
        })
    }

    // ─── MemoryBrain 兼容适配方法 ─────────────────────────────────

    /// 存储完整对话轨迹（兼容旧 MemoryBrain 接口）
    ///
    /// 将 TurnRecord 逐条写入 L1，不再写 L2 短期记忆（由浓缩引擎统一处理）。
    pub fn store_turns(&self, turns: &[TurnRecord]) -> Result<()> {
        self.store_turns_in_session(turns, &self.config.session_id)
    }

    fn store_turns_in_session(&self, turns: &[TurnRecord], session_id: &str) -> Result<()> {
        let pool = RawPool::new(self.storage.clone());
        for turn in turns {
            let role = format!("{:?}", turn.role).to_lowercase();
            let tool_output = turn.tool_call.as_ref().map(|tc| {
                format!(
                    "{}: {}",
                    tc.tool_name,
                    tc.output.chars().take(200).collect::<String>()
                )
            });
            pool.append_turn(session_id, &role, &turn.content, tool_output.as_deref())?;
        }
        tracing::info!("L1 完整轨迹追加 {} 条", turns.len());
        Ok(())
    }

    /// 读取最近的对话记录（兼容旧 MemoryBrain 接口）
    ///
    /// 从 L1 raw pool 读取最近 max_entries 条记录，返回 JSON 字符串。
    pub fn read_recent_conversations(&self, max_entries: usize) -> Vec<String> {
        let pool = RawPool::new(self.storage.clone());
        let mut turns = Vec::new();
        if let Ok(sessions) = pool.list_sessions() {
            for session in sessions {
                if let Ok(session_turns) = pool.read_session(&session) {
                    turns.extend(session_turns);
                }
            }
        }
        turns.sort_by_key(|turn| turn.timestamp);
        turns
            .into_iter()
            .rev()
            .take(max_entries)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|turn| {
                serde_json::json!({
                    "role": turn.role,
                    "content": turn.content,
                    "timestamp": turn.timestamp.to_rfc3339()
                })
                .to_string()
            })
            .collect()
    }

    /// 记忆召回（兼容旧 MemoryBrain 接口）
    ///
    /// 将金字塔召回结果转换为 MemoryEntry 格式。
    pub fn recall_for_context(&self, query: &str, _max: usize) -> Vec<MemoryEntry> {
        if self.conversation_memory_is_stale() {
            return Vec::new();
        }
        let recall = ProgressiveRecall::new(self.storage.clone());
        match recall.recall(query, crate::pyramid_types::PyramidLayer::Summary) {
            Ok(hits) => hits
                .into_iter()
                .enumerate()
                .map(|(i, hit)| MemoryEntry {
                    id: format!("pyramid-{}", i),
                    content: hit.content,
                    tags: vec![format!("{:?}", hit.layer).to_lowercase()],
                    layer: MemoryLayer::Raw,
                    importance: 0.7,
                    source: KnowledgeSource::Memory {
                        memory_id: hit.source,
                        layer: MemoryLayer::Raw,
                    },
                    confidence: 0.8,
                    reference_count: 0,
                    created_at: chrono::Utc::now(),
                    last_accessed: chrono::Utc::now(),
                    consolidated: false,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 搜索记忆（兼容旧 MemoryBrain 接口）
    pub fn search(&self, query: &str, max_results: usize) -> Vec<MemoryEntry> {
        self.recall_for_context(query, max_results)
    }

    /// 列出最近的会话摘要（兼容旧 MemoryBrain 接口）
    pub fn list_recent_summaries(&self, limit: usize) -> Vec<RecentSummary> {
        if self.conversation_memory_is_stale() {
            return Vec::new();
        }
        let pool = SummaryPool::new(self.storage.clone());
        let summaries = pool.load_all().unwrap_or_default();

        summaries
            .into_iter()
            .rev()
            .take(limit)
            .enumerate()
            .map(|(i, s)| RecentSummary {
                file_path: self
                    .storage
                    .l2_dir()
                    .join(format!("{}.json", s.task_id))
                    .to_string_lossy()
                    .to_string(),
                session_id: s.task_id,
                session_start: s.created_at.to_rfc3339(),
                session_end: s.updated_at.to_rfc3339(),
                tags: s.tags,
                summary_preview: s.summary.chars().take(100).collect(),
                sort_index: i,
            })
            .collect()
    }

    /// 获取存储根目录
    pub fn base_dir(&self) -> &Path {
        self.config.base_dir.as_path()
    }

    /// 获取当前会话 ID
    pub fn session_id(&self) -> &str {
        &self.config.session_id
    }

    /// 加载潜意识摘要（兼容旧接口返回 Option）
    pub fn load_subconscious_summary_opt(&self) -> Option<String> {
        self.load_subconscious_summary()
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// 获取记忆统计（兼容旧 MemoryStats 格式）
    pub fn stats_legacy(&self) -> Result<brain_core::types::MemoryStats> {
        let stats = self.stats()?;
        Ok(brain_core::types::MemoryStats {
            l0_count: stats.l1_count,
            l1_count: stats.l2_count,
            l2_count: 0, // 金字塔模式下无短期记忆
            l3_count: stats.l3_count,
            total_size_bytes: 0,
        })
    }
}

/// L2 会话摘要简要信息（兼容旧 RecentSummary 接口）
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecentSummary {
    pub file_path: String,
    pub session_id: String,
    pub session_start: String,
    pub session_end: String,
    pub tags: Vec<String>,
    pub summary_preview: String,
    pub sort_index: usize,
}

/// 金字塔记忆统计
#[derive(Debug, Clone, serde::Serialize)]
pub struct PyramidMemoryStats {
    pub l1_count: u32,
    pub l2_count: u32,
    pub l3_count: u32,
    pub l4_exists: bool,
    pub active_persona: String,
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona_types::PersonaConfig;
    use crate::pyramid_types::{L1Ref, TaskSummary, TaskType};
    use brain_graph::error::ToolResult;
    use brain_graph::schema::{EdgeKind, TraceDirection};
    use brain_graph::store::{CatalogQuery, TraceQuery};

    struct RejectingAnalysisLlm;

    impl crate::concentration::AnalysisLlm for RejectingAnalysisLlm {
        fn analyze_structured(
            &self,
            _system: &str,
            _user: &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send + '_>,
        > {
            Box::pin(async { Err("analysis must not run".into()) })
        }
    }

    fn make_brain(tmp: &tempfile::TempDir) -> PyramidMemoryBrain {
        let base_dir = tmp.path().to_path_buf();
        let config = PyramidMemoryBrainConfig {
            graph_db_path: Some(base_dir.join("graph").join("graph.db")),
            base_dir,
            session_id: "sess-test".into(),
        };
        PyramidMemoryBrain::new(config).unwrap()
    }

    #[test]
    fn new_creates_default_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        assert_eq!(brain.active_persona().id, "default");
        assert_eq!(brain.active_persona().name, "智脑");
    }

    #[test]
    fn store_and_recall_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        brain.store_turn("User", "你好", None).unwrap();
        brain.store_turn("Assistant", "你好！", None).unwrap();

        let pool = RawPool::new(brain.storage().clone());
        let turns = pool.read_session("sess-test").unwrap();
        assert_eq!(turns.len(), 2);
    }

    #[test]
    fn store_batch_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        brain
            .store_turns_batch(&[("User", "hello", None), ("Tool", "ls", Some("file1"))])
            .unwrap();

        let pool = RawPool::new(brain.storage().clone());
        let turns = pool.read_session("sess-test").unwrap();
        assert_eq!(turns.len(), 2);
    }

    #[test]
    fn scoped_web_turns_are_invalidated_without_touching_other_generations() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        let old_scope = ConversationMemoryScope::new("chat_1", "generation_old").unwrap();
        let kept_scope = ConversationMemoryScope::new("chat_1", "generation_kept").unwrap();
        let turn = TurnRecord {
            role: brain_core::types::TurnRole::User,
            content: "分支内容".into(),
            tool_call: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        brain
            .store_turns_scoped(std::slice::from_ref(&turn), &old_scope)
            .unwrap();
        brain.store_turns_scoped(&[turn], &kept_scope).unwrap();
        ProfileStore::new(brain.storage().clone())
            .regenerate("可能包含旧分支的画像")
            .unwrap();

        let moved = brain
            .invalidate_conversation_memory(&ConversationMemoryInvalidation {
                conversation_id: "chat_1".into(),
                generation_ids: vec!["generation_old".into()],
                includes_legacy_unscoped: false,
            })
            .unwrap();

        assert_eq!(moved, 1);
        let old_session = old_scope.storage_session_id("sess-test").unwrap();
        let kept_session = kept_scope.storage_session_id("sess-test").unwrap();
        assert!(!brain.storage().l1_session_path(&old_session).exists());
        assert!(brain.storage().l1_session_path(&kept_session).exists());
        assert!(brain.conversation_memory_is_stale());
        assert!(brain.load_profile().unwrap().is_empty());
        assert!(brain.recall_for_context("分支内容", 3).is_empty());
    }

    #[tokio::test]
    async fn legacy_unscoped_invalidation_never_runs_a_contaminated_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        brain
            .store_turn("User", "legacy deleted branch", None)
            .unwrap();
        brain
            .invalidate_conversation_memory(&ConversationMemoryInvalidation {
                conversation_id: "legacy_chat".into(),
                generation_ids: Vec::new(),
                includes_legacy_unscoped: true,
            })
            .unwrap();

        let report = brain.concentrate(&RejectingAnalysisLlm).await;

        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("legacy unscoped"));
        assert!(brain.conversation_memory_is_stale());
    }

    #[test]
    fn mirror_l2_to_graph_writes_catalog_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        let now = chrono::Utc::now();
        let task = TaskSummary {
            task_id: "task-001".into(),
            task_type: TaskType::Coding,
            task_name: "红冲资费生成逻辑".into(),
            summary: "解释红冲资费生成的关键上下文".into(),
            l1_refs: vec![L1Ref {
                session: "sess-test".into(),
                paragraphs: vec![0, 1],
            }],
            tags: vec!["红冲".into(), "资费".into()],
            importance: 0.9,
            created_at: now,
            updated_at: now,
        };

        SummaryPool::new(brain.storage().clone())
            .regenerate(vec![task])
            .unwrap();

        let mirrored = brain.mirror_l2_to_graph().unwrap();
        assert_eq!(mirrored, 1);

        let db_path = brain.config().graph_db_path.as_ref().unwrap();
        let graph = GraphStore::open(db_path).unwrap();
        let result = graph
            .search_catalog(&CatalogQuery::new(vec!["红冲".into()]))
            .unwrap();

        let ToolResult::Ok { data } = result else {
            panic!("expected graph search to succeed");
        };
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].title, "红冲资费生成逻辑");
        assert_eq!(data.entries[0].node_id, "memory_memory_l2_task-001");

        let ToolResult::Ok { data: detail } =
            graph.get_node_detail("memory_memory_l2_task-001").unwrap()
        else {
            panic!("expected graph detail to succeed");
        };
        assert!(detail
            .source_refs
            .iter()
            .any(|source_ref| source_ref["file"]
                == "personas/default/pyramid/l1-raw/sess-test.jsonl"
                && source_ref["line_start"] == 1
                && source_ref["line_end"] == 2));
    }

    #[test]
    fn mirror_l2_to_graph_links_related_summaries() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        let now = chrono::Utc::now();
        let first = TaskSummary {
            task_id: "task-a".into(),
            task_type: TaskType::Coding,
            task_name: "图谱目录检索".into(),
            summary: "实现图谱目录检索能力".into(),
            l1_refs: vec![L1Ref {
                session: "sess-shared".into(),
                paragraphs: vec![0],
            }],
            tags: vec!["图谱".into(), "检索".into()],
            importance: 0.8,
            created_at: now,
            updated_at: now,
        };
        let second = TaskSummary {
            task_id: "task-b".into(),
            task_type: TaskType::Coding,
            task_name: "图谱追踪查询".into(),
            summary: "实现图谱追踪能力".into(),
            l1_refs: vec![L1Ref {
                session: "sess-shared".into(),
                paragraphs: vec![1],
            }],
            tags: vec!["图谱".into(), "追踪".into()],
            importance: 0.7,
            created_at: now,
            updated_at: now,
        };

        SummaryPool::new(brain.storage().clone())
            .regenerate(vec![first, second])
            .unwrap();

        let mirrored = brain.mirror_l2_to_graph().unwrap();
        assert_eq!(mirrored, 2);

        let db_path = brain.config().graph_db_path.as_ref().unwrap();
        let graph = GraphStore::open(db_path).unwrap();
        let mut query = TraceQuery::new("memory_memory_l2_task-a");
        query.direction = TraceDirection::Both;
        let result = graph.trace_memory(&query).unwrap();

        let ToolResult::Ok { data } = result else {
            panic!("expected graph trace to succeed");
        };
        assert!(data
            .steps
            .iter()
            .any(|step| step.to_node_id == "memory_memory_l2_task-b"
                && step.edge_kind == EdgeKind::SimilarTo));
        assert!(data
            .steps
            .iter()
            .any(|step| step.to_node_id == "memory_memory_l2_task-b"
                && step.edge_kind == EdgeKind::RelatedTo));
    }

    #[test]
    fn graph_memory_recall_requires_explicit_intent_and_reads_l1_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        brain
            .store_turn(
                "User",
                "之前我们讨论过红冲资费生成逻辑，入口在 billing 模块。",
                None,
            )
            .unwrap();
        brain
            .store_turn(
                "Assistant",
                "当时结论是先查订单，再生成红冲资费明细。",
                None,
            )
            .unwrap();

        let now = chrono::Utc::now();
        let task = TaskSummary {
            task_id: "task-recall".into(),
            task_type: TaskType::Coding,
            task_name: "红冲资费生成逻辑".into(),
            summary: "记录红冲资费生成逻辑的历史讨论。".into(),
            l1_refs: vec![L1Ref {
                session: "sess-test".into(),
                paragraphs: vec![0, 1],
            }],
            tags: vec!["红冲".into(), "资费".into()],
            importance: 0.9,
            created_at: now,
            updated_at: now,
        };
        SummaryPool::new(brain.storage().clone())
            .regenerate(vec![task])
            .unwrap();
        brain.mirror_l2_to_graph().unwrap();

        let no_intent = brain
            .recall_graph_memory_context("红冲资费生成逻辑怎么做", 3)
            .unwrap();
        assert!(no_intent.is_none());

        let recalled = brain
            .recall_graph_memory_context("之前红冲资费生成逻辑怎么处理的", 3)
            .unwrap()
            .expect("explicit memory query should recall context");
        assert!(recalled.contains("[历史记忆召回]"));
        assert!(recalled.contains("红冲资费生成逻辑"));
        assert!(recalled.contains("l1-raw/sess-test.jsonl"));
        assert!(recalled.contains("line=1"));
        assert!(recalled.contains("先查订单"));
    }

    #[test]
    fn tick_and_should_analyze() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        // default interval = 5
        assert!(!brain.tick_and_should_analyze()); // 1
        assert!(!brain.tick_and_should_analyze()); // 2
        assert!(!brain.tick_and_should_analyze()); // 3
        assert!(!brain.tick_and_should_analyze()); // 4
        assert!(brain.tick_and_should_analyze()); // 5
    }

    #[test]
    fn auto_inject_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        let ctx = brain.auto_inject().unwrap();
        assert!(ctx.subconscious_text.is_empty());
        assert!(ctx.profile.is_empty());
    }

    #[test]
    fn switch_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let mut brain = make_brain(&tmp);

        // 创建新人格
        brain
            .persona_manager_mut()
            .create(
                "writer".into(),
                "作家".into(),
                "网文".into(),
                "你是网文助手".into(),
                PersonaConfig::default(),
            )
            .unwrap();

        brain.switch_persona("writer").unwrap();
        assert_eq!(brain.active_persona().id, "writer");
    }

    #[test]
    fn switch_persona_resets_query_count() {
        let tmp = tempfile::tempdir().unwrap();
        let mut brain = make_brain(&tmp);

        brain
            .persona_manager_mut()
            .create(
                "writer".into(),
                "作家".into(),
                "网文".into(),
                "".into(),
                PersonaConfig::default(),
            )
            .unwrap();

        // 触发一些 tick
        brain.tick_and_should_analyze();
        brain.tick_and_should_analyze();

        brain.switch_persona("writer").unwrap();

        // 切换后 query_count 重置
        assert!(!brain.tick_and_should_analyze()); // 1 again
    }

    #[test]
    fn stats_report() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        brain.store_turn("User", "hello", None).unwrap();

        let stats = brain.stats().unwrap();
        assert_eq!(stats.l1_count, 1);
        assert_eq!(stats.l2_count, 0);
        assert!(!stats.l4_exists);
        assert_eq!(stats.active_persona, "default");
    }

    #[test]
    fn load_subconscious_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        let narrative = brain.load_subconscious_summary().unwrap();
        assert!(narrative.is_empty());
    }

    #[test]
    fn build_inject_text_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        let text = brain.build_inject_text().unwrap();
        assert!(text.is_empty());
    }

    #[test]
    fn load_eval_requirements_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);
        let reqs = brain.load_eval_requirements();
        assert!(reqs.is_empty());
    }

    #[test]
    fn load_eval_requirements_with_data() {
        let tmp = tempfile::tempdir().unwrap();
        let brain = make_brain(&tmp);

        // 写入评估信息
        let store = crate::profile_eval::EvalInfoStore::new(brain.storage().clone());
        store
            .regenerate(
                vec!["必须先有计划再动手".into()],
                vec!["不要自作主张偏离设计".into()],
                vec!["严格按设计文档执行".into()],
            )
            .unwrap();

        let reqs = brain.load_eval_requirements();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].content, "必须先有计划再动手");
        assert_eq!(reqs[0].source, "记忆脑分析");
        assert!(!reqs[0].superseded);
        assert!(reqs[1].content.contains("[已知踩坑]"));
        assert!(reqs[2].content.contains("[进化规则]"));
    }
}
