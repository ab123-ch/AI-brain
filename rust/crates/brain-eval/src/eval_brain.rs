use std::sync::Arc;

use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{EvalRequirement, EvolutionRule, PitfallRecord, ProgressEvent, ToolCall, TurnRecord, UserProfile};
use brain_llm::{ChatMessage, ChatRequest, ContentBlock, LlmProvider, ToolChoice, ToolDefinition};
use serde::{Deserialize, Serialize};

use crate::checker;
use crate::error::{EvalError, Result};
use crate::extractor;
use crate::prompts;
use crate::skills::SkillRegistry;

/// 评估脑允许使用的只读工具白名单
const READ_ONLY_TOOLS: &[&str] = &["read_file", "grep_search", "glob_search", "Skill"];

/// 检查工具名是否在只读白名单中
pub(crate) fn is_read_only_tool(name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&name)
}

/// bash 只读命令白名单（前缀匹配）
const READ_ONLY_BASH_COMMANDS: &[&str] = &[
    "cargo check",
    "cargo clippy",
    "cargo test",
    "git diff",
    "git log",
    "git status",
    "ls",
    "cat",
    "head",
    "wc",
];

/// 检查 bash 命令是否在只读白名单中
pub(crate) fn is_read_only_bash_command(cmd: &str) -> bool {
    let cmd_lower = cmd.trim().to_lowercase();
    READ_ONLY_BASH_COMMANDS.iter().any(|allowed| {
        cmd_lower.starts_with(&allowed.to_lowercase())
    })
}

/// 构建评估脑专用的只读工具定义
///
/// 精简版：只暴露 LLM 需要的核心参数，避免 eval 脑误用高级参数
pub fn build_read_only_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        // Skill tool
        ToolDefinition {
            name: "Skill".into(),
            description: "加载审查技能的完整规则。可用技能：code-review, fact-check, task-completion, writing-quality。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "要加载的技能名称，如 code-review 或 fact-check"
                    }
                },
                "required": ["command"]
            }),
        },
        ToolDefinition {
            name: "read_file".into(),
            description: "读取文件内容（只读）。可以指定行范围。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "要读取的文件的绝对路径"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "从第几行开始读取（可选，默认从第一行）"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多读取多少行（可选，默认全部）"
                    }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "grep_search".into(),
            description: "在文件内容中搜索匹配正则表达式的行（只读）。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "正则表达式搜索模式"
                    },
                    "path": {
                        "type": "string",
                        "description": "搜索目录（可选，默认当前目录）"
                    },
                    "output_mode": {
                        "type": "string",
                        "description": "输出模式：content（显示匹配行）或 files_with_matches（只显示文件名）"
                    },
                    "head_limit": {
                        "type": "integer",
                        "description": "最多返回多少条结果（建议设为20以内）"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDefinition {
            name: "glob_search".into(),
            description: "按 glob 模式搜索文件路径（只读）。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "glob 模式，如 **/*.rs 或 src/**/*.ts"
                    },
                    "path": {
                        "type": "string",
                        "description": "搜索目录（可选，默认当前目录）"
                    }
                },
                "required": ["pattern"]
            }),
        },
        // bash（只读）
        ToolDefinition {
            name: "bash".into(),
            description: "执行只读 shell 命令（如 cargo check、git diff）。只允许白名单命令。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "要执行的只读命令（必须在白名单中：cargo check/clippy/test, git diff/log/status, ls, cat, head, wc）"
                    }
                },
                "required": ["command"]
            }),
        },
    ]
}

/// 截断验证工具输出，防止大结果撑爆评估脑上下文
fn truncate_verification_output(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!(
        "{truncated}\n\n[输出已截断，原始 {} 字符，保留前 {} 字符]",
        s.chars().count(),
        max_chars
    )
}

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
    tool_executor: Option<Arc<dyn ToolExecutor>>,
    progress_tx: Option<tokio::sync::mpsc::Sender<brain_core::types::ProgressEvent>>,
}

