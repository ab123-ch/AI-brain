use std::sync::Arc;

use brain_core::types::{EvolutionRule, PitfallRecord, ProgressEvent, UserProfile};
use brain_llm::{ChatMessage, ChatRequest, LlmProvider};
use serde::{Deserialize, Serialize};

use crate::checker;
use crate::error::{EvalError, Result};
use crate::prompts;

/// 问题严重程度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    /// 警告：建议改进但不阻断
    Warning,
    /// 严重：必须修正
    Critical,
}

impl std::fmt::Display for IssueSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warning => write!(f, "Warning"),
            Self::Critical => write!(f, "Critical"),
        }
    }
}

/// 问题分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCategory {
    /// 重复踩坑
    PitfallRepeat,
    /// 违反用户偏好
    PreferenceViolation,
    /// 已知失败模式重复
    KnownFailurePattern,
    /// 偷懒行为（写 TODO 而非实现）
    LazyBehavior,
    /// 事实错误
    FactError,
    /// 忽略用户指令
    InstructionIgnored,
}

impl std::fmt::Display for IssueCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PitfallRepeat => write!(f, "PitfallRepeat"),
            Self::PreferenceViolation => write!(f, "PreferenceViolation"),
            Self::KnownFailurePattern => write!(f, "KnownFailurePattern"),
            Self::LazyBehavior => write!(f, "LazyBehavior"),
            Self::FactError => write!(f, "FactError"),
            Self::InstructionIgnored => write!(f, "InstructionIgnored"),
        }
    }
}

/// 单个评估问题（quick_check 仍使用结构化格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalIssue {
    /// 严重程度
    pub severity: IssueSeverity,
    /// 问题分类
    pub category: IssueCategory,
    /// 问题描述
    pub description: String,
    /// 给主脑的修正建议
    pub suggestion: String,
}

/// 评估结果 — LLM 返回自然语言反馈，直接喂给主脑
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    /// 是否通过（LLM 回复中包含"存在问题"则为 false）
    pub passed: bool,
    /// LLM 的完整评估反馈文本（markdown 格式，直接注入主脑上下文）
    pub feedback: String,
}

impl EvalResult {
    /// 创建通过的评估结果
    pub fn passed() -> Self {
        Self {
            passed: true,
            feedback: "评估结果-正常".to_string(),
        }
    }
}

/// v2 评估脑 — 常驻后台的监听者
///
/// 主脑每次产生输出后自动触发评估。基于记忆脑的踩坑库 + 用户画像 + 自进化规则，
/// 由 LLM 对主脑输出进行自然语言评估，结果直接作为反馈文本返回给主脑。
pub struct EvalBrain {
    llm: Arc<dyn LlmProvider>,
    progress_tx: Option<tokio::sync::mpsc::Sender<brain_core::types::ProgressEvent>>,
}

