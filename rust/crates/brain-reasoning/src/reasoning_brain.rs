use std::path::PathBuf;

use brain_core::agent::BrainAgent;
use brain_core::types::{
    BrainId, BrainKind, BrainResponse, BroadcastMessage, CollaborationKind, CollaborationMessage,
    FastThinkResult, SlowThinkResult, ThinkContext,
};

use crate::error::Result;
use crate::experience::ExperienceStore;
use crate::pattern_matcher::PatternMatcher;
use crate::reasoning_engine::ReasoningEngine;

/// 推理脑配置
#[derive(Debug, Clone)]
pub struct ReasoningConfig {
    /// 经验库存储路径
    pub experience_path: PathBuf,
    /// 最小成功率阈值
    pub min_success_rate: f64,
    /// 快思考置信度阈值（高于此值直接用快思考结果）
    pub fast_think_threshold: f64,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".into());
        Self {
            experience_path: PathBuf::from(home)
                .join(".ai-brain")
                .join("reasoning")
                .join("experience.json"),
            min_success_rate: 0.5,
            fast_think_threshold: 0.7,
        }
    }
}

/// 推理脑（前额叶）
///
/// 职责：
/// - 快思考：经验库模式匹配（~10ms）
/// - 慢思考：深度推理（~1-5s），生成新经验
/// - 新经验写入经验库 + 通过协作通道通知记忆脑
/// - 失败路径标记为反面案例
pub struct ReasoningBrain {
    id: BrainId,
    config: ReasoningConfig,
    engine: ReasoningEngine,
    pending_experience_ids: Vec<String>,
}

impl ReasoningBrain {
    pub fn new(config: ReasoningConfig) -> Result<Self> {
        let experience =
            ExperienceStore::new(config.experience_path.clone(), config.min_success_rate);
        let matcher = PatternMatcher::new(experience);
        let engine = ReasoningEngine::new(matcher);

        Ok(Self {
            id: BrainId::reasoning(),
            config,
            engine,
            pending_experience_ids: Vec::new(),
        })
    }

    /// 注入 LLM Provider（由编排器在初始化时调用）
    pub fn set_llm(&mut self, llm: std::sync::Arc<dyn brain_llm::LlmProvider>) {
        self.engine.set_llm(llm);
        tracing::info!("推理脑已接入 LLM");
    }

    /// 获取经验库统计
    pub fn experience_stats(&self) -> (usize, usize) {
        let store = self.engine.pattern_matcher().experience();
        let total = store.len();
        let negative = store.get_negative_examples(&[], 10000).len();
        (total, negative)
    }

    /// 持久化经验库
    pub fn persist(&self) -> Result<()> {
        self.engine.pattern_matcher().experience().persist()
    }
}

impl BrainAgent for ReasoningBrain {
    fn id(&self) -> &BrainId {
        &self.id
    }

    fn kind(&self) -> BrainKind {
        BrainKind::Reasoning
    }

    /// 快思考 — 经验库模式匹配
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult {
        let matcher = self.engine.pattern_matcher();

        if let Some((confidence, reasoning_path, exp_id)) = matcher.match_pattern(msg) {
            if confidence >= self.config.fast_think_threshold {
                return FastThinkResult {
                    relevant: true,
                    confidence,
                    summary: Some(reasoning_path.join(" → ")),
                    suggested_tools: Vec::new(),
                    matched_experience: Some(exp_id),
                };
            }
            // 有匹配但置信度不够高，标记为相关但需要慢思考
            return FastThinkResult {
                relevant: true,
                confidence,
                summary: Some(format!(
                    "部分匹配经验，需要深入分析: {}",
                    reasoning_path.join("→")
                )),
                suggested_tools: Vec::new(),
                matched_experience: Some(exp_id),
            };
        }

        // 无匹配
        FastThinkResult {
            relevant: true, // 推理脑总是相关，它负责思考
            confidence: 0.4,
            summary: None,
            suggested_tools: Vec::new(),
            matched_experience: None,
        }
    }

    /// 慢思考 — 深度推理
    fn slow_think(
        &self,
        msg: &BroadcastMessage,
        _context: &ThinkContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SlowThinkResult> + Send + '_>> {
        let msg = msg.clone();
        Box::pin(async move {
            match self.engine.reason(&msg).await {
                Ok(result) => result,
                Err(e) => SlowThinkResult {
                    conclusion: format!("推理失败: {e}"),
                    reasoning_path: Vec::new(),
                    confidence: 0.1,
                    sources: Vec::new(),
                    new_experience: None,
                },
            }
        })
    }

