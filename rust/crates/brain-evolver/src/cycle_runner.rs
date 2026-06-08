//! CycleRunner — 六阶段进化循环驱动器
//!
//! Phase 3 的核心组件，驱动 Perceive→Research→Learn→Synthesize→Register→Verify 循环。
//! 支持：
//! - 迭代重试（验证不通过时回到 Learn）
//! - Token 预算控制
//! - 超时保护
//! - 取消中断

use crate::coordinator::EvoTargetCandidate;
use crate::error::{EvolverError, Result};
use crate::evo_log::EvoPhase;
use brain_llm::provider::{ChatMessage, ChatRequest, ChatResponse, LlmProvider};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tuning knobs for a single evolution cycle.
#[derive(Clone, Debug)]
pub struct CycleConfig {
    /// Max retry iterations when verification fails.
    pub max_iterations: u32,
    /// Token budget per target (soft limit).
    pub token_budget_per_target: u64,
    /// Verification score threshold to pass (0.0–100.0).
    pub verify_threshold: f64,
    /// Max wall-clock time per target.
    pub max_duration: Duration,
}

impl Default for CycleConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            token_budget_per_target: 100_000,
            verify_threshold: 70.0,
            max_duration: Duration::from_secs(3600),
        }
    }
}

// ---------------------------------------------------------------------------
// Phase result types
// ---------------------------------------------------------------------------

/// Common metadata for any phase execution.
#[derive(Clone, Debug)]
pub struct PhaseOutput {
    pub phase: EvoPhase,
    pub summary: String,
    pub tokens_used: u64,
    pub duration_secs: u64,
}

/// Result of the Perceive phase.
#[derive(Clone, Debug)]
pub struct PerceiveResult {
    pub output: PhaseOutput,
    /// Identified knowledge/capability gaps.
    pub gaps: Vec<String>,
    /// Progress from a previous interrupted cycle (cross-night resume).
    pub previous_progress: Option<String>,
    /// Related backlog entries that motivated this target.
    pub related_backlog: Vec<String>,
}

/// Result of the Research phase.
#[derive(Clone, Debug)]
pub struct ResearchResult {
    pub output: PhaseOutput,
    /// Condensed research summary (~5K tokens).
    pub research_summary: String,
    /// Sources that were fetched.
    pub sources_used: Vec<String>,
}

/// Result of the Learn phase.
#[derive(Clone, Debug)]
pub struct LearnResult {
    pub output: PhaseOutput,
    /// Points that have been mastered.
    pub mastered_points: Vec<String>,
    /// Questions still unresolved (feed back into next iteration).
    pub unresolved_questions: Vec<String>,
}

/// A draft skill file (SKILL.md content).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillDraft {
    pub name: String,
    pub description: String,
    /// Full SKILL.md content (frontmatter + body).
    pub content: String,
    /// Keywords that trigger this skill from L4 subconscious.
    pub trigger_keywords: Vec<String>,
}

/// Result of the Synthesize phase.
#[derive(Clone, Debug)]
pub struct SynthesizeResult {
    pub output: PhaseOutput,
    pub skills: Vec<SkillDraft>,
}

/// Verification specification for a single skill.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationSpec {
    pub skill_name: String,
    pub skill_description: String,
    pub knowledge_points: Vec<String>,
}

/// Result of the Register phase.
#[derive(Clone, Debug)]
pub struct RegisterResult {
    pub output: PhaseOutput,
    /// Names of successfully registered skills.
    pub registered_skills: Vec<String>,
    /// Verification specs for the verification agent.
    pub verification_specs: Vec<VerificationSpec>,
}

/// Result of a single verification question.
#[derive(Clone, Debug)]
pub struct QuestionResult {
    pub question: String,
    pub score: f64,
    pub feedback: String,
}

/// Result of the Verify phase.
#[derive(Clone, Debug)]
pub struct VerificationResult {
    pub output: PhaseOutput,
    pub passed: bool,
    pub score: f64,
    pub question_details: Vec<QuestionResult>,
    /// Improvement suggestions (used as feedback for retry).
    pub feedback: String,
}

// ---------------------------------------------------------------------------
// Cycle result
// ---------------------------------------------------------------------------

/// Overall result of a complete evolution cycle.
#[derive(Clone, Debug)]
pub enum CycleResult {
    /// Successfully completed and verified.
    Success {
        verification: VerificationResult,
        skills_created: Vec<String>,
        total_tokens: u64,
        total_duration_secs: u64,
    },
    /// Blocked after max iterations (all retries exhausted).
    Blocked {
        feedback: String,
        total_tokens: u64,
        total_duration_secs: u64,
    },
    /// Cancelled by user or timeout.
    Cancelled {
        reason: String,
        total_tokens: u64,
        total_duration_secs: u64,
    },
}

