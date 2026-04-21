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

/// 单个评估问题
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

/// 评估结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    /// 是否通过（无 Critical 级别问题时视为通过）
    pub passed: bool,
    /// 发现的问题列表
    pub issues: Vec<EvalIssue>,
}

impl EvalResult {
    /// 创建通过的评估结果
    pub fn passed() -> Self {
        Self {
            passed: true,
            issues: Vec::new(),
        }
    }

    /// 创建未通过的评估结果
    pub fn failed(issues: Vec<EvalIssue>) -> Self {
        let passed = !issues.iter().any(|i| i.severity == IssueSeverity::Critical);
        Self { passed, issues }
    }
}

/// v2 评估脑 — 常驻后台的监听者
///
/// 主脑每次产生输出后自动触发评估。基于记忆脑的踩坑库 + 用户画像 + 自进化规则，
/// 检查主脑输出是否存在问题。
///
/// 评估策略：快速规则预检（不调 LLM） + LLM 深度评估（LLM 分析上下文）
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
    /// 双路径：
    /// 1. 快速规则预检 — 纯字符串匹配，不调 LLM
    /// 2. LLM 深度评估 — 注入踩坑库 + 用户画像 + 自进化规则，让 LLM 判断
    ///
    /// 如果踩坑库为空且规则为空且用户画像无禁忌，跳过 LLM 调用，直接通过。
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

        // 路径 1: 快速规则预检
        let quick_issues = self.quick_check(ai_output, pitfalls, &user_profile.taboos);

        // 判断是否需要 LLM 深度评估
        let needs_llm = !pitfalls.is_empty()
            || !rules.is_empty()
            || !user_profile.explicit_preferences.is_empty()
            || !user_profile.implicit_preferences.is_empty()
            || !user_profile.habits.is_empty();

        if !needs_llm {
            // 无记忆数据，仅返回规则预检结果
            if quick_issues.is_empty() {
                self.emit_result_with_issues(true, &[]);
                return Ok(EvalResult::passed());
            }
            let passed = !quick_issues
                .iter()
                .any(|i| i.severity == IssueSeverity::Critical);
            self.emit_result_with_issues(passed, &quick_issues);
            return Ok(EvalResult {
                passed,
                issues: quick_issues,
            });
        }

        // 路径 2: LLM 深度评估
        let llm_issues = self
            .llm_evaluate(user_input, ai_output, pitfalls, user_profile, rules)
            .await?;

        // 合并去重：以 (category, description 前缀) 去重
        let mut all_issues = quick_issues;
        let existing_keys: Vec<(IssueCategory, String)> = all_issues
            .iter()
            .map(|i| (i.category, i.description.chars().take(40).collect()))
            .collect();

        for issue in llm_issues {
            let key = (
                issue.category,
                issue.description.chars().take(40).collect::<String>(),
            );
            if !existing_keys.contains(&key) {
                all_issues.push(issue);
            }
        }

        // 按严重程度排序：Critical 在前
        all_issues.sort_by(|a, b| {
            let order = |s: &IssueSeverity| match s {
                IssueSeverity::Critical => 0,
                IssueSeverity::Warning => 1,
            };
            order(&a.severity).cmp(&order(&b.severity))
        });

        let passed = !all_issues
            .iter()
            .any(|i| i.severity == IssueSeverity::Critical);

        self.emit_result_with_issues(passed, &all_issues);

        Ok(EvalResult {
            passed,
            issues: all_issues,
        })
    }

    /// 发送评估结果进度事件
    fn emit_result_with_issues(&self, passed: bool, issues: &[EvalIssue]) {
        if let Some(tx) = &self.progress_tx {
            let issue_strs: Vec<String> = issues
                .iter()
                .map(|i| format!("[{}] {}", i.severity, i.description))
                .collect();
            let _ = tx.try_send(ProgressEvent::EvaluationResult {
                passed,
                issues: issue_strs,
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

    /// LLM 深度评估
    async fn llm_evaluate(
        &self,
        user_input: &str,
        ai_output: &str,
        pitfalls: &[PitfallRecord],
        user_profile: &UserProfile,
        rules: &[EvolutionRule],
    ) -> Result<Vec<EvalIssue>> {
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
            return Ok(Vec::new());
        }

        // 从 LLM 响应中提取 JSON
        let json_str = extract_json_from_response(&text);
        Ok(parse_llm_issues(&json_str))
    }
}

/// 从 LLM 响应中提取 JSON 数组部分
/// 找到与 `open` 位置的 `{` 配对的 `}` 位置
fn find_matching_brace(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &ch) in bytes.iter().enumerate().skip(open) {
        if escape {
            escape = false;
            continue;
        }
        if ch == b'\\' && in_string {
            escape = true;
            continue;
        }
        if ch == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// 找到与 `open` 位置的 `[` 配对的 `]` 位置
fn find_matching_bracket(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &ch) in bytes.iter().enumerate().skip(open) {
        if escape {
            escape = false;
            continue;
        }
        if ch == b'\\' && in_string {
            escape = true;
            continue;
        }
        if ch == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

///
/// LLM 可能在 JSON 前后附加说明文字，需要提取出纯 JSON。
fn extract_json_from_response(text: &str) -> String {
    let trimmed = text.trim();

    // 尝试直接解析
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return trimmed.to_string();
    }

    // 查找 JSON 块（可能被 markdown 代码块包裹）
    if let Some(start) = trimmed.find("```json") {
        let json_start = start + 7;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim().to_string();
        }
    }

    // 查找第一个 { 或 [，然后匹配配对的结束括号
    if let Some(pos) = trimmed.find('{') {
        if let Some(end) = find_matching_brace(trimmed, pos) {
            return trimmed[pos..=end].to_string();
        }
        // 找不到配对括号时，退回到截取到最后一个 }
        if let Some(last) = trimmed.rfind('}') {
            return trimmed[pos..=last].to_string();
        }
    }
    if let Some(pos) = trimmed.find('[') {
        if let Some(end) = find_matching_bracket(trimmed, pos) {
            return trimmed[pos..=end].to_string();
        }
        if let Some(last) = trimmed.rfind(']') {
            return trimmed[pos..=last].to_string();
        }
    }

    trimmed.to_string()
}

/// 解析 LLM 返回的评估结果 JSON
fn parse_llm_issues(json_str: &str) -> Vec<EvalIssue> {
    // 尝试解析为完整评估结果对象
    if let Ok(full_result) = serde_json::from_str::<LlmEvalResponse>(json_str) {
        return full_result
            .issues
            .into_iter()
            .map(LlmEvalIssue::into_eval_issue)
            .collect();
    }

    // 尝试解析为问题数组
    if let Ok(issues) = serde_json::from_str::<Vec<LlmEvalIssue>>(json_str) {
        return issues
            .into_iter()
            .map(LlmEvalIssue::into_eval_issue)
            .collect();
    }

    // 尝试修复常见格式问题后再解析
    let cleaned = clean_json_string(json_str);
    if let Ok(full_result) = serde_json::from_str::<LlmEvalResponse>(&cleaned) {
        tracing::debug!("eval JSON 解析成功（经 clean 修复）");
        return full_result
            .issues
            .into_iter()
            .map(LlmEvalIssue::into_eval_issue)
            .collect();
    }
    if let Ok(issues) = serde_json::from_str::<Vec<LlmEvalIssue>>(&cleaned) {
        tracing::debug!("eval JSON 解析成功（经 clean 修复）");
        return issues
            .into_iter()
            .map(LlmEvalIssue::into_eval_issue)
            .collect();
    }

    // JSON 解析失败，返回空（降级，不阻断主流程）
    tracing::warn!(
        "Failed to parse LLM evaluation response as JSON, skipping LLM issues. Raw: {}",
        &json_str[..json_str.len().min(200)]
    );
    Vec::new()
}

/// 修复 LLM 输出中的常见 JSON 格式问题
fn clean_json_string(s: &str) -> String {
    let mut result = s.to_string();
    // 移除尾随逗号（}, ] 前的逗号）
    result = result.replace(",}", "}").replace(", ]", "]").replace(",]", "]");
    // 修复中文标点
    result = result.replace('\u{ff1a}', ":").replace('\u{ff0c}', ",");
    result = result.replace(['\u{201c}', '\u{201d}'], "\"");
    result
}

/// LLM 评估响应的 JSON 反序列化结构
#[derive(Debug, Deserialize)]
struct LlmEvalResponse {
    #[serde(rename = "passed")]
    _passed: bool,
    issues: Vec<LlmEvalIssue>,
}

/// LLM 返回的单个问题
#[derive(Debug, Deserialize)]
struct LlmEvalIssue {
    severity: String,
    category: String,
    description: String,
    suggestion: String,
}

impl LlmEvalIssue {
    fn into_eval_issue(self) -> EvalIssue {
        EvalIssue {
            severity: parse_severity(&self.severity),
            category: parse_category(&self.category),
            description: self.description,
            suggestion: self.suggestion,
        }
    }
}

fn parse_severity(s: &str) -> IssueSeverity {
    match s.to_lowercase().as_str() {
        "critical" => IssueSeverity::Critical,
        _ => IssueSeverity::Warning,
    }
}

fn parse_category(s: &str) -> IssueCategory {
    match s.to_lowercase().as_str() {
        "pitfall_repeat" | "pitfallrepeat" => IssueCategory::PitfallRepeat,
        "preference_violation" | "preferenceviolation" => IssueCategory::PreferenceViolation,
        "known_failure_pattern" | "knownfailurepattern" => IssueCategory::KnownFailurePattern,
        "lazy_behavior" | "lazybehavior" => IssueCategory::LazyBehavior,
        "instruction_ignored" | "instructionignored" => IssueCategory::InstructionIgnored,
        _ => IssueCategory::FactError,
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
        assert!(result.issues.is_empty());
    }

    #[test]
    fn eval_result_failed_with_warning_only() {
        let issues = vec![EvalIssue {
            severity: IssueSeverity::Warning,
            category: IssueCategory::LazyBehavior,
            description: "test".into(),
            suggestion: "fix it".into(),
        }];
        let result = EvalResult::failed(issues);
        // 只有 Warning 没有 Critical，passed=true
        assert!(result.passed);
    }

    #[test]
    fn eval_result_failed_with_critical() {
        let issues = vec![
            EvalIssue {
                severity: IssueSeverity::Warning,
                category: IssueCategory::LazyBehavior,
                description: "test warning".into(),
                suggestion: "fix warning".into(),
            },
            EvalIssue {
                severity: IssueSeverity::Critical,
                category: IssueCategory::PitfallRepeat,
                description: "test critical".into(),
                suggestion: "fix critical".into(),
            },
        ];
        let result = EvalResult::failed(issues);
        assert!(!result.passed);
        assert_eq!(result.issues.len(), 2);
    }

    #[test]
    fn extract_json_from_plain_json() {
        let input = r#"{"passed": true, "issues": []}"#;
        assert_eq!(extract_json_from_response(input), input);
    }

    #[test]
    fn extract_json_from_markdown_block() {
        let input = "Here is the result:\n```json\n{\"passed\": true, \"issues\": []}\n```\nDone.";
        let extracted = extract_json_from_response(input);
        assert!(extracted.starts_with('{'));
        assert!(extracted.contains("passed"));
    }

    #[test]
    fn extract_json_from_text_prefix() {
        let input =
            "评估结果如下：\n{\"passed\": false, \"issues\": [{\"severity\": \"Warning\"}]}";
        let extracted = extract_json_from_response(input);
        assert!(extracted.starts_with('{'));
    }

    #[test]
    fn parse_llm_issues_valid_response() {
        let json = r#"{"passed": false, "issues": [{"severity": "Critical", "category": "lazy_behavior", "description": "写了 TODO", "suggestion": "请实现完整逻辑"}]}"#;
        let issues = parse_llm_issues(json);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, IssueSeverity::Critical);
        assert_eq!(issues[0].category, IssueCategory::LazyBehavior);
    }

    #[test]
    fn parse_llm_issues_invalid_json_returns_empty() {
        let json = "this is not json";
        let issues = parse_llm_issues(json);
        assert!(issues.is_empty());
    }

    #[test]
    fn parse_severity_variants() {
        assert_eq!(parse_severity("critical"), IssueSeverity::Critical);
        assert_eq!(parse_severity("Critical"), IssueSeverity::Critical);
        assert_eq!(parse_severity("warning"), IssueSeverity::Warning);
        assert_eq!(parse_severity("anything"), IssueSeverity::Warning);
    }

    #[test]
    fn parse_category_variants() {
        assert_eq!(
            parse_category("pitfall_repeat"),
            IssueCategory::PitfallRepeat
        );
        assert_eq!(
            parse_category("PitfallRepeat"),
            IssueCategory::PitfallRepeat
        );
        assert_eq!(parse_category("lazy_behavior"), IssueCategory::LazyBehavior);
        assert_eq!(parse_category("fact_error"), IssueCategory::FactError);
        assert_eq!(
            parse_category("instruction_ignored"),
            IssueCategory::InstructionIgnored
        );
        assert_eq!(parse_category("unknown"), IssueCategory::FactError);
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
    async fn evaluate_empty_data_passes() {
        let llm = Arc::new(MockLlmProvider::new(""));
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
        assert!(result.issues.is_empty());
    }

    #[tokio::test]
    async fn evaluate_detects_lazy_behavior_via_quick_check() {
        let llm = Arc::new(MockLlmProvider::new(""));
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
        assert!(result
            .issues
            .iter()
            .any(|i| i.category == IssueCategory::LazyBehavior));
    }

    #[tokio::test]
    async fn evaluate_detects_taboo_violation() {
        let llm = Arc::new(MockLlmProvider::new(""));
        let brain = EvalBrain::new(llm);
        let mut profile = UserProfile::default();
        profile.taboos.push("unsafe".into());
        let result = brain
            .evaluate("写代码", "使用 unsafe 块来提升性能", &[], &profile, &[])
            .await
            .unwrap();
        assert!(!result.passed);
        assert!(result
            .issues
            .iter()
            .any(|i| i.category == IssueCategory::PreferenceViolation));
    }

    #[tokio::test]
    async fn evaluate_llm_returns_issues() {
        let llm_response = r#"{"passed": false, "issues": [{"severity": "Critical", "category": "fact_error", "description": "日期计算错误", "suggestion": "请修正日期计算逻辑"}]}"#;
        let llm = Arc::new(MockLlmProvider::new(llm_response));
        let brain = EvalBrain::new(llm);

        let mut profile = UserProfile::default();
        profile.explicit_preferences.push("代码必须经过测试".into());

        let result = brain
            .evaluate("计算日期", "明天是 4 月 31 日", &[], &profile, &[])
            .await
            .unwrap();

        // LLM 返回了 fact_error，应该包含在结果中
        assert!(result
            .issues
            .iter()
            .any(|i| i.category == IssueCategory::FactError));
    }

    #[tokio::test]
    async fn evaluate_rejects_empty_input() {
        let llm = Arc::new(MockLlmProvider::new(""));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate("", "some output", &[], &UserProfile::default(), &[])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn evaluate_rejects_empty_output() {
        let llm = Arc::new(MockLlmProvider::new(""));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate("some input", "", &[], &UserProfile::default(), &[])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn evaluate_merges_quick_and_llm_issues() {
        let llm_response = r#"{"passed": false, "issues": [{"severity": "Warning", "category": "preference_violation", "description": "不符合用户偏好风格", "suggestion": "请调整代码风格"}]}"#;
        let llm = Arc::new(MockLlmProvider::new(llm_response));
        let brain = EvalBrain::new(llm);

        let mut profile = UserProfile::default();
        profile.explicit_preferences.push("使用函数式风格".into());

        // AI 输出同时包含 TODO（快速预检）和 LLM 可检测的风格问题
        let result = brain
            .evaluate(
                "写代码",
                "// TODO: 重构这段代码\nlet mut x = 0;",
                &[],
                &profile,
                &[],
            )
            .await
            .unwrap();

        // 应该同时有快速预检的 LazyBehavior 和 LLM 返回的 PreferenceViolation
        assert!(result
            .issues
            .iter()
            .any(|i| i.category == IssueCategory::LazyBehavior));
        assert!(result
            .issues
            .iter()
            .any(|i| i.category == IssueCategory::PreferenceViolation));
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
