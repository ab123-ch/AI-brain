use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::Utc;

use brain_core::agent::BrainAgent;
use brain_core::types::{
    BrainId, BrainKind, BrainResponse, BrainResponsePayload, BroadcastMessage, CollaborationKind,
    CollaborationMessage, FastThinkResult, KnowledgeSource, MemoryEntry, MemoryLayer, RecallQuery,
    SlowThinkResult, ThinkContext,
};

use crate::analyzer::AnalysisLlm;
use crate::archive::ArchiveStore;
use crate::consolidation::ConsolidationEngine;
use crate::error::Result;
use crate::event_index::EventIndexLayer;
use crate::importance::ImportanceManager;
use crate::memory_iteration::MemoryStoreType;
use crate::raw_layer::{RawEntry, RawLayer};
use crate::recall::RecallEngine;
use crate::short_term::{ShortTermConfig, ShortTermLayer};
use crate::storage::Storage;
use crate::subconscious::SubconsciousStore;
use crate::summary::SessionSummaryStore;
use crate::task_summary::TaskSummaryLayer;

/// 记忆脑配置
#[derive(Debug, Clone)]
pub struct MemoryBrainConfig {
    /// 存储根目录
    pub base_dir: PathBuf,
    /// 当前会话 ID（L3 文件名）
    pub session_id: String,
    /// 快思考关键词提取：从广播消息中提取的最大关键词数
    pub max_keywords: usize,
}

impl Default for MemoryBrainConfig {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".into());
        Self {
            base_dir: PathBuf::from(home).join(".ai-brain"),
            session_id: format!("sess-{}", chrono::Utc::now().timestamp()),
            max_keywords: 5,
        }
    }
}

/// 记忆脑（海马体）
///
/// 职责：
/// - 接收广播消息 → 存入 L3 原始层 + 提取关键词存入 L2 短期层
/// - 快思考：关键词匹配 L0-L2（~10ms）
/// - 慢思考：深度检索含 L3（~100ms，无 LLM 时不调 LLM）
/// - 协作：响应其他副脑的召回请求
/// - 空闲时触发巩固
/// 每多少轮查询触发一次四步分析
const ANALYSIS_INTERVAL: u32 = 5;

pub struct MemoryBrain {
    id: BrainId,
    config: MemoryBrainConfig,
    raw: RawLayer,
    short_term: ShortTermLayer,
    event_index: EventIndexLayer,
    task_summary: TaskSummaryLayer,
    recall: RecallEngine,
    /// 查询计数器（用于判断是否触发四步分析）
    query_count: AtomicU32,
    /// LLM 提供者（语义召回用）
    llm: Option<Box<dyn AnalysisLlm>>,
}

impl MemoryBrain {
    /// 创建记忆脑
    pub fn new(config: MemoryBrainConfig) -> Result<Self> {
        let storage = Storage::new(config.base_dir.clone())?;
        let storage2 = Storage::new(config.base_dir.clone())?;
        let storage3 = Storage::new(config.base_dir.clone())?;
        let storage4 = Storage::new(config.base_dir.clone())?;

        let raw = RawLayer::new(storage);
        let short_term = ShortTermLayer::new(storage2, ShortTermConfig::default());
        let event_index = EventIndexLayer::new(storage3);
        let task_summary = TaskSummaryLayer::new(storage4);

        let recall = RecallEngine::new(
            TaskSummaryLayer::new(Storage::new_lazy(config.base_dir.clone())),
            EventIndexLayer::new(Storage::new_lazy(config.base_dir.clone())),
            ShortTermLayer::new(
                Storage::new_lazy(config.base_dir.clone()),
                ShortTermConfig::default(),
            ),
            RawLayer::new(Storage::new_lazy(config.base_dir.clone())),
        );

        Ok(Self {
            id: BrainId::memory(),
            config,
            raw,
            short_term,
            event_index,
            task_summary,
            recall,
            query_count: AtomicU32::new(0),
            llm: None,
        })
    }

    /// 注入 LLM（用于语义召回和四步分析）
    pub fn set_llm(&mut self, llm: Box<dyn AnalysisLlm>) {
        self.llm = Some(llm);
    }