impl CycleResult {
    pub fn total_tokens(&self) -> u64 {
        match self {
            CycleResult::Success { total_tokens, .. }
            | CycleResult::Blocked { total_tokens, .. }
            | CycleResult::Cancelled { total_tokens, .. } => *total_tokens,
        }
    }

    pub fn total_duration_secs(&self) -> u64 {
        match self {
            CycleResult::Success {
                total_duration_secs,
                ..
            }
            | CycleResult::Blocked {
                total_duration_secs,
                ..
            }
            | CycleResult::Cancelled {
                total_duration_secs,
                ..
            } => *total_duration_secs,
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, CycleResult::Success { .. })
    }
}

// ---------------------------------------------------------------------------
// CycleRunner
// ---------------------------------------------------------------------------

/// Drives the six-phase evolution cycle for a single target.
///
/// Phase flow:
/// ```text
/// Perceive → Research → Learn → Synthesize → Register → Verify
///                                              ↑               │
///                                              └── retry ←─────┘
/// ```
pub struct CycleRunner {
    llm: Arc<dyn LlmProvider>,
    config: CycleConfig,
}

impl CycleRunner {
    /// Create a new runner with the given LLM and config.
    pub fn new(llm: Arc<dyn LlmProvider>, config: CycleConfig) -> Self {
        Self { llm, config }
    }

    /// Run the full evolution cycle for a target.
    ///
    /// If verification fails, retries from Learn with feedback.
    /// Stops after `max_iterations` retries and returns `Blocked`.
    pub async fn run(&self, target: &EvoTargetCandidate) -> Result<CycleResult> {
        let start = Instant::now();
        let mut total_tokens = 0u64;
        let mut feedback = String::new();

        for iteration in 0..self.config.max_iterations {
            // Check timeout
            if start.elapsed() > self.config.max_duration {
                return Ok(CycleResult::Cancelled {
                    reason: "timeout".into(),
                    total_tokens,
                    total_duration_secs: start.elapsed().as_secs(),
                });
            }

            // Check token budget
            if total_tokens > self.config.token_budget_per_target {
                return Ok(CycleResult::Cancelled {
                    reason: "token budget exhausted".into(),
                    total_tokens,
                    total_duration_secs: start.elapsed().as_secs(),
                });
            }

            // --- Phase 1: Perceive ---
            let perceive = self.phase_perceive(target).await?;
            total_tokens += perceive.output.tokens_used;

            // --- Phase 2: Research ---
            let research = self.phase_research(&perceive).await?;
            total_tokens += research.output.tokens_used;

            // --- Phase 3: Learn ---
            let learn = if iteration > 0 && !feedback.is_empty() {
                // Inject feedback from previous verification failure
                self.phase_learn_with_feedback(&research, &feedback).await?
            } else {
                self.phase_learn(&research).await?
            };
            total_tokens += learn.output.tokens_used;

            // --- Phase 4: Synthesize ---
            let synthesize = self.phase_synthesize(&learn).await?;
            total_tokens += synthesize.output.tokens_used;

            // If no skills produced, this is a hard failure
            if synthesize.skills.is_empty() {
                feedback = "Synthesize produced no skills".into();
                continue;
            }

            // --- Phase 5: Register ---
            let register = self.phase_register(&synthesize).await?;
            total_tokens += register.output.tokens_used;

            // --- Phase 6: Verify ---
            let verification = self.phase_verify(&register).await?;
            total_tokens += verification.output.tokens_used;

            if verification.passed {
                return Ok(CycleResult::Success {
                    verification,
                    skills_created: register.registered_skills,
                    total_tokens,
                    total_duration_secs: start.elapsed().as_secs(),
                });
            }

            // Verification failed — save feedback for next iteration
            feedback = verification.feedback.clone();
        }

        // Exhausted all iterations
        Ok(CycleResult::Blocked {
            feedback,
            total_tokens,
            total_duration_secs: start.elapsed().as_secs(),
        })
    }

    // -- Phase implementations ------------------------------------------------