    fn on_broadcast(&mut self, msg: BroadcastMessage) {
        // 快思考判断
        let result = self.fast_think(&msg);

        // 如果有匹配经验，记录使用
        if let Some(exp_id) = &result.matched_experience {
            self.engine
                .pattern_matcher_mut()
                .experience_mut()
                .record_usage(exp_id, true);
            self.pending_experience_ids.push(exp_id.clone());
        }

        // 如果慢思考产出了新经验，保存
        // (实际由主脑通过 on_collaboration 调度慢思考后处理)
    }

    fn on_collaboration(&mut self, msg: CollaborationMessage) -> Option<BrainResponse> {
        match msg.kind {
            CollaborationKind::Dispatch => {
                // Dispatch 现在由 orchestrator 通过 slow_think() 处理，
                // 此路径不再需要硬编码响应
                tracing::debug!(
                    "推理脑收到 Dispatch（应由 slow_think 处理）: {}",
                    msg.content.chars().take(100).collect::<String>()
                );
                None
            }
            CollaborationKind::Request => {
                if msg.to.contains(&self.id) {
                    tracing::debug!(
                        "推理脑收到协作请求: {}",
                        msg.content.chars().take(100).collect::<String>()
                    );
                }
                None
            }
            CollaborationKind::Response => {
                if msg.from == BrainId::memory() {
                    tracing::debug!("推理脑收到记忆脑召回结果");
                }
                None
            }
        }
    }

    /// 慢思考完成后自动保存新经验到经验库
    fn on_slow_think_result(&mut self, result: &SlowThinkResult) {
        if let Some(exp) = &result.new_experience {
            let id = format!("exp-{}", chrono::Utc::now().timestamp_millis());
            let entry = crate::experience::ExperienceEntry {
                id: id.clone(),
                trigger_pattern: exp.trigger_pattern.clone(),
                reasoning_path: exp.reasoning_path.clone(),
                tools_used: exp.tools_used.clone(),
                success_rate: 0.5,
                usage_count: 1,
                is_negative: false,
                files_modified: Vec::new(),
                failure_reason: None,
                created_at: chrono::Utc::now(),
                last_used: chrono::Utc::now(),
            };
            self.engine
                .pattern_matcher_mut()
                .experience_mut()
                .store(entry);
            self.pending_experience_ids.push(id);
            tracing::info!("推理脑新经验已保存: {}", exp.trigger_pattern);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::BrainContext;
    use tempfile::TempDir;

    fn make_brain() -> ReasoningBrain {
        let tmp = TempDir::new().unwrap();
        let config = ReasoningConfig {
            experience_path: tmp.path().join("experience.json"),
            min_success_rate: 0.5,
            fast_think_threshold: 0.7,
        };
        let brain = ReasoningBrain::new(config).unwrap();
        std::mem::forget(tmp);
        brain
    }

    fn make_broadcast(content: &str) -> BroadcastMessage {
        use chrono::Utc;
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
        let brain = make_brain();
        assert_eq!(brain.id(), &BrainId::reasoning());
        assert_eq!(brain.kind(), BrainKind::Reasoning);
    }

    #[test]
    fn fast_think_no_experience() {
        let brain = make_brain();
        let msg = make_broadcast("新任务");
        let result = brain.fast_think(&msg);
        // 推理脑总是 relevant
        assert!(result.relevant);
        assert!(result.matched_experience.is_none());
    }

    #[test]
    fn fast_think_with_experience() {
        let mut brain = make_brain();
        brain.engine.save_new_experience(
            "代码,bug修复",
            vec!["定位问题".into(), "修复代码".into(), "运行测试".into()],
            vec!["Grep".into(), "Edit".into()],
        );

        let msg = make_broadcast("代码里有一个bug需要修复");
        let result = brain.fast_think(&msg);
        assert!(result.relevant);
        assert!(result.matched_experience.is_some());
    }

    #[tokio::test]
    async fn slow_think_without_llm_returns_low_confidence() {
        let brain = make_brain();
        let msg = make_broadcast("分析系统性能瓶颈");
        let result = brain
            .slow_think(
                &msg,
                &ThinkContext {
                    related_memories: Vec::new(),
                    task_history: Vec::new(),
                },
            )
            .await;
        // 无 LLM 时，reason() 返回 Err，slow_think 捕获后返回低置信度结果
        assert!((result.confidence - 0.1).abs() < f64::EPSILON);
        assert!(result.reasoning_path.is_empty());
    }

    #[test]
    fn on_broadcast_records_usage() {
        let mut brain = make_brain();
        brain
            .engine
            .save_new_experience("测试", vec!["步骤1".into()], vec![]);

        let msg = make_broadcast("测试任务");
        brain.on_broadcast(msg);
        assert!(!brain.pending_experience_ids.is_empty());
    }

    #[test]
    fn experience_stats() {
        let mut brain = make_brain();
        brain
            .engine
            .save_new_experience("测试", vec!["步骤".into()], vec![]);
        let (total, _negative) = brain.experience_stats();
        assert_eq!(total, 1);
    }
}