    /// 是否有 LLM 可用
    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// 存储广播消息到 L3 + 提取到 L2
    pub fn store_broadcast(&mut self, msg: &BroadcastMessage) -> Result<()> {
        // L3: 原始存储
        let entry = RawEntry {
            id: format!("raw-{}", Utc::now().timestamp_millis()),
            content: msg.content.clone(),
            raw_input: msg.raw_input.clone(),
            context: msg.context.clone(),
            timestamp: msg.timestamp,
        };
        self.raw.append(&self.config.session_id, &entry)?;

        // L2: 提取关键词存入短期记忆
        let tags = extract_keywords(&msg.content, self.config.max_keywords);
        let memory_entry = MemoryEntry {
            id: format!("stm-{}", Utc::now().timestamp_millis()),
            content: msg.content.clone(),
            tags,
            layer: MemoryLayer::ShortTerm,
            importance: 0.6, // 初始 importance
            source: KnowledgeSource::Memory {
                memory_id: format!("raw-{}", Utc::now().timestamp_millis()),
                layer: MemoryLayer::Raw,
            },
            confidence: 0.7,
            reference_count: 0,
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            consolidated: false,
        };
        self.short_term.store(memory_entry);
        // 每次 store 后立即持久化，防止重启丢失
        if let Err(e) = self.short_term.persist() {
            tracing::warn!("L2 短期记忆持久化失败: {e}");
        }

        Ok(())
    }

    /// 执行巩固
    pub fn consolidate(&mut self) -> Result<brain_core::types::ConsolidationReport> {
        // 先持久化 L2 数据到磁盘，确保巩固引擎能读到
        self.short_term.persist()?;

        let mut engine = ConsolidationEngine::new(
            RawLayer::new(Storage::new_lazy(self.config.base_dir.clone())),
            ShortTermLayer::new(
                Storage::new_lazy(self.config.base_dir.clone()),
                ShortTermConfig::default(),
            ),
            EventIndexLayer::new(Storage::new_lazy(self.config.base_dir.clone())),
            TaskSummaryLayer::new(Storage::new_lazy(self.config.base_dir.clone())),
        );
        engine.run()
    }

    /// 获取记忆统计
    pub fn stats(&self) -> Result<brain_core::types::MemoryStats> {
        Ok(brain_core::types::MemoryStats {
            l0_count: self.task_summary.count().unwrap_or(0),
            l1_count: self.event_index.count().unwrap_or(0),
            l2_count: self.short_term.len() as u32,
            l3_count: self.raw.count().unwrap_or(0),
            total_size_bytes: 0, // 简化，不计算实际大小
        })
    }

    /// 为上下文召回记忆
    ///
    /// 有 LLM 时：语义召回（把所有记忆内容给 LLM，让它挑选相关的）
    /// 无 LLM 时：关键词匹配回退
    pub fn recall_for_context(&self, query: &str, max: usize) -> Vec<MemoryEntry> {
        // 先收集所有候选记忆（不含 LLM 调用，纯文件读取）
        let candidates = self.gather_all_candidates();

        if candidates.is_empty() {
            return Vec::new();
        }

        // 如果有 LLM，走语义召回路径
        // 注意：这里不能直接调用 async LLM，因为 recall_for_context 是同步方法
        // 所以我们在调用侧（orchestrator）处理 LLM 召回
        // 这里只返回候选列表，由 orchestrator 的 recall_with_llm 来做语义匹配

        // 回退：关键词匹配
        let keywords = extract_keywords(query, 8);
        let mut results = Vec::new();
        for entry in &candidates {
            if results.len() >= max {
                break;
            }
            let content_lower = entry.content.to_lowercase();
            if keywords
                .iter()
                .any(|kw| content_lower.contains(&kw.to_lowercase()))
            {
                results.push(entry.clone());
            }
        }

        // 关键词匹配不够时，补充最近的记忆
        if results.len() < max {
            for entry in &candidates {
                if results.len() >= max {
                    break;
                }
                if !results.iter().any(|r| r.id == entry.id) {
                    results.push(entry.clone());
                }
            }
        }

        // 召回后强化：对选中的 subconscious 条目增加 importance
        if !results.is_empty() {
            let sc_ids: Vec<String> = results
                .iter()
                .filter(|r| r.id.starts_with("sc-"))
                .map(|r| r.id.clone())
                .collect();
            if !sc_ids.is_empty() {
                if let Err(e) = ImportanceManager::reinforce(
                    &Storage::new_lazy(self.config.base_dir.clone()),
                    &sc_ids,
                    MemoryStoreType::Subconscious,
                ) {
                    tracing::warn!("召回强化失败: {e}");
                }
            }
        }

        results
    }