    /// Phase 1: Perceive — identify gaps and recall previous progress.
    ///
    /// Uses the LLM to analyze the target and identify knowledge gaps.
    pub async fn phase_perceive(&self, target: &EvoTargetCandidate) -> Result<PerceiveResult> {
        let start = Instant::now();

        let target_desc = describe_target(target);
        let prompt = format!(
            "你是一个知识分析专家。分析以下进化目标，识别需要学习的知识缺口。\n\n\
             目标: {target_desc}\n\n\
             请列出:\n\
             1. 需要掌握的关键知识点（每行一个）\n\
             2. 已有的基础（如果有）\n\
             3. 相关的待解决问题（如果有）\n\n\
             格式:\n\
             GAPS:\n- 知识点1\n- 知识点2\n\
             PROGRESS:\n已有基础描述\n\
             BACKLOG:\n- 问题1"
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        let (gaps, previous_progress, related_backlog) = parse_perceive_response(&text);

        Ok(PerceiveResult {
            output: PhaseOutput {
                phase: EvoPhase::Perceive,
                summary: text.clone(),
                tokens_used: response.usage.total_tokens,
                duration_secs: start.elapsed().as_secs(),
            },
            gaps,
            previous_progress,
            related_backlog,
        })
    }

    /// Phase 2: Research — search for information.
    ///
    /// In this initial implementation, uses the LLM to generate research notes
    /// based on the perceived gaps. MCP tools will be integrated in Task 8.
    pub async fn phase_research(&self, perceive: &PerceiveResult) -> Result<ResearchResult> {
        let start = Instant::now();

        let gaps_text = perceive.gaps.join("\n- ");
        let prompt = format!(
            "基于以下知识缺口，进行深入研究并总结:\n\n\
             缺口:\n- {gaps_text}\n\n\
             请提供:\n\
             1. 每个缺口的详细解释\n\
             2. 最佳实践和模式\n\
             3. 常见陷阱\n\
             4. 实际示例\n\n\
             SOURCES:\n- 来源1\n- 来源2\n\
             SUMMARY:\n研究总结内容"
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        let (research_summary, sources_used) = parse_research_response(&text);

        Ok(ResearchResult {
            output: PhaseOutput {
                phase: EvoPhase::Research,
                summary: research_summary.clone(),
                tokens_used: response.usage.total_tokens,
                duration_secs: start.elapsed().as_secs(),
            },
            research_summary,
            sources_used,
        })
    }

    /// Phase 3: Learn — digest and analyze research material.
    pub async fn phase_learn(&self, research: &ResearchResult) -> Result<LearnResult> {
        self.phase_learn_with_feedback(research, "").await
    }

    /// Phase 3 with feedback from a previous verification failure.
    async fn phase_learn_with_feedback(
        &self,
        research: &ResearchResult,
        feedback: &str,
    ) -> Result<LearnResult> {
        let start = Instant::now();

        let feedback_section = if feedback.is_empty() {
            String::new()
        } else {
            format!("\n上一次验证反馈（需要改进）:\n{feedback}\n")
        };

        let prompt = format!(
            "学习以下研究材料，提取关键知识:\n\n\
             研究资料:\n{}\n\
             {feedback_section}\
             请整理:\n\
             1. 已掌握的知识点\n\
             2. 仍然不清楚的问题\n\n\
             MASTERED:\n- 知识点1\n- 知识点2\n\
             UNRESOLVED:\n- 问题1",
            research.research_summary
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        let (mastered_points, unresolved_questions) = parse_learn_response(&text);

        Ok(LearnResult {
            output: PhaseOutput {
                phase: EvoPhase::Learn,
                summary: text,
                tokens_used: response.usage.total_tokens,
                duration_secs: start.elapsed().as_secs(),
            },
            mastered_points,
            unresolved_questions,
        })
    }

    /// Phase 4: Synthesize — generate SKILL.md drafts.
    pub async fn phase_synthesize(&self, learn: &LearnResult) -> Result<SynthesizeResult> {
        let start = Instant::now();

        let mastered = learn.mastered_points.join("\n- ");
        let prompt = format!(
            "基于以下已掌握的知识，生成 SKILL.md 技能文件:\n\n\
             已掌握:\n- {mastered}\n\n\
             请生成 SKILL.md 格式的内容，包含:\n\
             1. YAML frontmatter (name, description, when_to_use)\n\
             2. 知识正文\n\
             3. 示例代码（如适用）\n\
             4. 触发关键词\n\n\
             SKILL_NAME: 技能名\n\
             SKILL_DESCRIPTION: 技能描述\n\
             SKILL_CONTENT:\n技能正文\n\
             TRIGGER_KEYWORDS: 关键词1, 关键词2"
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        let skills = parse_synthesize_response(&text);

        Ok(SynthesizeResult {
            output: PhaseOutput {
                phase: EvoPhase::Synthesize,
                summary: text,
                tokens_used: response.usage.total_tokens,
                duration_secs: start.elapsed().as_secs(),
            },
            skills,
        })
    }

    /// Phase 5: Register — prepare skills for registration.
    ///
    /// In this initial implementation, returns the skill names as "registered".
    /// Actual file writing and PluginManager integration will be added in Task 11.
    pub async fn phase_register(&self, synthesize: &SynthesizeResult) -> Result<RegisterResult> {
        let start = Instant::now();

        let registered_skills: Vec<String> =
            synthesize.skills.iter().map(|s| s.name.clone()).collect();

        let verification_specs: Vec<VerificationSpec> = synthesize
            .skills
            .iter()
            .map(|s| VerificationSpec {
                skill_name: s.name.clone(),
                skill_description: s.description.clone(),
                knowledge_points: s.trigger_keywords.clone(),
            })
            .collect();

        Ok(RegisterResult {
            output: PhaseOutput {
                phase: EvoPhase::Register,
                summary: format!("Registered {} skills", registered_skills.len()),
                tokens_used: 50,
                duration_secs: start.elapsed().as_secs(),
            },
            registered_skills,
            verification_specs,
        })
    }

    /// Phase 6: Verify — verify skills using LLM.
    ///
    /// Constructs test questions and evaluates the skill quality.
    pub async fn phase_verify(&self, register: &RegisterResult) -> Result<VerificationResult> {
        let start = Instant::now();

        let specs_text = register
            .verification_specs
            .iter()
            .map(|s| {
                format!(
                    "技能: {} ({})\n知识点: {}",
                    s.skill_name,
                    s.skill_description,
                    s.knowledge_points.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "验证以下技能的质量:\n\n{specs_text}\n\n\
             请评估:\n\
             1. 知识的正确性\n\
             2. 知识的完整性\n\
             3. 实用性\n\n\
             VERDICT: PASS 或 FAIL\n\
             SCORE: 0-100\n\
             FEEDBACK:\n改进建议"
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        let (passed, score, feedback) = parse_verify_response(&text, self.config.verify_threshold);

        Ok(VerificationResult {
            output: PhaseOutput {
                phase: EvoPhase::Verify,
                summary: text,
                tokens_used: response.usage.total_tokens,
                duration_secs: start.elapsed().as_secs(),
            },
            passed,
            score,
            question_details: Vec::new(),
            feedback,
        })
    }

    // -- Helpers --------------------------------------------------------------

    async fn llm_complete(&self, prompt: String) -> brain_llm::Result<ChatResponse> {
        let request = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::system("你是智脑的进化子系统，负责学习、研究和生成技能文件。"),
                ChatMessage::user(prompt),
            ],
            max_tokens: Some(4096),
            temperature: Some(0.3),
            tools: None,
            tool_choice: None,
        };
        self.llm.complete(request).await
    }

    /// Access the config.
    pub fn config(&self) -> &CycleConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Describe a target candidate for prompt construction.
pub fn describe_target(target: &EvoTargetCandidate) -> String {
    match target {
        EvoTargetCandidate::UserTarget(t) => {
            format!(
                "用户目标: {} — {} (优先级: {})",
                t.direction, t.description, t.priority
            )
        }
        EvoTargetCandidate::BacklogEntry(e) => {
            format!("运行时问题: {} (频率: {})", e.description, e.frequency)
        }
        EvoTargetCandidate::CodeSelfCheck => "代码自检".to_string(),
        EvoTargetCandidate::CapabilityGap { domain, missing } => {
            format!("能力缺口 — {}: 缺少 {}", domain, missing.join(", "))
        }
    }
}

fn parse_perceive_response(response: &str) -> (Vec<String>, Option<String>, Vec<String>) {
    let mut gaps = Vec::new();
    let mut progress = None;
    let mut backlog = Vec::new();
    let mut section = "";

    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("GAPS:") {
            section = "gaps";
            continue;
        } else if trimmed.starts_with("PROGRESS:") {
            section = "progress";
            continue;
        } else if trimmed.starts_with("BACKLOG:") {
            section = "backlog";
            continue;
        }

        let content = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        if content.is_empty() {
            continue;
        }

        match section {
            "gaps" => gaps.push(content.to_string()),
            "progress" => progress = Some(content.to_string()),
            "backlog" => backlog.push(content.to_string()),
            _ => {}
        }
    }

    (gaps, progress, backlog)
}

fn parse_research_response(response: &str) -> (String, Vec<String>) {
    let mut sources = Vec::new();
    let mut in_sources = false;
    let mut summary_lines = Vec::new();
    let mut in_summary = false;

    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("SOURCES:") {
            in_sources = true;
            in_summary = false;
            continue;
        } else if trimmed.starts_with("SUMMARY:") {
            in_sources = false;
            in_summary = true;
            continue;
        }

        let content = trimmed.strip_prefix("- ").unwrap_or(trimmed);

        if in_sources && !content.is_empty() {
            sources.push(content.to_string());
        }
        if in_summary && !content.is_empty() {
            summary_lines.push(content.to_string());
        }
    }

    let research_summary = if summary_lines.is_empty() {
        response.to_string()
    } else {
        summary_lines.join("\n")
    };

    (research_summary, sources)
}

fn parse_learn_response(response: &str) -> (Vec<String>, Vec<String>) {
    let mut mastered = Vec::new();
    let mut unresolved = Vec::new();
    let mut section = "";

    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("MASTERED:") {
            section = "mastered";
            continue;
        } else if trimmed.starts_with("UNRESOLVED:") {
            section = "unresolved";
            continue;
        }

        let content = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        if content.is_empty() {
            continue;
        }

        match section {
            "mastered" => mastered.push(content.to_string()),
            "unresolved" => unresolved.push(content.to_string()),
            _ => {}
        }
    }

    (mastered, unresolved)
}

fn parse_synthesize_response(response: &str) -> Vec<SkillDraft> {
    let mut name = String::new();
    let mut description = String::new();
    let mut content_lines = Vec::new();
    let mut triggers = Vec::new();
    let mut in_content = false;

    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(n) = trimmed.strip_prefix("SKILL_NAME:") {
            name = n.trim().to_string();
        } else if let Some(d) = trimmed.strip_prefix("SKILL_DESCRIPTION:") {
            description = d.trim().to_string();
        } else if trimmed.starts_with("SKILL_CONTENT:") {
            in_content = true;
            continue;
        } else if let Some(kw) = trimmed.strip_prefix("TRIGGER_KEYWORDS:") {
            in_content = false;
            triggers = kw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        } else if in_content {
            content_lines.push(line.to_string());
        }
    }

    if name.is_empty() {
        return Vec::new();
    }

    vec![SkillDraft {
        name,
        description,
        content: content_lines.join("\n"),
        trigger_keywords: triggers,
    }]
}

fn parse_verify_response(response: &str, threshold: f64) -> (bool, f64, String) {
    let mut passed = false;
    let mut score = 0.0_f64;
    let mut feedback = String::new();
    let mut in_feedback = false;

    for line in response.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("VERDICT:") {
            passed = v.trim().eq_ignore_ascii_case("PASS");
        } else if let Some(s) = trimmed.strip_prefix("SCORE:") {
            score = s.trim().parse::<f64>().unwrap_or(0.0);
        } else if trimmed.starts_with("FEEDBACK:") {
            in_feedback = true;
            continue;
        } else if in_feedback {
            if !feedback.is_empty() {
                feedback.push('\n');
            }
            feedback.push_str(trimmed);
        }
    }

    // Score must also exceed threshold
    if score < threshold {
        passed = false;
    }

    (passed, score, feedback)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::{BacklogCategory, BacklogSource, Severity};
    use crate::backlog::{BacklogEntry, BacklogStatus};
    use crate::target::{EvoTarget, TargetStatus};
    use brain_llm::echo::EchoLlmProvider;
    use brain_llm::types::{ContentBlock, FinishReason, TokenUsage};
    use chrono::Utc;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // -- Response parsing tests -----------------------------------------------

    #[test]
    fn test_parse_perceive_response() {
        let response =
            "GAPS:\n- async runtime\n- pinning\nPROGRESS:\n已掌握错误处理\nBACKLOG:\n- 不理解 Pin";
        let (gaps, progress, backlog) = parse_perceive_response(response);
        assert_eq!(gaps, vec!["async runtime", "pinning"]);
        assert_eq!(progress, Some("已掌握错误处理".to_string()));
        assert_eq!(backlog, vec!["不理解 Pin"]);
    }

    #[test]
    fn test_parse_research_response() {
        let response = "SOURCES:\n- tokio.rs\n- rust-book\nSUMMARY:\n研究总结内容\n第二行";
        let (summary, sources) = parse_research_response(response);
        assert_eq!(sources, vec!["tokio.rs", "rust-book"]);
        assert!(summary.contains("研究总结内容"));
    }

    #[test]
    fn test_parse_learn_response() {
        let response = "MASTERED:\n- runtime 模型\n- 错误传播\nUNRESOLVED:\n- pinning 语义";
        let (mastered, unresolved) = parse_learn_response(response);
        assert_eq!(mastered, vec!["runtime 模型", "错误传播"]);
        assert_eq!(unresolved, vec!["pinning 语义"]);
    }

    #[test]
    fn test_parse_synthesize_response() {
        let response = "SKILL_NAME: rust-async\nSKILL_DESCRIPTION: Rust async patterns\nSKILL_CONTENT:\n# Rust Async\n\nContent here\nTRIGGER_KEYWORDS: async, tokio, future";
        let skills = parse_synthesize_response(response);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "rust-async");
        assert_eq!(skills[0].trigger_keywords, vec!["async", "tokio", "future"]);
        assert!(skills[0].content.contains("Rust Async"));
    }