impl EvalBrain {
    /// 创建评估脑实例（纯文本评估，无工具验证）
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self {
            llm,
            tool_executor: None,
            progress_tx: None,
        }
    }

    /// 创建带工具验证能力的评估脑实例
    pub fn with_verification(
        llm: Arc<dyn LlmProvider>,
        tool_executor: Arc<dyn ToolExecutor>,
    ) -> Self {
        Self {
            llm,
            tool_executor: Some(tool_executor),
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
        eval_requirements: &[EvalRequirement],
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
        let feedback = self
            .llm_evaluate(user_input, ai_output, pitfalls, user_profile, rules, eval_requirements)
            .await?;

        // 判断是否通过：包含"存在问题"则不通过
        let passed = !feedback.contains("存在问题");

        self.emit_result(passed, &feedback);

        Ok(EvalResult { passed, feedback })
    }

    /// 带工具验证的评估
    ///
    /// 流程：
    /// 1. 提取文件变更 → 无变更时降级到纯文本评估
    /// 2. Round 1: LLM 分析，可选调用只读工具
    /// 3. 执行工具（只允许 read_only 白名单）
    /// 4. Round 2: LLM 基于工具证据出最终评估
    pub async fn evaluate_with_verification(
        &self,
        user_input: &str,
        ai_output: &str,
        turns: &[TurnRecord],
        pitfalls: &[PitfallRecord],
        user_profile: &UserProfile,
        rules: &[EvolutionRule],
        eval_requirements: &[EvalRequirement],
    ) -> Result<EvalResult> {
        // 降级条件：无 tool_executor 或无文件变更
        let file_changes = extractor::extract_file_changes(turns);
        if self.tool_executor.is_none() || file_changes.is_empty() {
            return self
                .evaluate(user_input, ai_output, pitfalls, user_profile, rules, eval_requirements)
                .await;
        }

        if user_input.trim().is_empty() || ai_output.trim().is_empty() {
            return Err(EvalError::InvalidInput(
                "user_input and ai_output must not be empty".into(),
            ));
        }

        // 发送评估开始事件
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(ProgressEvent::EvaluationStart);
        }

        let has_tools = self.tool_executor.is_some() && !file_changes.is_empty();
        let system_prompt = prompts::build_evaluation_system_prompt(eval_requirements, &SkillRegistry::new(), has_tools);
        let user_prompt = prompts::build_evaluation_user_prompt(
            user_input,
            ai_output,
            pitfalls,
            user_profile,
            rules,
            &file_changes,
        );

        // ── Round 1: 带工具定义，LLM 可选调用工具 ──
        let read_only_tools = build_read_only_tool_definitions();
        let messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
        ];
        let request = ChatRequest {
            model: None,
            messages,
            max_tokens: Some(2048),
            temperature: Some(0.1),
            tools: Some(read_only_tools),
            tool_choice: Some(ToolChoice::Auto),
        };

        let response = self
            .llm
            .complete(request)
            .await
            .map_err(|e| EvalError::LlmError(e.to_string()))?;

        // LLM 没有调用工具 → 直接解析为评估结果
        if !response.has_tool_calls() {
            let feedback = response.text();
            let passed = !feedback.contains("存在问题");
            self.emit_result(passed, &feedback);
            return Ok(EvalResult {
                passed,
                feedback: feedback.trim().to_string(),
            });
        }

        // ── 执行验证工具 ──
        let tool_executor = self.tool_executor.as_ref().unwrap();
        let mut messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
            ChatMessage::assistant_blocks(response.content.clone()),
        ];

        for tool_block in response.tool_calls() {
            if let ContentBlock::ToolUse {
                id,
                name,
                input,
            } = tool_block
            {
                // 安全校验：只允许只读工具
                if !is_read_only_tool(name) {
                    tracing::warn!("评估脑工具安全拒绝: {name}");
                    messages.push(ChatMessage::tool_result(
                        id,
                        format!("工具 {name} 不可用：评估脑只允许只读工具"),
                        true,
                    ));
                    continue;
                }

                let tool_call = ToolCall {
                    tool_name: name.clone(),
                    input: input.clone(),
                    validated: false,
                    validation_id: None,
                };

                tracing::info!("评估脑验证工具: {name}");
                let result = tool_executor.execute(&tool_call).await;

                // 截断工具输出（评估脑不需要超大输出）
                let output = truncate_verification_output(&result.output, 5000);

                messages.push(ChatMessage::tool_result(id, output, result.is_error));
            }
        }

        // ── Round 2: 带工具证据，无工具定义，出最终评估 ──
        let request = ChatRequest {
            model: None,
            messages,
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

        let feedback = response.text();
        if feedback.trim().is_empty() {
            self.emit_result(true, "评估结果-正常");
            return Ok(EvalResult::passed());
        }

        let passed = !feedback.contains("存在问题");
        self.emit_result(passed, &feedback);
        Ok(EvalResult {
            passed,
            feedback: feedback.trim().to_string(),
        })
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
        eval_requirements: &[EvalRequirement],
    ) -> Result<String> {
        let system_prompt = prompts::build_evaluation_system_prompt(eval_requirements, &SkillRegistry::new(), false);
        let user_prompt = prompts::build_evaluation_user_prompt(
            user_input,
            ai_output,
            pitfalls,
            user_profile,
            rules,
            &[],
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
            .evaluate("", "some output", &[], &UserProfile::default(), &[], &[])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn evaluate_rejects_empty_output() {
        let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
        let brain = EvalBrain::new(llm);
        let result = brain
            .evaluate("some input", "", &[], &UserProfile::default(), &[], &[])
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

    #[test]
    fn read_only_tool_whitelist() {
        assert!(is_read_only_tool("read_file"));
        assert!(is_read_only_tool("grep_search"));
        assert!(is_read_only_tool("glob_search"));
        assert!(is_read_only_tool("Skill"));
        assert!(!is_read_only_tool("edit_file"));
        assert!(!is_read_only_tool("write_file"));
        assert!(!is_read_only_tool("bash"));
    }

    #[test]
    fn build_read_only_tools_has_five() {
        let tools = build_read_only_tool_definitions();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Skill"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"grep_search"));
        assert!(names.contains(&"glob_search"));
        assert!(names.contains(&"bash"));
    }

    // ── evaluate_with_verification tests ──

    use brain_core::types::{ToolCallRecord, TurnRole};

    fn make_edit_turn(path: &str, old: &str, new: &str) -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "edit_file".into(),
                input: serde_json::json!({
                    "file_path": path,
                    "old_string": old,
                    "new_string": new,
                }),
                output: "OK".into(),
                duration_ms: 10,
                is_error: false,
            }),
            timestamp: String::new(),
        }
    }

    #[tokio::test]
    async fn evaluate_with_verification_no_changes_falls_back() {
        // 无文件变更时降级到纯文本评估
        let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
        let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
        let brain = EvalBrain::with_verification(llm, executor);

        let result = brain
            .evaluate_with_verification(
                "写代码",
                "fn add() {}",
                &[], // turns 为空，无文件变更
                &[],
                &UserProfile::default(),
                &[],
                &[],
            )
            .await
            .unwrap();
        assert!(result.passed);
    }

    #[tokio::test]
    async fn evaluate_with_verification_skips_tools_when_no_changes() {
        // 有 tool_executor 但 turns 为空 → 降级
        let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
        let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
        let brain = EvalBrain::with_verification(llm, executor);

        let result = brain
            .evaluate_with_verification(
                "闲聊",
                "你好",
                &[],
                &[],
                &UserProfile::default(),
                &[],
                &[],
            )
            .await
            .unwrap();
        assert!(result.passed);
    }

    #[tokio::test]
    async fn evaluate_with_verification_calls_tools_two_rounds() {
        // LLM Round 1 调用 read_file → Round 2 出评估
        use std::sync::atomic::{AtomicUsize, Ordering};
        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();

        struct TwoRoundLlm {
            call_count: Arc<AtomicUsize>,
        }

        impl LlmProvider for TwoRoundLlm {
            fn model(&self) -> &'static str {
                "mock"
            }

            fn complete(
                &self,
                _request: ChatRequest,
            ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>>
            {
                let count = self.call_count.clone();
                Box::pin(async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        // Round 1: 返回工具调用
                        Ok(ChatResponse {
                            content: vec![
                                ContentBlock::text("需要验证文件内容"),
                                ContentBlock::ToolUse {
                                    id: "tu_1".into(),
                                    name: "read_file".into(),
                                    input: serde_json::json!({"file_path": "/tmp/test.rs"}),
                                },
                            ],
                            model: "mock".into(),
                            usage: brain_llm::TokenUsage::default(),
                            finish_reason: Some(brain_llm::FinishReason::ToolUse),
                        })
                    } else {
                        // Round 2: 返回评估结果
                        Ok(ChatResponse {
                            content: vec![ContentBlock::text("评估结果-正常")],
                            model: "mock".into(),
                            usage: brain_llm::TokenUsage::default(),
                            finish_reason: Some(brain_llm::FinishReason::EndTurn),
                        })
                    }
                })
            }
        }

        let llm = Arc::new(TwoRoundLlm {
            call_count: count_clone,
        });
        let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
        let brain = EvalBrain::with_verification(llm, executor);

        let turns = vec![make_edit_turn("/tmp/test.rs", "old", "new")];

        let result = brain
            .evaluate_with_verification(
                "改代码",
                "已修改",
                &turns,
                &[],
                &UserProfile::default(),
                &[],
                &[],
            )
            .await
            .unwrap();

        assert!(result.passed);
        assert_eq!(call_count.load(Ordering::SeqCst), 2); // 确认两轮 LLM
    }

    #[tokio::test]
    async fn evaluate_with_verification_rejects_write_tools() {
        // LLM 尝试调用 write_file → 安全拒绝 → Round 2 仍能出评估
        use std::sync::atomic::{AtomicUsize, Ordering};
        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();

        struct TwoCallLlm {
            call_count: Arc<AtomicUsize>,
        }

        impl LlmProvider for TwoCallLlm {
            fn model(&self) -> &'static str {
                "mock"
            }

            fn complete(
                &self,
                _request: ChatRequest,
            ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
                let count = self.call_count.clone();
                Box::pin(async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        Ok(ChatResponse {
                            content: vec![
                                ContentBlock::text("写入"),
                                ContentBlock::ToolUse {
                                    id: "tu_1".into(),
                                    name: "write_file".into(),
                                    input: serde_json::json!({"file_path": "/tmp/evil.rs"}),
                                },
                            ],
                            model: "mock".into(),
                            usage: brain_llm::TokenUsage::default(),
                            finish_reason: Some(brain_llm::FinishReason::ToolUse),
                        })
                    } else {
                        Ok(ChatResponse {
                            content: vec![ContentBlock::text("评估结果-正常")],
                            model: "mock".into(),
                            usage: brain_llm::TokenUsage::default(),
                            finish_reason: Some(brain_llm::FinishReason::EndTurn),
                        })
                    }
                })
            }
        }

        let llm = Arc::new(TwoCallLlm {
            call_count: count_clone,
        });
        let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
        let brain = EvalBrain::with_verification(llm, executor);

        let turns = vec![make_edit_turn("/tmp/test.rs", "old", "new")];
        let result = brain
            .evaluate_with_verification(
                "改代码",
                "已修改",
                &turns,
                &[],
                &UserProfile::default(),
                &[],
                &[],
            )
            .await
            .unwrap();
        assert!(result.passed);
    }

    #[test]
    fn truncate_verification_output_short() {
        assert_eq!(truncate_verification_output("hello", 10), "hello");
    }

    #[test]
    fn truncate_verification_output_long() {
        let long: String = "x".repeat(100);
        let result = truncate_verification_output(&long, 10);
        assert!(result.contains("输出已截断"));
        assert!(result.starts_with("xxxxxxxxxx"));
    }
}