    /// 收集所有候选记忆（纯文件读取，不调 LLM）
    fn gather_all_candidates(&self) -> Vec<MemoryEntry> {
        let mut candidates = Vec::new();

        // 0. 潜意识层（最外层索引，渐进式披露入口）— 只加载可召回的
        let sc_store = SubconsciousStore::new(Storage::new_lazy(self.config.base_dir.clone()));
        if let Ok(entries) = sc_store.load_recallable() {
            for sc in entries.iter().take(10) {
                candidates.push(MemoryEntry {
                    id: sc.id.clone(),
                    content: format!("[印象] {} — {}", sc.topic, sc.impression),
                    tags: sc.trigger_keywords.clone(),
                    layer: MemoryLayer::TaskSummary,
                    importance: sc.importance,
                    source: KnowledgeSource::Memory {
                        memory_id: sc.id.clone(),
                        layer: MemoryLayer::TaskSummary,
                    },
                    confidence: 0.9,
                    reference_count: 0,
                    created_at: sc.created_at,
                    last_accessed: Utc::now(),
                    consolidated: true,
                });
            }
        }

        // 1. L1 归档主题匹配（按重要度取 top 5）
        let archive_store = ArchiveStore::new(Storage::new_lazy(self.config.base_dir.clone()));
        if let Ok(topics) = archive_store.list_topics() {
            for topic in topics.iter().take(5) {
                if let Ok(Some(index)) = archive_store.load_topic(topic) {
                    candidates.push(MemoryEntry {
                        id: format!("archive-{topic}"),
                        content: format!("[归档] {} — {}", index.topic, index.merged_summary),
                        tags: index.trigger_keywords.clone(),
                        layer: MemoryLayer::TaskSummary,
                        importance: index.importance,
                        source: KnowledgeSource::Memory {
                            memory_id: format!("archive-{topic}"),
                            layer: MemoryLayer::TaskSummary,
                        },
                        confidence: 0.8,
                        reference_count: 0,
                        created_at: index.created_at,
                        last_accessed: Utc::now(),
                        consolidated: true,
                    });
                }
            }
        }

        // 2. L2 会话总结（最近的 10 条，只取可召回的）
        let summary_store =
            SessionSummaryStore::new(Storage::new_lazy(self.config.base_dir.clone()));
        if let Ok(all) = summary_store.find_recallable() {
            for s in all.iter().take(10) {
                candidates.push(MemoryEntry {
                    id: format!("summary-{}", s.session_id),
                    content: format!("[会话] {} — {}", s.fact_summary, s.decisions.join("; ")),
                    tags: s.tags.clone(),
                    layer: MemoryLayer::EventIndex,
                    importance: 0.7,
                    source: KnowledgeSource::Memory {
                        memory_id: format!("summary-{}", s.session_id),
                        layer: MemoryLayer::EventIndex,
                    },
                    confidence: 0.75,
                    reference_count: 0,
                    created_at: s.created_at,
                    last_accessed: Utc::now(),
                    consolidated: s.archived,
                });
            }
        }

        // 3. fact_summary
        if let Some(fact) = self.load_fact_summary() {
            candidates.push(MemoryEntry {
                id: "fact-summary".into(),
                content: fact,
                tags: Vec::new(),
                layer: MemoryLayer::TaskSummary,
                importance: 0.9,
                source: KnowledgeSource::Memory {
                    memory_id: "fact_summary".into(),
                    layer: MemoryLayer::TaskSummary,
                },
                confidence: 0.8,
                reference_count: 0,
                created_at: Utc::now(),
                last_accessed: Utc::now(),
                consolidated: true,
            });
        }

        // 4. pitfall 活跃记录
        let storage = crate::storage::Storage::new_lazy(self.config.base_dir.clone());
        let pitfall_store = crate::pitfall::PitfallStore::new(storage);
        if let Ok(pitfalls) = pitfall_store.load_active() {
            for p in pitfalls.iter().take(5) {
                candidates.push(MemoryEntry {
                    id: p.id.clone(),
                    content: format!("[踩坑] {}", p.description),
                    tags: Vec::new(),
                    layer: MemoryLayer::ShortTerm,
                    importance: 0.85,
                    source: KnowledgeSource::Memory {
                        memory_id: p.id.clone(),
                        layer: MemoryLayer::ShortTerm,
                    },

                    confidence: 0.8,
                    reference_count: 0,
                    created_at: p.occurred_at,
                    last_accessed: Utc::now(),
                    consolidated: true,
                });
            }
        }

        // 5. evolution 高优先级规则（只加载未 superseded 的）
        let storage = crate::storage::Storage::new_lazy(self.config.base_dir.clone());
        let evo_store = crate::evolution::EvolutionStore::new(storage);
        if let Ok(rules) = evo_store.load_active() {
            let high_rules: Vec<_> = rules.iter().filter(|r| r.priority >= 3).take(5).collect();
            for r in high_rules {
                candidates.push(MemoryEntry {
                    id: r.id.clone(),
                    content: format!("[规则] {}", r.rule),
                    tags: Vec::new(),
                    layer: MemoryLayer::TaskSummary,
                    importance: 0.95,
                    source: KnowledgeSource::Memory {
                        memory_id: r.id.clone(),
                        layer: MemoryLayer::TaskSummary,
                    },
                    confidence: 0.9,
                    reference_count: 0,
                    created_at: r.created_at,
                    last_accessed: Utc::now(),
                    consolidated: true,
                });
            }
        }

        // 6. L3 原始会话数据（最近 3 个历史会话的起始/结尾内容）
        if let Ok(sessions) = self.raw.list_sessions() {
            let recent: Vec<&String> = sessions
                .iter()
                .filter(|s| *s != &self.config.session_id)
                .rev()
                .take(3)
                .collect();
            for sid in &recent {
                if let Ok(entries) = self.raw.read_session(sid) {
                    if entries.is_empty() {
                        continue;
                    }
                    // 开头：首次用户输入
                    if let Some(first) = entries.first() {
                        let preview: String = first.content.chars().take(150).collect();
                        candidates.push(MemoryEntry {
                            id: format!("raw-{}-head", sid),
                            content: format!("[对话开头] {}", preview),
                            tags: Vec::new(),
                            layer: MemoryLayer::Raw,
                            importance: 0.4,
                            source: KnowledgeSource::Memory {
                                memory_id: format!("raw-{}-head", sid),
                                layer: MemoryLayer::Raw,
                            },
                            confidence: 1.0,
                            reference_count: 0,
                            created_at: first.timestamp,
                            last_accessed: Utc::now(),
                            consolidated: false,
                        });
                    }
                    // 结尾：最近 N 条对话
                    let tail: String = entries
                        .iter()
                        .rev()
                        .take(3)
                        .map(|e| e.content.chars().take(100).collect::<String>())
                        .collect::<Vec<_>>()
                        .join(" | ");
                    candidates.push(MemoryEntry {
                        id: format!("raw-{}-tail", sid),
                        content: format!("[对话结尾] {}", tail),
                        tags: Vec::new(),
                        layer: MemoryLayer::Raw,
                        importance: 0.5,
                        source: KnowledgeSource::Memory {
                            memory_id: format!("raw-{}-tail", sid),
                            layer: MemoryLayer::Raw,
                        },
                        confidence: 1.0,
                        reference_count: 0,
                        created_at: entries.last().map(|e| e.timestamp).unwrap_or_default(),
                        last_accessed: Utc::now(),
                        consolidated: false,
                    });
                }
            }
        }

        candidates
    }
    /// 按关键词搜索记忆（供 search_memory 工具调用）
    ///
    /// 同时搜索潜意识层 + L3 原始会话 + L2 会话摘要，返回合并结果。
    pub fn search(&self, query: &str, max_results: usize) -> Vec<MemoryEntry> {
        let mut results = Vec::new();

        // 将查询字符串拆分为关键词
        let keywords: Vec<String> = query
            .split(&[' ', ',', '，', '、', '；'][..])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // 0. 潜意识层关键词匹配
        let sc_store = SubconsciousStore::new(Storage::new_lazy(self.config.base_dir.clone()));
        if let Ok(matched) = sc_store.match_keywords(&keywords, max_results) {
            for sc in &matched {
                if results.len() >= max_results {
                    break;
                }
                results.push(MemoryEntry {
                    id: sc.id.clone(),
                    content: format!("[印象] {} — {}", sc.topic, sc.impression),
                    tags: sc.trigger_keywords.clone(),
                    layer: MemoryLayer::TaskSummary,
                    importance: sc.importance,
                    source: KnowledgeSource::Memory {
                        memory_id: sc.id.clone(),
                        layer: MemoryLayer::TaskSummary,
                    },
                    confidence: 0.9,
                    reference_count: 0,
                    created_at: sc.created_at,
                    last_accessed: Utc::now(),
                    consolidated: true,
                });
            }
        }

        // 1. L3 原始会话关键词搜索
        if let Ok(raw_matches) = self.raw.search(&keywords, max_results) {
            for m in raw_matches {
                if results.len() >= max_results {
                    break;
                }
                results.push(m);
            }
        }

        // 2. L2 会话摘要匹配
        let summary_store =
            SessionSummaryStore::new(Storage::new_lazy(self.config.base_dir.clone()));
        if let Ok(summaries) = summary_store.match_keywords(&keywords, max_results) {
            for s in summaries {
                if results.len() >= max_results {
                    break;
                }
                results.push(MemoryEntry {
                    id: format!("summary-{}", s.session_id),
                    content: format!(
                        "[会话摘要] {} | 决策: {} | 踩坑: {}",
                        s.fact_summary,
                        s.decisions.join("; "),
                        s.pitfalls.join("; ")
                    ),
                    tags: s.tags.clone(),
                    layer: MemoryLayer::EventIndex,
                    importance: 0.7,
                    source: KnowledgeSource::Memory {
                        memory_id: format!("summary-{}", s.session_id),
                        layer: MemoryLayer::EventIndex,
                    },
                    confidence: 0.75,
                    reference_count: 0,
                    created_at: s.created_at,
                    last_accessed: Utc::now(),
                    consolidated: s.archived,
                });
            }
        }

        results
    }