    #[test]
    fn test_parse_synthesize_response_empty() {
        let skills = parse_synthesize_response("no skill data here");
        assert!(skills.is_empty());
    }

    #[test]
    fn test_parse_verify_response_pass() {
        let response = "VERDICT: PASS\nSCORE: 85\nFEEDBACK:\n很好";
        let (passed, score, feedback) = parse_verify_response(response, 70.0);
        assert!(passed);
        assert_eq!(score, 85.0);
        assert!(feedback.contains("很好"));
    }

    #[test]
    fn test_parse_verify_response_fail_score_below_threshold() {
        let response = "VERDICT: PASS\nSCORE: 50\nFEEDBACK:\n需要改进";
        let (passed, score, _) = parse_verify_response(response, 70.0);
        assert!(!passed); // Score below threshold
        assert_eq!(score, 50.0);
    }

    #[test]
    fn test_parse_verify_response_fail_verdict() {
        let response = "VERDICT: FAIL\nSCORE: 40\nFEEDBACK:\n知识不完整";
        let (passed, score, feedback) = parse_verify_response(response, 70.0);
        assert!(!passed);
        assert_eq!(score, 40.0);
        assert!(feedback.contains("知识不完整"));
    }

    #[test]
    fn test_describe_target_user() {
        let target = EvoTargetCandidate::UserTarget(EvoTarget {
            id: "tgt_001".into(),
            direction: "Rust async".into(),
            description: "掌握 async/await".into(),
            priority: 1,
            status: TargetStatus::Pending,
            checkpoints: vec![],
            created_at: Utc::now(),
            related_skills: vec![],
        });
        let desc = describe_target(&target);
        assert!(desc.contains("Rust async"));
        assert!(desc.contains("用户目标"));
    }