impl EvalBrain {
    /// 创建评估脑实例
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self {
            llm,
            progress_tx: None,
        }
    }

    /// 设置进度事件通道
    pub fn set_progress_tx(
        &mut self,
        tx: tokio::sync::mpsc::Sender<brain_core::types::ProgressEvent>,
    ) {
        self.progress_tx = Some(tx);
    }

    /// 评估主脑输出
    ///
    /// 调用 LLM 进行评估，LLM 返回自然语言反馈文本。
    /// 通过检测响应中是否包含"存在问题"来判断 passed/failed。
    pub async fn evaluate(
        &self,
        user_input: &str,
        ai_output: &str,
        pitfalls: &[PitfallRecord],
        user_profile: &UserProfile,
        rules: &[EvolutionRule],
    ) -> Result<EvalResult> {
        if user_input.trim().is_empty() || ai_output.trim().is_empty() {
            return Err(EvalError::InvalidInput(
                "user_input and ai_output must not be empty".into(),
            ));
        }

        // 发送评估开始事件
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(ProgressEvent::EvaluationStart);
        }

        // LLM 评估
        let feedback = self.llm_evaluate(user_input, ai_output, pitfalls, user_profile, rules).await?;

        // 判断是否通过：包含"存在问题"则不通过
        let passed = !feedback.contains("存在问题");

        self.emit_result(passed, &feedback);

        Ok(EvalResult { passed, feedback })
    }

    /// 发送评估结果进度事件
    fn emit_result(&self, passed: bool, feedback: &str) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(ProgressEvent::EvaluationResult {
                passed,
                feedback: feedback.to_string(),
            });
        }
    }

    /// 快速规则预检（不调 LLM，纯规则匹配）
    ///
    /// 检查项：
    /// 1. 偷懒模式检测（TODO/FIXME/HACK/省略号实现）
    /// 2. 禁忌词匹配
    /// 3. 已踩坑模式复现检测（基于踩坑描述的关键词匹配）
    pub fn quick_check(
        &self,
        ai_output: &str,
        pitfalls: &[PitfallRecord],
        taboos: &[String],
    ) -> Vec<EvalIssue> {
        let mut issues = Vec::new();

        // 检查 1: 偷懒模式
        issues.extend(checker::detect_lazy_behavior(ai_output));

        // 检查 2: 禁忌词匹配
        issues.extend(checker::detect_taboo_violations(ai_output, taboos));

        // 检查 3: 已踩坑模式复现
        issues.extend(checker::detect_pitfall_repeats(ai_output, pitfalls));

        issues
    }

    /// LLM 评估 — 返回自然语言反馈
    async fn llm_evaluate(
        &self,
        user_input: &str,
        ai_output: &str,
        pitfalls: &[PitfallRecord],
        user_profile: &UserProfile,
        rules: &[EvolutionRule],
    ) -> Result<String> {
        let system_prompt = prompts::build_evaluation_system_prompt();
        let user_prompt = prompts::build_evaluation_user_prompt(
            user_input,
            ai_output,
            pitfalls,
            user_profile,
            rules,
        );

        let request = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::system(system_prompt),
                ChatMessage::user(user_prompt),
            ],
            max_tokens: Some(2048),
            temperature: Some(0.1),
            tools: None,
            tool_choice: None,
        };

        let response = self
            .llm
            .complete(request)
            .await
            .map_err(|e| EvalError::LlmError(e.to_string()))?;

        let text = response.text();
        if text.trim().is_empty() {
            return Ok("评估结果-正常".to_string());
        }

        Ok(text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use super::*;
    use brain_llm::ChatResponse;

    #[test]
    fn eval_result_passed_factory() {
        let result = EvalResult::passed();
        assert!(result.passed);
        assert_eq!(result.feedback, "评估结果-正常");
    }

    #[test]
    fn issue_severity_display() {
        assert_eq!(format!("{}", IssueSeverity::Critical), "Critical");
        assert_eq!(format!("{}", IssueSeverity::Warning), "Warning");
    }

    #[test]
    fn issue_category_display() {
        assert_eq!(format!("{}", IssueCategory::PitfallRepeat), "PitfallRepeat");
        assert_eq!(format!("{}", IssueCategory::LazyBehavior), "LazyBehavior");
        assert_eq!(format!("{}", IssueCategory::FactError), "FactError");
    }

    // ── Mock LLM Provider for async integration tests ──

    struct MockLlmProvider {
        response: String,
    }

    impl MockLlmProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.into(),
            }
        }
    }

    impl LlmProvider for MockLlmProvider {
        fn model(&self) -> &'static str {
            "mock-model"
        }

        fn complete(
            &self,
            _request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            let text = self.response.clone();
            Box::pin(async move {
                Ok(ChatResponse {
                    content: vec![brain_llm::ContentBlock::text(text)],
                    model: "mock-model".into(),
                    usage: brain_llm::TokenUsage::default(),
                    finish_reason: None,
                })
            })
        }
    }

    // ── Async integration tests ──

    #[tokio::test]
    async fn evaluate_normal_response_passes() {
        let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate(
                "写一个函数",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                &[],
                &UserProfile::default(),
                &[],
            )
            .await
            .unwrap();
        assert!(result.passed);
        assert!(result.feedback.contains("评估结果-正常"));
    }

    #[tokio::test]
    async fn evaluate_detects_issues_via_llm() {
        let llm_response = "评估结果-存在问题。具体问题：1.使用 TODO 占位而非实现，需要补充完整逻辑 需要理解根据问题和要求/需求继续修改。";
        let llm = Arc::new(MockLlmProvider::new(llm_response));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate(
                "写代码",
                "fn process() {\n    // TODO: implement this\n}",
                &[],
                &UserProfile::default(),
                &[],
            )
            .await
            .unwrap();
        assert!(!result.passed);
        assert!(result.feedback.contains("存在问题"));
    }

    #[tokio::test]
    async fn evaluate_rejects_empty_input() {
        let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate("", "some output", &[], &UserProfile::default(), &[])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn evaluate_rejects_empty_output() {
        let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate("some input", "", &[], &UserProfile::default(), &[])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn evaluate_empty_llm_response_passes() {
        let llm = Arc::new(MockLlmProvider::new(""));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate(
                "写代码",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                &[],
                &UserProfile::default(),
                &[],
            )
            .await
            .unwrap();
        // LLM 返回空 = 默认通过
        assert!(result.passed);
    }

    #[tokio::test]
    async fn quick_check_returns_all_issue_types() {
        let llm = Arc::new(MockLlmProvider::new(""));
        let brain = EvalBrain::new(llm);

        let pitfall = brain_core::types::PitfallRecord {
            id: "p1".into(),
            category: brain_core::types::PitfallCategory::LazyBehavior,
            description: "使用了 unwrap 导致 panic".into(),
            user_correction: None,
            occurred_at: chrono::Utc::now(),
            occurrence_count: 2,
            superseded: false,
        };

        let issues = brain.quick_check(
            "// TODO: fix this\nuse unsafe code here",
            &[pitfall],
            &["unsafe".into()],
        );

        // 应该检测到：TODO（lazy）、unsafe（taboo）、unwrap/panic（pitfall repeat）
        assert!(!issues.is_empty());
    }
}