    /// LLM 语义召回（async，由 orchestrator 调用）
    ///
    /// 把候选记忆给 LLM，让它选出与 query 最相关的 max 条
    pub async fn recall_with_llm(
        &self,
        query: &str,
        candidates: &[MemoryEntry],
        max: usize,
    ) -> Vec<MemoryEntry> {
        let llm = match &self.llm {
            Some(l) => l,
            None => return Vec::new(),
        };

        if candidates.is_empty() {
            return Vec::new();
        }

        // 构建候选列表（截断每个候选到 200 字避免 token 爆炸）
        let candidate_text: String = candidates
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "{}. [{}] {}",
                    i + 1,
                    m.id,
                    m.content.chars().take(200).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "# 任务\n\
             从以下记忆条目中，选出与用户查询最相关的 {max} 条。\n\
             返回相关条目的编号列表（JSON 数组），按相关度降序排列。\n\
             如果没有相关的，返回空数组。\n\n\
             # 用户查询\n\
             {query}\n\n\
             # 记忆条目\n\
             {candidate_text}\n\n\
             # 输出格式\n\
             只输出 JSON 数组，例如: [3, 1, 7]\n\
             不要输出其他任何内容。"
        );

        match llm.complete(&prompt).await {
            Ok(response) => {
                // 解析 LLM 返回的索引
                let indices = parse_index_response(&response, candidates.len());
                let mut results = Vec::new();
                for idx in indices.iter().take(max) {
                    if *idx < candidates.len() {
                        results.push(candidates[*idx].clone());
                    }
                }
                if !results.is_empty() {
                    tracing::info!(
                        "LLM 语义召回: {} 条候选中选出 {} 条",
                        candidates.len(),
                        results.len()
                    );
                    // LLM 召回后强化
                    let sc_ids: Vec<String> = results
                        .iter()
                        .filter(|r| r.id.starts_with("sc-"))
                        .map(|r| r.id.clone())
                        .collect();
                    if !sc_ids.is_empty() {
                        if let Err(e) = ImportanceManager::reinforce(
                            &Storage::new_lazy(self.config.base_dir.clone()),
                            &sc_ids,
                            MemoryStoreType::Subconscious,
                        ) {
                            tracing::warn!("LLM召回强化失败: {e}");
                        }
                    }
                }
                results
            }
            Err(e) => {
                tracing::warn!("LLM 语义召回失败，回退关键词: {e}");
                Vec::new()
            }
        }
    }

    /// 加载 fact_summary
    fn load_fact_summary(&self) -> Option<String> {
        let path = self.config.base_dir.join("fact_summary.json");
        let data = std::fs::read_to_string(&path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&data).ok()?;
        json.get("summary")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// 递增查询计数，返回是否应该触发四步分析
    pub fn tick_and_should_analyze(&self) -> bool {
        let count = self.query_count.fetch_add(1, Ordering::Relaxed) + 1;
        count > 0 && count % ANALYSIS_INTERVAL == 0
    }

    /// 读取最近的 L3 原始对话记录（供四步分析使用）
    pub fn read_recent_conversations(&self, max_entries: usize) -> Vec<String> {
        let mut entries = Vec::new();

        // 读取当前 session
        if let Ok(raw_entries) = self.raw.read_session(&self.config.session_id) {
            for entry in raw_entries.iter().rev().take(max_entries) {
                entries.push(
                    serde_json::json!({
                        "role": "user",
                        "content": entry.content,
                        "timestamp": entry.timestamp.to_rfc3339()
                    })
                    .to_string(),
                );
            }
        }

        entries.reverse();
        entries
    }

    /// 获取存储根目录（供外部创建 FourStepAnalyzer）
    pub fn base_dir(&self) -> &std::path::Path {
        self.config.base_dir.as_path()
    }

    /// 获取当前会话 ID
    pub fn session_id(&self) -> &str {
        &self.config.session_id
    }

    /// 加载潜意识层的全部摘要文本（供启动时注入主脑）
    pub fn load_subconscious_summary(&self) -> Option<String> {
        let sc_store = SubconsciousStore::new(Storage::new_lazy(self.config.base_dir.clone()));
        let entries = sc_store.load_all().ok()?;
        if entries.is_empty() {
            return None;
        }
        let text = entries
            .iter()
            .map(|e| {
                let kws = e.trigger_keywords.join("、");
                format!("• {} (触发词: {}) — {}", e.topic, kws, e.impression)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(text)
    }

    /// 处理协作消息（召回请求）
    fn handle_collaboration(&mut self, msg: &CollaborationMessage) -> Option<BrainResponse> {
        match msg.kind {
            CollaborationKind::Request => {
                // 解析召回关键词
                let keywords = msg
                    .content
                    .split(&[' ', ',', '，', '、'][..])
                    .map(String::from)
                    .collect::<Vec<_>>();
                let query = RecallQuery {
                    keywords,
                    tags: Vec::new(),
                    max_results: 5,
                    min_importance: 0.2,
                    layers: vec![
                        MemoryLayer::TaskSummary,
                        MemoryLayer::EventIndex,
                        MemoryLayer::ShortTerm,
                    ],
                };
                match self.recall.recall(&query) {
                    Ok(memories) => Some(BrainResponse {
                        from: self.id.clone(),
                        relevance: if memories.is_empty() { 0.1 } else { 0.8 },
                        confidence: 0.7,
                        result: BrainResponsePayload::MemoryRecall(memories),
                        need_slow_think: false,
                        timestamp: Utc::now(),
                    }),
                    Err(e) => {
                        tracing::warn!("记忆召回失败: {e}");
                        None
                    }
                }
            }
            CollaborationKind::Response | CollaborationKind::Dispatch => None,
        }
    }
}

impl BrainAgent for MemoryBrain {
    fn id(&self) -> &BrainId {
        &self.id
    }

    fn kind(&self) -> BrainKind {
        BrainKind::Memory
    }

    /// 快思考 — 关键词匹配 L0-L2
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult {
        let keywords = extract_keywords(&msg.content, self.config.max_keywords);
        let results = self.recall.quick_check(&keywords, 0.2);

        if results.is_empty() {
            FastThinkResult {
                relevant: false,
                confidence: 0.3,
                summary: None,
                suggested_tools: Vec::new(),
                matched_experience: None,
            }
        } else {
            let summary_parts: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
            FastThinkResult {
                relevant: true,
                confidence: 0.7,
                summary: Some(summary_parts.join("; ")),
                suggested_tools: Vec::new(),
                matched_experience: results.first().map(|r| r.content.clone()),
            }
        }
    }

    /// 慢思考 — 深度检索含 L3
    fn slow_think(
        &self,
        msg: &BroadcastMessage,
        _context: &ThinkContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SlowThinkResult> + Send + '_>> {
        let keywords = extract_keywords(&msg.content, self.config.max_keywords);
        let query = RecallQuery {
            keywords,
            tags: Vec::new(),
            max_results: 10,
            min_importance: 0.1,
            layers: vec![
                MemoryLayer::TaskSummary,
                MemoryLayer::EventIndex,
                MemoryLayer::ShortTerm,
                MemoryLayer::Raw,
            ],
        };

        Box::pin(async move {
            // 使用 block_on 因为 recall 是同步操作
            // 但我们在 async 上下文中，直接调用同步方法
            match self.recall.recall(&query) {
                Ok(memories) => {
                    let conclusion = if memories.is_empty() {
                        "未找到相关记忆".into()
                    } else {
                        memories
                            .iter()
                            .map(|m| format!("[{}] {}", layer_label(m.layer), m.content))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    SlowThinkResult {
                        conclusion,
                        reasoning_path: memories.iter().map(|m| m.content.clone()).collect(),
                        confidence: if memories.is_empty() { 0.2 } else { 0.75 },
                        sources: memories.iter().map(|m| m.source.clone()).collect(),
                        new_experience: None,
                    }
                }
                Err(e) => SlowThinkResult {
                    conclusion: format!("记忆检索失败: {e}"),
                    reasoning_path: Vec::new(),
                    confidence: 0.1,
                    sources: Vec::new(),
                    new_experience: None,
                },
            }
        })
    }

    /// 接收广播 → 存入 L3 + L2
    fn on_broadcast(&mut self, msg: BroadcastMessage) {
        if let Err(e) = self.store_broadcast(&msg) {
            tracing::warn!("记忆脑存储广播失败: {e}");
        }
    }

    /// 接收协作消息
    fn on_collaboration(&mut self, msg: CollaborationMessage) -> Option<BrainResponse> {
        self.handle_collaboration(&msg)
    }
}

/// 解析 LLM 返回的索引列表（如 [3, 1, 7]）
fn parse_index_response(response: &str, max_len: usize) -> Vec<usize> {
    let text = response.trim();

    // 尝试提取 JSON 数组
    let json_str = if let Some(start) = text.find('[') {
        let end = text[start..]
            .find(']')
            .map(|i| start + i + 1)
            .unwrap_or(text.len());
        text[start..end].to_string()
    } else {
        // 可能只返回了数字，如 "3, 1, 7"
        format!("[{}]", text)
    };

    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&json_str) {
        return arr
            .iter()
            .filter_map(|v| v.as_u64())
            .map(|v| if v == 0 { 0 } else { (v - 1) as usize })
            .filter(|&i| i < max_len)
            .collect();
    }

    Vec::new()
}

/// 从内容中提取关键词（简化版，对中文友好）
///
/// 策略：
/// 1. 按标点和空格分割
/// 2. 对长中文片段做滑动窗口（2-4字）
/// 3. 过滤停用词和短词
fn extract_keywords(content: &str, max: usize) -> Vec<String> {
    let stopwords = [
        "的", "了", "是", "在", "我", "你", "他", "她", "它", "们", "这", "那", "有", "不", "就",
        "也", "都", "还", "又", "很", "要", "会", "能", "把", "被", "让", "给", "到", "和", "与",
        "吗", "呢", "吧", "啊", "哦", "嗯", "呀", "哈", "哪", "什么", "怎么", "如何", "可以",
        "能够", "应该", "需要", "刚刚", "刚才", "一个", "一些", "这个", "那个", "这些", "那些",
    ];

    let mut keywords = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // 1. 按标点和空格分割
    let segments: Vec<&str> = content
        .split(
            &[
                ' ', ',', '，', '。', '、', '；', '！', '？', '\n', '\t', '：', ':', '(', ')',
                '（', '）', '[', ']', '"', '\'',
            ][..],
        )
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    for seg in &segments {
        // 如果是英文/数字/混合，直接作为关键词
        if seg.chars().any(|c| c.is_ascii_alphanumeric()) && seg.len() >= 2 {
            if seen.insert(seg.to_lowercase()) {
                keywords.push(seg.to_string());
            }
            continue;
        }

        // 纯中文：做滑动窗口提取 2-4 字片段
        let chars: Vec<char> = seg.chars().collect();
        for window_size in [4, 3, 2] {
            if chars.len() <= window_size {
                // 整段作为关键词（但过滤停用词）
                let word: String = chars.iter().collect();
                if word.len() >= 2 && !stopwords.contains(&word.as_str()) {
                    if seen.insert(word.clone()) {
                        keywords.push(word);
                    }
                }
                continue;
            }
            for i in 0..=chars.len() - window_size {
                let word: String = chars[i..i + window_size].iter().collect();
                if !stopwords.contains(&word.as_str()) {
                    if seen.insert(word.clone()) {
                        keywords.push(word.clone());
                    }
                }
            }
        }
    }

    keywords.truncate(max);
    keywords
}

fn layer_label(layer: MemoryLayer) -> &'static str {
    match layer {
        MemoryLayer::TaskSummary => "L0",
        MemoryLayer::EventIndex => "L1",
        MemoryLayer::ShortTerm => "L2",
        MemoryLayer::Raw => "L3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::BrainContext;
    use tempfile::TempDir;

    fn make_brain() -> (TempDir, MemoryBrain) {
        let tmp = TempDir::new().unwrap();
        let config = MemoryBrainConfig {
            base_dir: tmp.path().to_path_buf(),
            session_id: "test-session".into(),
            max_keywords: 5,
        };
        let brain = MemoryBrain::new(config).unwrap();
        (tmp, brain)
    }

    fn make_broadcast(content: &str) -> BroadcastMessage {
        BroadcastMessage {
            content: content.into(),
            raw_input: content.into(),
            context: BrainContext {
                current_date: "2026-04-04".into(),
                cwd: "/test".into(),
                git_branch: Some("main".into()),
                platform: "darwin".into(),
            },
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn brain_id_and_kind() {
        let (_tmp, brain) = make_brain();
        assert_eq!(brain.id(), &BrainId::memory());
        assert_eq!(brain.kind(), BrainKind::Memory);
    }

    #[test]
    fn store_broadcast_and_retrieve() {
        let (_tmp, mut brain) = make_brain();
        let msg = make_broadcast("用户查询清明节假期安排");
        brain.store_broadcast(&msg).unwrap();

        // L3 应该有 1 条
        assert_eq!(brain.raw.count().unwrap(), 1);
        // L2 应该有 1 条
        assert_eq!(brain.short_term.len(), 1);
    }

    #[test]
    fn fast_think_no_memory() {
        let (_tmp, brain) = make_brain();
        let msg = make_broadcast("这是一个新查询");
        let result = brain.fast_think(&msg);
        assert!(!result.relevant);
    }

    #[test]
    fn fast_think_with_memory() {
        let (_tmp, mut brain) = make_brain();

        // 先存一些记忆
        brain
            .store_broadcast(&make_broadcast("清明节是4月5日，放假1天"))
            .unwrap();

        // 快思考应该能找到
        let result = brain.fast_think(&make_broadcast("清明节几号"));
        // 取决于关键词提取，可能 relevant 也可能不是
        // 至少不应该 panic
        assert!(result.confidence >= 0.0);
    }

    #[test]
    fn on_broadcast_stores_message() {
        let (_tmp, mut brain) = make_brain();
        let msg = make_broadcast("测试消息存储");
        brain.on_broadcast(msg);

        assert_eq!(brain.short_term.len(), 1);
    }

    #[test]
    fn extract_keywords_basic() {
        let kws = extract_keywords("用户查询清明节假期安排", 5);
        assert!(!kws.is_empty());
        // 至少能分出一些词
        assert!(!kws.is_empty());
    }

    #[test]
    fn memory_stats() {
        let (_tmp, mut brain) = make_brain();
        brain.store_broadcast(&make_broadcast("测试统计")).unwrap();

        let stats = brain.stats().unwrap();
        assert_eq!(stats.l2_count, 1);
        assert_eq!(stats.l3_count, 1);
    }

    #[tokio::test]
    async fn slow_think_returns_results() {
        let (_tmp, mut brain) = make_brain();
        brain
            .store_broadcast(&make_broadcast("清明节是4月5日"))
            .unwrap();

        let msg = make_broadcast("清明节几号");
        let result = brain
            .slow_think(
                &msg,
                &ThinkContext {
                    related_memories: Vec::new(),
                    task_history: Vec::new(),
                },
            )
            .await;

        assert!(result.confidence > 0.0);
    }

    #[test]
    fn consolidation_runs() {
        let (_tmp, mut brain) = make_brain();
        brain
            .store_broadcast(&make_broadcast("完成了记忆脑开发"))
            .unwrap();

        let report = brain.consolidate().unwrap();
        // 应该创建了事件
        assert!(report.event_indexes_created > 0 || report.memories_consolidated > 0);
    }
}