    #[test]
    fn test_describe_target_backlog() {
        let target = EvoTargetCandidate::BacklogEntry(BacklogEntry {
            id: "blg_001".into(),
            source: BacklogSource::Eval,
            category: BacklogCategory::KnowledgeGap,
            description: "Docker multi-stage".into(),
            severity: Severity::High,
            frequency: 5,
            status: BacklogStatus::Pending,
            created_at: Utc::now(),
            context_snapshot: None,
            resolved_at: None,
            evolution_log_id: None,
        });
        let desc = describe_target(&target);
        assert!(desc.contains("Docker multi-stage"));
        assert!(desc.contains("频率: 5"));
    }

    #[test]
    fn test_describe_target_capability_gap() {
        let target = EvoTargetCandidate::CapabilityGap {
            domain: "Rust".into(),
            missing: vec!["async".into(), "macro".into()],
        };
        let desc = describe_target(&target);
        assert!(desc.contains("能力缺口"));
        assert!(desc.contains("async, macro"));
    }

    // -- Integration tests with EchoLlmProvider --------------------------------

    fn make_target() -> EvoTargetCandidate {
        EvoTargetCandidate::UserTarget(EvoTarget {
            id: "test-target".into(),
            direction: "test direction".into(),
            description: "test description".into(),
            priority: 1,
            status: TargetStatus::Pending,
            checkpoints: vec![],
            created_at: Utc::now(),
            related_skills: vec![],
        })
    }

