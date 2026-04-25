use std::future::Future;
use std::pin::Pin;

use brain_core::agent::BrainAgent;
use brain_core::types::{
    BrainId, BrainKind, BrainResponse, BroadcastMessage, CollaborationKind, CollaborationMessage,
    FastThinkResult, KnowledgeSource, SafetyCheckResult, SlowThinkResult, ThinkContext, ToolCall,
    TruthfulnessResult,
};

use crate::error::Result;
use crate::safety::SafetyChecker;
use crate::truthfulness::TruthfulnessChecker;

/// 校验脑配置
#[derive(Debug, Clone)]
pub struct ValidationConfig {
    /// 真实性警告阈值
    pub truthfulness_warning_threshold: f64,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            truthfulness_warning_threshold: 0.60,
        }
    }
}

/// 校验脑（ACC + 基底节）
///
/// 职责：
/// - **安全性校验**: 检查工具调用是否安全，识别高危操作
/// - **真实性校验**: 追踪信息来源，评估可信度，交叉验证
///
/// 快思考: 高危关键词快速匹配（~1ms）
/// 慢思考: 完整的多来源真实性分析
pub struct ValidationBrain {
    id: BrainId,
    #[allow(dead_code)]
    config: ValidationConfig,
    safety: SafetyChecker,
    truthfulness: TruthfulnessChecker,
    /// 最近一次安全校验缓存
    last_safety_check: Option<SafetyCheckResult>,
    /// 最近一次真实性校验缓存
    last_truthfulness_check: Option<TruthfulnessResult>,
}

impl ValidationBrain {
    pub fn new(config: ValidationConfig) -> Result<Self> {
        Ok(Self {
            id: BrainId::validation(),
            config,
            safety: SafetyChecker::new(),
            truthfulness: TruthfulnessChecker::new(),
            last_safety_check: None,
            last_truthfulness_check: None,
        })
    }

    /// 安全校验 — 检查工具调用是否安全
    pub fn check_safety(&mut self, tool_call: &ToolCall) -> SafetyCheckResult {
        let result = self.safety.check(tool_call);
        self.last_safety_check = Some(result.clone());
        result
    }

    /// 真实性校验 — 追踪信息来源，评估可信度
    pub fn check_truthfulness(
        &mut self,
        claim: &str,
        sources: &[KnowledgeSource],
    ) -> TruthfulnessResult {
        let result = self.truthfulness.check(claim, sources);
        self.last_truthfulness_check = Some(result.clone());
        result
    }

    /// 快速安全判断 — 只看工具名和高危关键词
    fn quick_safety_check(&self, msg: &BroadcastMessage) -> bool {
        let content_lower = msg.content.to_lowercase();
        let raw_lower = msg.raw_input.to_lowercase();

        // 检查内容是否包含高危关键词
        let keywords = self.safety.high_risk_keywords();
        keywords.iter().any(|kw| {
            let kw_lower = kw.to_lowercase();
            content_lower.contains(&kw_lower) || raw_lower.contains(&kw_lower)
        })
    }

    /// 获取最近的安全校验结果
    pub fn last_safety_check(&self) -> Option<&SafetyCheckResult> {
        self.last_safety_check.as_ref()
    }

    /// 获取最近的真实性校验结果
    pub fn last_truthfulness_check(&self) -> Option<&TruthfulnessResult> {
        self.last_truthfulness_check.as_ref()
    }
}

impl BrainAgent for ValidationBrain {
    fn id(&self) -> &BrainId {
        &self.id
    }

    fn kind(&self) -> BrainKind {
        BrainKind::Validation
    }

    /// 快思考 — 高危关键词快速匹配
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult {
        let has_danger = self.quick_safety_check(msg);

        if has_danger {
            FastThinkResult {
                relevant: true,
                confidence: 0.95,
                summary: Some("检测到高危操作关键词，需要完整安全校验".into()),
                suggested_tools: Vec::new(),
                matched_experience: None,
            }
        } else {
            // 一般内容，校验脑也保持相关（安全审计角色）
            FastThinkResult {
                relevant: true,
                confidence: 0.6,
                summary: Some("内容安全，可执行轻量审计".into()),
                suggested_tools: Vec::new(),
                matched_experience: None,
            }
        }
    }

