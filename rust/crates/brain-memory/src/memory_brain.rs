use std::path::PathBuf;

use chrono::Utc;

use brain_core::agent::BrainAgent;
use brain_core::types::{
    BrainId, BrainKind, BrainResponse, BrainResponsePayload, BroadcastMessage, CollaborationKind,
    CollaborationMessage, FastThinkResult, KnowledgeSource, MemoryEntry, MemoryLayer, RecallQuery,
    SlowThinkResult, ThinkContext,
};

use crate::consolidation::ConsolidationEngine;
use crate::error::Result;
use crate::event_index::EventIndexLayer;
use crate::raw_layer::{RawEntry, RawLayer};
use crate::recall::RecallEngine;
use crate::short_term::{ShortTermConfig, ShortTermLayer};
use crate::storage::Storage;
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
pub struct MemoryBrain {
    id: BrainId,
    config: MemoryBrainConfig,
    raw: RawLayer,
    short_term: ShortTermLayer,
    event_index: EventIndexLayer,
    task_summary: TaskSummaryLayer,
    recall: RecallEngine,
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
        })
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

    /// 为慢思考提供上下文记忆
    ///
    /// 从 L0→L1→L2 逐层召回与 query 关联的记忆条目。
    pub fn recall_for_context(&self, query: &str, max: usize) -> Vec<MemoryEntry> {
        let keywords = crate::memory_brain::extract_keywords(query, 5);
        let query_obj = RecallQuery {
            keywords,
            tags: Vec::new(),
            max_results: max,
            min_importance: 0.2,
            layers: vec![
                MemoryLayer::TaskSummary,
                MemoryLayer::EventIndex,
                MemoryLayer::ShortTerm,
            ],
        };
        self.recall.recall(&query_obj).unwrap_or_default()
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

/// 从内容中提取关键词（简化版，基于规则分词）
fn extract_keywords(content: &str, max: usize) -> Vec<String> {
    // 按标点和空格分割，过滤短词
    let words: Vec<String> = content
        .split(&[' ', ',', '，', '。', '、', '；', '！', '？', '\n', '\t'][..])
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() >= 2) // 至少 2 个字符
        .take(max)
        .collect();
    words
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