    #[tokio::test]
    async fn test_cycle_runner_phase_perceive() {
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let runner = CycleRunner::new(llm, CycleConfig::default());

        let result = runner.phase_perceive(&make_target()).await.unwrap();
        assert_eq!(result.output.phase, EvoPhase::Perceive);
        // Echo provider returns text, parser may or may not extract structured data
    }

    #[tokio::test]
    async fn test_cycle_runner_phase_research() {
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let runner = CycleRunner::new(llm, CycleConfig::default());

        let perceive = PerceiveResult {
            output: PhaseOutput {
                phase: EvoPhase::Perceive,
                summary: "test".into(),
                tokens_used: 100,
                duration_secs: 1,
            },
            gaps: vec!["async runtime".into()],
            previous_progress: None,
            related_backlog: vec![],
        };

        let result = runner.phase_research(&perceive).await.unwrap();
        assert_eq!(result.output.phase, EvoPhase::Research);
    }

    #[tokio::test]
    async fn test_cycle_runner_full_cycle_echo() {
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let runner = CycleRunner::new(llm, CycleConfig::default());

        let result = runner.run(&make_target()).await.unwrap();
        // With EchoLlmProvider, the cycle will run through phases
        // (may not produce valid SKILL.md, so likely Blocked)
        assert!(result.total_tokens() > 0);
        // Duration is always >= 0 for u64, just verify it's valid
        let _ = result.total_duration_secs();
    }

    // -- Configurable mock for loop logic tests --------------------------------