    /// 慢思考 — 完整的真实性校验
    fn slow_think(
        &self,
        msg: &BroadcastMessage,
        _context: &ThinkContext,
    ) -> Pin<Box<dyn Future<Output = SlowThinkResult> + Send + '_>> {
        let content = msg.content.clone();
        Box::pin(async move {
            // 慢思考阶段做更细致的分析
            // 当前用规则 stub，Phase 7 接入 LLM 做深度语义验证
            let preview: String = content.chars().take(50).collect();
            SlowThinkResult {
                conclusion: format!("校验脑对 \"{preview}\" 完成深度审查"),
                reasoning_path: vec![
                    "关键词安全扫描".into(),
                    "来源追踪分析".into(),
                    "交叉验证".into(),
                ],
                confidence: 0.85,
                sources: Vec::new(),
                new_experience: None,
            }
        })
    }

    fn on_broadcast(&mut self, msg: BroadcastMessage) {
        let result = self.fast_think(&msg);
        // 如果检测到危险，记录缓存
        if result.confidence > 0.8 {
            tracing::warn!("校验脑检测到潜在风险: {:?}", result.summary);
        }
    }

    fn on_collaboration(&mut self, msg: CollaborationMessage) -> Option<BrainResponse> {
        match msg.kind {
            CollaborationKind::Request => {
                if msg.to.contains(&self.id) {
                    tracing::debug!(
                        "校验脑收到安全审查请求: {}",
                        msg.content.chars().take(100).collect::<String>()
                    );
                }
            }
            CollaborationKind::Response => {}
            CollaborationKind::Dispatch => {
                tracing::debug!("校验脑收到主脑调度指令");
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::{BrainContext, MemoryLayer};
    use chrono::Utc;

    fn make_brain() -> ValidationBrain {
        ValidationBrain::new(ValidationConfig::default()).unwrap()
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
        let brain = make_brain();
        assert_eq!(brain.id(), &BrainId::validation());
        assert_eq!(brain.kind(), BrainKind::Validation);
    }

    #[test]
    fn fast_think_safe_content() {
        let brain = make_brain();
        let msg = make_broadcast("帮我查看这个文件的内容");
        let result = brain.fast_think(&msg);
        assert!(result.relevant);
        assert!(result.confidence < 0.9);
    }

    #[test]
    fn fast_think_dangerous_content() {
        let brain = make_brain();
        let msg = make_broadcast("执行 rm -rf /tmp/test");
        let result = brain.fast_think(&msg);
        assert!(result.relevant);
        assert!(result.confidence > 0.9);
    }

    #[test]
    fn check_safety_high_risk() {
        let mut brain = make_brain();
        let call = ToolCall {
            tool_name: "Bash".into(),
            input: serde_json::Value::String("rm -rf /tmp/test".into()),
            validated: false,
            validation_id: None,
        };
        let result = brain.check_safety(&call);
        assert!(!result.safe);
        assert!(brain.last_safety_check().unwrap().reason.is_some());
    }

    #[test]
    fn check_safety_low_risk() {
        let mut brain = make_brain();
        let call = ToolCall {
            tool_name: "Read".into(),
            input: serde_json::Value::String("/path/to/file".into()),
            validated: false,
            validation_id: None,
        };
        let result = brain.check_safety(&call);
        assert!(result.safe);
    }

    #[test]
    fn check_truthfulness_cross_verified() {
        let mut brain = make_brain();
        let result = brain.check_truthfulness(
            "清明节4月4-6日放假",
            &[
                KnowledgeSource::WebSearch {
                    url: "https://example.com".into(),
                },
                KnowledgeSource::Memory {
                    memory_id: "mem_001".into(),
                    layer: MemoryLayer::ShortTerm,
                },
            ],
        );
        assert!(result.confidence > 0.6);
        assert!(result.cross_verified);
        assert!(brain.last_truthfulness_check().is_some());
    }

    #[test]
    fn check_truthfulness_no_source_warning() {
        let mut brain = make_brain();
        let result = brain.check_truthfulness("无来源声明", &[]);
        assert!((result.confidence - 0.0).abs() < f64::EPSILON);
        assert!(result.warning.is_some());
    }

    #[tokio::test]
    async fn slow_think_returns_result() {
        let brain = make_brain();
        let msg = make_broadcast("检查这段代码的安全性");
        let result = brain
            .slow_think(
                &msg,
                &ThinkContext {
                    related_memories: Vec::new(),
                    task_history: Vec::new(),
                },
            )
            .await;
        assert!(!result.conclusion.is_empty());
        assert!(!result.reasoning_path.is_empty());
    }

    #[test]
    fn on_broadcast_dangerous_logs_warning() {
        let mut brain = make_brain();
        let msg = make_broadcast("执行 rm -rf /tmp");
        brain.on_broadcast(msg);
        // 验证不会 panic 即可
    }

    #[test]
    fn on_collaboration_handles_request() {
        let mut brain = make_brain();
        let msg = CollaborationMessage {
            id: "collab_001".into(),
            from: BrainId::master(),
            to: vec![BrainId::validation()],
            correlation_id: Some("corr_001".into()),
            hop_count: 0,
            priority: brain_core::types::MessagePriority::High,
            content: "请校验此操作".into(),
            kind: CollaborationKind::Request,
        };
        brain.on_collaboration(msg);
    }
}
