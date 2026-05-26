//! 金字塔版记忆脑
//!
//! 基于 per-persona 金字塔存储的新 MemoryBrain。
//! 逐步替代旧 MemoryBrain，保持对外接口兼容。

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use brain_core::types::{BrainId, MemoryEntry, MemoryLayer, KnowledgeSource, TurnRecord};

use crate::abstract_layer::AbstractLayer;
use crate::concentration::{ConcentrationEngine, ConcentrationReport};
use crate::error::Result;
use crate::persona_manager::PersonaManager;
use crate::profile_eval::{EvalInfoStore, ProfileStore};
use crate::progressive_recall::{InjectContext, ProgressiveRecall};
use crate::pyramid_storage::PyramidStorage;
use crate::raw_pool::RawPool;
use crate::subconscious_pool::SubconsciousPool;
use crate::summary_pool::SummaryPool;

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
}

impl Default for PyramidMemoryBrainConfig {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".into());
        Self {
            base_dir: PathBuf::from(home).join(".ai-brain"),
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

    /// 存储对话轮次到 L1
    pub fn store_turn(
        &self,
        role: &str,
        content: &str,
        tool_output: Option<&str>,
    ) -> Result<()> {
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
        let engine = ConcentrationEngine::new(self.storage.clone(), self.config.session_id.clone());
        engine.run(llm).await
    }

    /// 自动注入上下文（潜意识 + 画像 + injectable 经验）
    pub fn auto_inject(&self) -> Result<InjectContext> {
        let recall = ProgressiveRecall::new(self.storage.clone());
        recall.auto_inject()
    }

    /// 生成注入文本
    pub fn build_inject_text(&self) -> Result<String> {
        let recall = ProgressiveRecall::new(self.storage.clone());
        recall.build_inject_text()
    }

    /// 渐进式召回
    pub fn recall(
        &self,
        query: &str,
        max_depth: crate::pyramid_types::PyramidLayer,
    ) -> Result<Vec<crate::progressive_recall::RecallHit>> {
        let recall = ProgressiveRecall::new(self.storage.clone());
        recall.recall(query, max_depth)
    }

    /// 获取潜意识叙事
    pub fn load_subconscious_summary(&self) -> Result<String> {
        let pool = SubconsciousPool::new(self.storage.clone());
        pool.narrative()
    }

    /// 获取用户画像
    pub fn load_profile(&self) -> Result<String> {
        let store = ProfileStore::new(self.storage.clone());
        store.summary()
    }

    /// 获取评估信息注入文本
    pub fn load_eval_inject_text(&self) -> Result<String> {
        let store = EvalInfoStore::new(self.storage.clone());
        store.inject_text()
    }

    /// 切换人格
    pub fn switch_persona(&mut self, persona_id: &str) -> Result<()> {
        self.persona_manager.switch(persona_id)?;
        self.storage = PyramidStorage::new(
            self.config.base_dir.clone(),
            persona_id,
        );
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
        let pool = RawPool::new(self.storage.clone());
        for turn in turns {
            let role = format!("{:?}", turn.role).to_lowercase();
            let tool_output = turn.tool_call.as_ref().map(|tc| {
                format!("{}: {}", tc.tool_name, tc.output.chars().take(200).collect::<String>())
            });
            pool.append_turn(&self.config.session_id, &role, &turn.content, tool_output.as_deref())?;
        }
        tracing::info!("L1 完整轨迹追加 {} 条", turns.len());
        Ok(())
    }

    /// 读取最近的对话记录（兼容旧 MemoryBrain 接口）
    ///
    /// 从 L1 raw pool 读取最近 max_entries 条记录，返回 JSON 字符串。
    pub fn read_recent_conversations(&self, max_entries: usize) -> Vec<String> {
        let pool = RawPool::new(self.storage.clone());
        let mut entries = Vec::new();

        if let Ok(turns) = pool.read_session(&self.config.session_id) {
            for turn in turns.iter().rev().take(max_entries) {
                entries.push(
                    serde_json::json!({
                        "role": turn.role,
                        "content": turn.content,
                        "timestamp": turn.timestamp.to_rfc3339()
                    })
                    .to_string(),
                );
            }
        }

        entries.reverse();
        entries
    }

    /// 记忆召回（兼容旧 MemoryBrain 接口）
    ///
    /// 将金字塔召回结果转换为 MemoryEntry 格式。
    pub fn recall_for_context(&self, query: &str, _max: usize) -> Vec<MemoryEntry> {
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
        let pool = SummaryPool::new(self.storage.clone());
        let summaries = pool.load_all().unwrap_or_default();

        summaries
            .into_iter()
            .rev()
            .take(limit)
            .enumerate()
            .map(|(i, s)| RecentSummary {
                file_path: self.storage.l2_dir()
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
        self.load_subconscious_summary().ok().filter(|s| !s.is_empty())
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

    fn make_brain(tmp: &tempfile::TempDir) -> PyramidMemoryBrain {
        let config = PyramidMemoryBrainConfig {
            base_dir: tmp.path().to_path_buf(),
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
            .store_turns_batch(&[
                ("User", "hello", None),
                ("Tool", "ls", Some("file1")),
            ])
            .unwrap();

        let pool = RawPool::new(brain.storage().clone());
        let turns = pool.read_session("sess-test").unwrap();
        assert_eq!(turns.len(), 2);
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

        brain.persona_manager_mut()
            .create("writer".into(), "作家".into(), "网文".into(), "".into(), PersonaConfig::default())
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
}