    /// A mock LLM provider that returns configurable responses in sequence.
    struct MockLlm {
        responses: Vec<String>,
        call_count: AtomicUsize,
    }

    impl MockLlm {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses,
                call_count: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl LlmProvider for MockLlm {
        fn model(&self) -> &str {
            "mock"
        }

        fn complete(
            &self,
            _request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            let responses = &self.responses;
            let response_text = if idx < responses.len() {
                responses[idx].clone()
            } else {
                // Default: return empty structured response
                "VERDICT: FAIL\nSCORE: 0\nFEEDBACK:\nNo more responses".into()
            };
            Box::pin(async move {
                Ok(ChatResponse {
                    content: vec![ContentBlock::text(response_text)],
                    model: "mock".into(),
                    usage: TokenUsage {
                        prompt_tokens: 100,
                        completion_tokens: 200,
                        total_tokens: 300,
                        ..Default::default()
                    },
                    finish_reason: Some(FinishReason::EndTurn),
                })
            })
        }
    }

    /// Build a sequence of mock responses that simulate a successful cycle.
    /// Each iteration: perceive(1) + research(2) + learn(3) + synthesize(4) + verify(5) = 5 LLM calls
    /// (register does NOT call LLM)
    fn success_responses() -> Vec<String> {
        vec![
            // Phase 1: Perceive
            "GAPS:\n- async runtime\n- pinning".into(),
            // Phase 2: Research
            "SOURCES:\n- tokio docs\nSUMMARY:\nResearch complete".into(),
            // Phase 3: Learn
            "MASTERED:\n- async basics\n- runtime model\nUNRESOLVED:".into(),
            // Phase 4: Synthesize
            "SKILL_NAME: rust-async-basic\nSKILL_DESCRIPTION: Rust async basics\nSKILL_CONTENT:\n# Rust Async\nContent\nTRIGGER_KEYWORDS: async, tokio".into(),
            // Phase 6: Verify (Phase 5 Register does not call LLM)
            "VERDICT: PASS\nSCORE: 85\nFEEDBACK:\nGood quality".into(),
        ]
    }

    /// Build responses that fail verification once then succeed.
    /// Each iteration runs: perceive + research + learn + synthesize + verify = 5 LLM calls
    fn fail_then_success_responses() -> Vec<String> {
        vec![
            // --- Iteration 0 ---
            "GAPS:\n- async runtime".into(), // perceive
            "SOURCES:\n- docs\nSUMMARY:\nResearch".into(), // research
            "MASTERED:\n- basics\nUNRESOLVED:\n- pinning".into(), // learn
            "SKILL_NAME: skill-1\nSKILL_DESCRIPTION: desc\nSKILL_CONTENT:\nContent\nTRIGGER_KEYWORDS: kw1".into(), // synthesize
            "VERDICT: FAIL\nSCORE: 40\nFEEDBACK:\nNeed more detail".into(), // verify FAILS
            // --- Iteration 1 (retries from top, learn uses feedback) ---
            "GAPS:\n- async runtime\n- pinning detail".into(), // perceive
            "SOURCES:\n- docs v2\nSUMMARY:\nDeep research".into(), // research
            "MASTERED:\n- basics\n- pinning\nUNRESOLVED:".into(), // learn with feedback
            "SKILL_NAME: skill-1-v2\nSKILL_DESCRIPTION: desc v2\nSKILL_CONTENT:\nBetter content\nTRIGGER_KEYWORDS: kw1, kw2".into(), // synthesize
            "VERDICT: PASS\nSCORE: 80\nFEEDBACK:\nImproved".into(), // verify PASSES
        ]
    }

    /// Build responses that always fail verification.
    fn always_fail_responses() -> Vec<String> {
        vec![
            "GAPS:\n- gap1".into(), // perceive
            "SOURCES:\n- src\nSUMMARY:\nSummary".into(), // research
            "MASTERED:\n- m1\nUNRESOLVED:".into(), // learn
            "SKILL_NAME: skill-fail\nSKILL_DESCRIPTION: desc\nSKILL_CONTENT:\nContent\nTRIGGER_KEYWORDS: kw".into(), // synthesize
            "VERDICT: FAIL\nSCORE: 30\nFEEDBACK:\nNot good enough".into(), // verify fails
        ]
    }

    #[tokio::test]
    async fn test_cycle_success_first_try() {
        let mock = Arc::new(MockLlm::new(success_responses()));
        let runner = CycleRunner::new(mock.clone(), CycleConfig::default());

        let result = runner.run(&make_target()).await.unwrap();

        assert!(result.is_success());
        if let CycleResult::Success {
            skills_created,
            verification,
            ..
        } = &result
        {
            assert_eq!(skills_created.len(), 1);
            assert!(verification.passed);
            assert!(verification.score >= 70.0);
        }
        // Should have called LLM exactly 5 times (6 phases, but register doesn't call LLM)
        assert_eq!(mock.call_count(), 5);
    }

    #[tokio::test]
    async fn test_cycle_retry_then_success() {
        let mock = Arc::new(MockLlm::new(fail_then_success_responses()));
        let config = CycleConfig {
            max_iterations: 3,
            ..CycleConfig::default()
        };
        let runner = CycleRunner::new(mock.clone(), config);

        let result = runner.run(&make_target()).await.unwrap();

        assert!(result.is_success());
        if let CycleResult::Success { skills_created, .. } = &result {
            assert_eq!(skills_created.len(), 1);
        }
        // First iteration: 5 phases (perceive+research+learn+synthesize+verify)
        // Second iteration: 4 phases (learn+synthesize+register+verify, skip perceive/research)
        // Wait... the code always runs all 6 phases on every iteration.
        // Iteration 0: perceive(1) + research(2) + learn(3) + synthesize(4) + verify(5) = 5 LLM calls (register is not LLM)
        // Iteration 1: perceive(6) + research(7) + learn_with_feedback(8) + synthesize(9) + verify(10) = 5 more
        // Total: 10 calls
        // Actually wait, register is also not an LLM call in the current implementation.
        // Let me check: perceive(1), research(2), learn(3), synthesize(4), register(no LLM), verify(5) = 5 calls per iteration
        // But we have 8 responses in fail_then_success... hmm
        // Actually in the code, phase_register doesn't call the LLM. It just prepares data.
        // And phase_learn is called via phase_learn_with_feedback when iteration > 0.
        // So iteration 0: perceive(1) + research(2) + learn(3) + synthesize(4) + verify(5) = 5 calls
        // Iteration 1: perceive(6) + research(7) + learn_with_feedback(8) + synthesize(9) + verify(10) = 5 calls
        // Total = 10 calls, but we only have 8 responses...
        // Wait, I need to recount. The fail_then_success_responses has:
        // 1. perceive (iter 0)
        // 2. research (iter 0)
        // 3. learn (iter 0)
        // 4. synthesize (iter 0)
        // 5. verify FAIL (iter 0)
        // 6. learn_with_feedback (iter 1)
        // 7. synthesize (iter 1)
        // 8. verify PASS (iter 1)
        // But the code runs perceive + research again in iteration 1! That means:
        // Iter 0: perceive(1) + research(2) + learn(3) + synthesize(4) + verify(5) = 5 calls
        // Iter 1: perceive(6) + research(7) + learn_with_feedback(8) + synthesize(9) + verify(10) = 5 calls
        // We need 10 responses but only have 8.
        // Hmm, the mock will use the default "VERDICT: FAIL\nSCORE: 0" for responses beyond the list.
        // This would cause iteration 1 perceive and research to return the wrong data.
        // Let me fix this by adding perceive and research responses for iteration 1.
        // Actually, a simpler fix: make the mock responses for iteration 1 start from index 5.
        // Or... I can change the approach: make the responses comprehensive for both iterations.
    }

    #[tokio::test]
    async fn test_cycle_blocked_after_max_iterations() {
        let mock = Arc::new(MockLlm::new(always_fail_responses()));
        let config = CycleConfig {
            max_iterations: 2,
            ..CycleConfig::default()
        };
        let runner = CycleRunner::new(mock.clone(), config);

        let result = runner.run(&make_target()).await.unwrap();

        assert!(!result.is_success());
        if let CycleResult::Blocked {
            feedback,
            total_tokens,
            ..
        } = &result
        {
            assert!(!feedback.is_empty());
            assert!(total_tokens > &0);
        }
    }

    #[test]
    fn test_cycle_config_default() {
        let config = CycleConfig::default();
        assert_eq!(config.max_iterations, 3);
        assert_eq!(config.token_budget_per_target, 100_000);
        assert_eq!(config.verify_threshold, 70.0);
        assert_eq!(config.max_duration, Duration::from_secs(3600));
    }

    #[test]
    fn test_cycle_result_accessors() {
        let result = CycleResult::Success {
            verification: VerificationResult {
                output: PhaseOutput {
                    phase: EvoPhase::Verify,
                    summary: "test".into(),
                    tokens_used: 100,
                    duration_secs: 1,
                },
                passed: true,
                score: 85.0,
                question_details: vec![],
                feedback: String::new(),
            },
            skills_created: vec!["skill-1".into()],
            total_tokens: 1000,
            total_duration_secs: 10,
        };

        assert!(result.is_success());
        assert_eq!(result.total_tokens(), 1000);
        assert_eq!(result.total_duration_secs(), 10);
    }
}
