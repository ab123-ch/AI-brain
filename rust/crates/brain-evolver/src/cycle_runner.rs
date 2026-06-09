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
use crate::memory_access::{MemoryAccess, RecallResult, StubMemoryAccess};
use crate::web_search::{SearchResult, StubWebSearch, WebSearch};
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
    /// Evolution system prompt (built by EvoPrompt five-layer generator).
    /// If empty, a minimal fallback prompt is used.
    pub system_prompt: String,
}

impl Default for CycleConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            token_budget_per_target: 100_000,
            verify_threshold: 70.0,
            max_duration: Duration::from_secs(3600),
            system_prompt: String::new(),
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
    /// Sources used in research (passed for SKILL.md references).
    pub sources_used: Vec<String>,
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
    /// Cross-phase conversation history — preserves context across all six phases.
    history: Vec<ChatMessage>,
    /// Memory access for progressive recall and writing.
    memory: Arc<dyn MemoryAccess>,
    /// Web search for MCP tool calls.
    web_search: Arc<dyn WebSearch>,
}

impl CycleRunner {
    /// Create a new runner with the given LLM, config, and memory access.
    pub fn new(llm: Arc<dyn LlmProvider>, config: CycleConfig) -> Self {
        Self::with_resources(
            llm,
            config,
            Arc::new(StubMemoryAccess),
            Arc::new(StubWebSearch),
        )
    }

    /// Create a new runner with explicit memory access.
    pub fn with_memory(
        llm: Arc<dyn LlmProvider>,
        config: CycleConfig,
        memory: Arc<dyn MemoryAccess>,
    ) -> Self {
        Self::with_resources(llm, config, memory, Arc::new(StubWebSearch))
    }

    /// Create a new runner with full resources (memory + web search).
    pub fn with_resources(
        llm: Arc<dyn LlmProvider>,
        config: CycleConfig,
        memory: Arc<dyn MemoryAccess>,
        web_search: Arc<dyn WebSearch>,
    ) -> Self {
        Self {
            llm,
            config,
            history: Vec::new(),
            memory,
            web_search,
        }
    }

    /// Update memory access (called by EvoOrchestrator when memory is initialized).
    pub fn set_memory(&mut self, memory: Arc<dyn MemoryAccess>) {
        self.memory = memory;
    }

    /// Update web search (called when MCP tools are connected).
    pub fn set_web_search(&mut self, web_search: Arc<dyn WebSearch>) {
        self.web_search = web_search;
    }

    /// Update the system prompt (called by EvoOrchestrator before each target).
    pub fn set_system_prompt(&mut self, prompt: String) {
        self.config.system_prompt = prompt;
    }

    /// Clear conversation history (called at the start of each new target cycle).
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Run the full evolution cycle for a target.
    ///
    /// If verification fails, retries from Learn with feedback.
    /// Stops after `max_iterations` retries and returns `Blocked`.
    pub async fn run(&mut self, target: &EvoTargetCandidate) -> Result<CycleResult> {
        let start = Instant::now();
        let mut total_tokens = 0u64;
        let mut feedback = String::new();

        // Clear history for this new target cycle
        self.clear_history();

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
    /// Uses memory brain progressive recall (L4→L3→L2→L1) to recover
    /// previous learning progress, then uses LLM to analyze the target.
    pub async fn phase_perceive(&mut self, target: &EvoTargetCandidate) -> Result<PerceiveResult> {
        let start = Instant::now();

        let target_desc = describe_target(target);

        // Step 1: Progressive recall (跨夜续学恢复进度)
        let recall = self
            .memory
            .progressive_recall(&target_desc)
            .map_err(|e| EvolverError::Memory(e.to_string()))?;

        // Step 2: Build context from recall
        let recall_context = build_recall_context(&recall);

        // Step 3: LLM analysis with recall context
        let prompt = format!(
            "你是一个知识分析专家。分析以下进化目标，识别需要学习的知识缺口。\n\n\
             目标: {target_desc}\n\n\
             {recall_context}\n\n\
             请列出:\n\
             1. 需要掌握的关键知识点（每行一个）\n\
             2. 已有的基础（结合召回结果）\n\
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

        let (gaps, llm_progress, related_backlog) = parse_perceive_response(&text);

        // Step 4: Merge recall progress with LLM progress
        let previous_progress = merge_progress(recall.task_summary.clone(), llm_progress);

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
    /// Uses WebSearch interface to query relevant sources,
    /// then fetches and consolidates key content.
    pub async fn phase_research(&mut self, perceive: &PerceiveResult) -> Result<ResearchResult> {
        let start = Instant::now();

        // Step 1: Generate search queries from gaps
        let search_queries = generate_search_queries(&perceive.gaps);

        // Step 2: Execute searches (use web_search interface)
        let mut all_results: Vec<SearchResult> = Vec::new();
        for query in &search_queries {
            let results = self
                .web_search
                .search(query, 5)
                .map_err(|e| EvolverError::WebSearch(e.to_string()))?;
            all_results.extend(results);
        }

        // Step 3: Fetch top pages by credibility
        let top_urls: Vec<String> = all_results
            .iter()
            .filter(|r| r.credibility >= 70)
            .take(3)
            .map(|r| r.url.clone())
            .collect();

        let pages = self
            .web_search
            .fetch_pages(&top_urls)
            .map_err(|e| EvolverError::WebSearch(e.to_string()))?;

        // Step 4: Write raw content to memory (L1)
        for page in &pages {
            self.memory.write_memory(crate::memory_access::MemoryWriteRequest {
                layer: crate::memory_access::MemoryLayer::Raw,
                content: page.content.clone(),
                source: page.url.clone(),
            }).map_err(|e| EvolverError::Memory(e.to_string()))?;
        }

        // Step 5: LLM synthesis of research results
        let pages_text = pages
            .iter()
            .map(|p| format!("## {} ({})\n{}", p.title, p.url, p.content))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let prompt = format!(
            "基于以下研究资料，提炼关键知识:\n\n\
             {pages_text}\n\n\
             请提供:\n\
             1. 核心概念解释\n\
             2. 最佳实践和模式\n\
             3. 常见陷阱\n\
             4. 实际示例\n\n\
             SOURCES:\n{}\n\
             SUMMARY:\n研究总结（~500字）",
            top_urls.join("\n- ")
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        let (research_summary, sources_used) = parse_research_response(&text);

        // Step 6: Write summary to memory (L2)
        self.memory.write_memory(crate::memory_access::MemoryWriteRequest {
            layer: crate::memory_access::MemoryLayer::Summary,
            content: research_summary.clone(),
            source: "phase_research".into(),
        }).map_err(|e| EvolverError::Memory(e.to_string()))?;

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

    /// Phase 3: Learn — digest and analyze research material with four-step analysis.
    pub async fn phase_learn(&mut self, research: &ResearchResult) -> Result<LearnResult> {
        self.phase_learn_with_feedback(research, "").await
    }

    /// Phase 3 with feedback from a previous verification failure.
    ///
    /// Task 9 implementation: Four-step analysis + memory integration.
    async fn phase_learn_with_feedback(
        &mut self,
        research: &ResearchResult,
        feedback: &str,
    ) -> Result<LearnResult> {
        let start = Instant::now();

        // Step 1: Recall previous learning progress from memory
        let recall = self
            .memory
            .progressive_recall(&research.research_summary)
            .map_err(|e| EvolverError::Memory(e.to_string()))?;

        // Build previous progress section
        let previous_section = build_previous_progress_section(&recall);

        // Build feedback section
        let feedback_section = if feedback.is_empty() {
            String::new()
        } else {
            format!("\n## 上一次验证反馈（需要改进）\n{feedback}\n")
        };

        // Step 2: Four-step analysis prompt (inspired by brain-memory concentration engine)
        let prompt = format!(
            "## 身份\n\
             你是一个知识消化引擎。你将研究材料转化为结构化的学习成果，\
             同时维护一份渐进式的学习进度记录。\n\n\
             ## 输入\n\
             ### 研究资料\n\
             {research_summary}\n\n\
             {previous_section}\
             {feedback_section}\
             ## 分析规则（四步分析）\n\n\
             ### Step 1: 事实总结\n\
             - 从研究资料中提取客观事实和技术要点\n\
             - 保留所有关键细节：概念定义、代码片段、最佳实践\n\
             - 用中文输出，控制在 200 字以内\n\n\
             ### Step 2: 已掌握知识点\n\
             - 列出本次学习后理解的核心概念\n\
             - 每条用简洁的一句话描述\n\
             - 标注是否为新增掌握（vs 上次已掌握）\n\n\
             ### Step 3: 仍然不清楚的问题\n\
             - 列出仍有疑问的点，用于后续研究\n\
             - 每条用具体的问题形式描述\n\n\
             ### Step 4: 经验提炼\n\
             - 提炼 1-3 条可复用的经验规则\n\
             - 格式：\"当 [场景] 时，应该 [行为]\"\n\n\
             ## 输出格式（严格 JSON）\n\
             ```json\n\
             {{\n\
               \"fact_summary\": \"事实总结内容\",\n\
               \"mastered\": [\n\
                 \"知识点1（新增）\",\n\
                 \"知识点2\"\n\
               ],\n\
               \"unresolved\": [\n\
                 \"问题1\",\n\
                 \"问题2\"\n\
               ],\n\
               \"experience_rules\": [\n\
                 \"当遇到 X 时，应该 Y\"\n\
               ]\n\
             }}\n\
             ```",
            research_summary = research.research_summary,
            previous_section = previous_section,
            feedback_section = feedback_section,
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        // Step 3: Parse structured response
        let parsed = parse_learn_json_response(&text);

        // Step 4: Write learning results to memory (L2 Summary + L3 Abstract)
        // Write fact summary to L2
        self.memory
            .write_memory(crate::memory_access::MemoryWriteRequest {
                layer: crate::memory_access::MemoryLayer::Summary,
                content: parsed.fact_summary.clone(),
                source: format!("phase_learn_{}", self.config.verify_threshold),
            })
            .map_err(|e| EvolverError::Memory(e.to_string()))?;

        // Write experience rules to L3
        for rule in &parsed.experience_rules {
            self.memory
                .write_memory(crate::memory_access::MemoryWriteRequest {
                    layer: crate::memory_access::MemoryLayer::Abstract,
                    content: rule.clone(),
                    source: "phase_learn_experience".into(),
                })
                .map_err(|e| EvolverError::Memory(e.to_string()))?;
        }

        Ok(LearnResult {
            output: PhaseOutput {
                phase: EvoPhase::Learn,
                summary: text,
                tokens_used: response.usage.total_tokens,
                duration_secs: start.elapsed().as_secs(),
            },
            mastered_points: parsed.mastered,
            unresolved_questions: parsed.unresolved,
            sources_used: research.sources_used.clone(),
        })
    }

    /// Phase 4: Synthesize — generate SKILL.md drafts with references and triggers.
    ///
    /// Task 10 implementation: Full SKILL.md generation with:
    /// - YAML frontmatter (name, description, when_to_use, bootstrap)
    /// - Knowledge body + examples + references
    /// - Trigger keywords written to L4
    pub async fn phase_synthesize(&mut self, learn: &LearnResult) -> Result<SynthesizeResult> {
        let start = Instant::now();

        // Build mastered points section
        let mastered = learn.mastered_points.join("\n- ");

        // Build references section
        let references = if learn.sources_used.is_empty() {
            String::new()
        } else {
            format!(
                "\n## 参考来源\n{}\n",
                learn.sources_used.iter().map(|s| format!("- {}", s)).collect::<Vec<_>>().join("\n")
            )
        };

        // Build unresolved section (for skill limitation notes)
        let unresolved_section = if learn.unresolved_questions.is_empty() {
            String::new()
        } else {
            format!(
                "\n## 尚未掌握\n{}\n",
                learn.unresolved_questions.iter().map(|q| format!("- {}", q)).collect::<Vec<_>>().join("\n")
            )
        };

        let prompt = format!(
            "## 身份\n\
             你是一个技能文件生成引擎。你将学习成果转化为结构化的 SKILL.md 格式。\n\n\
             ## 输入\n\
             ### 已掌握的知识\n\
             - {mastered}\n\
             {references}\
             {unresolved_section}\
             ## 输出要求\n\
             生成一个完整的 SKILL.md 文件，包含:\n\n\
             ### YAML Frontmatter\n\
             ```yaml\n\
             name: 技能名称（英文小写-分隔）\n\
             description: 简短描述（20字以内）\n\
             when_to_use: 使用场景（逗号分隔的关键词）\n\
             bootstrap: 引导步骤（可选，如有必要）\n\
             ```\n\n\
             ### Markdown Body\n\
             1. 核心知识（2-5 个要点）\n\
             2. 代码示例（如适用，使用代码块）\n\
             3. 最佳实践/注意事项\n\
             4. 参考来源（从输入中复制）\n\n\
             ### 触发关键词\n\
             提取 3-5 个能触发此技能召回的关键词\n\n\
             ## 输出格式（严格 JSON）\n\
             ```json\n\
             {{\n\
               \"skills\": [\n\
                 {{\n\
                   \"name\": \"skill-name\",\n\
                   \"description\": \"技能描述\",\n\
                   \"content\": \"---\\nname: skill-name\\ndescription: ...\\nwhen_to_use: ...\\n---\\n\\n# 知识标题\\n\\n内容...\",\n\
                   \"trigger_keywords\": [\"关键词1\", \"关键词2\"]\n\
                 }}\n\
               ]\n\
             }}\n\
             ```\n\n\
             注意:\n\
             - 一个学习阶段可能生成多个技能（如果知识覆盖多个领域）\n\
             - 技能名称使用英文小写+连字符\n\
             - content 字段必须是完整的 SKILL.md 格式（含 frontmatter）",
            mastered = mastered,
            references = references,
            unresolved_section = unresolved_section,
        );

        let response = self
            .llm_complete(prompt)
            .await
            .map_err(|e| EvolverError::Llm(e.to_string()))?;
        let text = response.text();

        // Parse structured response
        let skills = parse_synthesize_json_response(&text);

        // Write trigger keywords to L4 (subconscious)
        for skill in &skills {
            for keyword in &skill.trigger_keywords {
                self.memory
                    .write_memory(crate::memory_access::MemoryWriteRequest {
                        layer: crate::memory_access::MemoryLayer::Subconscious,
                        content: format!("{} -> {}", keyword, skill.name),
                        source: "phase_synthesize_trigger".into(),
                    })
                    .map_err(|e| EvolverError::Memory(e.to_string()))?;
            }
        }

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
    pub async fn phase_register(&mut self, synthesize: &SynthesizeResult) -> Result<RegisterResult> {
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
    pub async fn phase_verify(&mut self, register: &RegisterResult) -> Result<VerificationResult> {
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

    async fn llm_complete(&mut self, prompt: String) -> brain_llm::Result<ChatResponse> {
        // Append user message to history
        self.history.push(ChatMessage::user(&prompt));

        // Build messages: system prompt + full history
        let system_text = if self.config.system_prompt.is_empty() {
            "你是智脑的进化子系统，负责学习、研究和生成技能文件。".to_string()
        } else {
            self.config.system_prompt.clone()
        };
        let mut messages = vec![ChatMessage::system(&system_text)];
        messages.extend(self.history.iter().cloned());

        let request = ChatRequest {
            model: None,
            messages,
            max_tokens: Some(4096),
            temperature: Some(0.3),
            tools: None,
            tool_choice: None,
        };
        let response = self.llm.complete(request).await;

        // Append assistant response to history (preserve cross-phase context)
        if let Ok(ref resp) = response {
            self.history.push(ChatMessage::assistant(resp.text()));
        }

        response
    }

    /// Access the config.
    pub fn config(&self) -> &CycleConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Generate search queries from knowledge gaps.
fn generate_search_queries(gaps: &[String]) -> Vec<String> {
    // Simple strategy: use each gap as a query, add domain context
    gaps.iter()
        .map(|g| format!("{} 教程 最佳实践", g))
        .collect()
}

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

// -- Recall context helpers -------------------------------------------------

/// Build context string from recall results for LLM prompt injection.
fn build_recall_context(recall: &RecallResult) -> String {
    let mut parts = Vec::new();

    if !recall.trigger_matches.is_empty() {
        parts.push(format!(
            "[上次学习触发词] {}\n（这些关键词表明你之前已学习过相关内容）",
            recall.trigger_matches.join(", ")
        ));
    }

    if let Some(ref exp) = recall.experience_summary {
        parts.push(format!("[已抽象的经验]\n{exp}"));
    }

    if let Some(ref summary) = recall.task_summary {
        parts.push(format!("[上次学习进度]\n{summary}"));
    }

    if !recall.related_pitfalls.is_empty() {
        parts.push(format!(
            "[相关踩坑记录]\n{}\n（这些是之前遇到的问题，学习时需避免重犯）",
            recall.related_pitfalls.join("\n")
        ));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("[记忆脑召回 — 跨夜续学上下文]\n{}", parts.join("\n\n"))
    }
}

/// Merge memory recall progress with LLM-analyzed progress.
fn merge_progress(recall_progress: Option<String>, llm_progress: Option<String>) -> Option<String> {
    match (recall_progress, llm_progress) {
        (Some(r), Some(l)) => Some(format!("{r}\n\n[本次分析补充]\n{l}")),
        (Some(r), None) => Some(r),
        (None, Some(l)) => Some(l),
        (None, None) => None,
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

/// Build previous progress section from recall result.
fn build_previous_progress_section(recall: &RecallResult) -> String {
    let mut sections = Vec::new();

    // L4 trigger matches
    if !recall.trigger_matches.is_empty() {
        sections.push(format!(
            "### L4 触发词匹配\n- {}",
            recall.trigger_matches.join("\n- ")
        ));
    }

    // L3 experience summary
    if let Some(ref exp) = recall.experience_summary {
        sections.push(format!("### L3 经验摘要\n{}", exp));
    }

    // L2 task summary (previous learning progress)
    if let Some(ref task) = recall.task_summary {
        sections.push(format!("### L2 上次学习进度\n{}", task));
    }

    // Related pitfalls
    if !recall.related_pitfalls.is_empty() {
        sections.push(format!(
            "### 相关踩坑记录\n- {}",
            recall.related_pitfalls.join("\n- ")
        ));
    }

    if sections.is_empty() {
        String::new()
    } else {
        format!("## 上次学习进度（渐进式召回）\n{}\n\n", sections.join("\n\n"))
    }
}

/// Parsed learn JSON response.
struct LearnJsonResponse {
    fact_summary: String,
    mastered: Vec<String>,
    unresolved: Vec<String>,
    experience_rules: Vec<String>,
}

/// Parse JSON-formatted learn response (Task 9 format).
fn parse_learn_json_response(response: &str) -> LearnJsonResponse {
    // Extract JSON from response (handle markdown code blocks)
    let json_str = extract_json_from_response(response);

    // Parse JSON
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
        let fact_summary = parsed
            .get("fact_summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mastered = parsed
            .get("mastered")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let unresolved = parsed
            .get("unresolved")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let experience_rules = parsed
            .get("experience_rules")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        LearnJsonResponse {
            fact_summary,
            mastered,
            unresolved,
            experience_rules,
        }
    } else {
        // Fallback to legacy parsing if JSON fails
        let (mastered, unresolved) = parse_learn_response(response);
        LearnJsonResponse {
            fact_summary: response.to_string(),
            mastered,
            unresolved,
            experience_rules: Vec::new(),
        }
    }
}

/// Extract JSON string from response (handle markdown code blocks).
fn extract_json_from_response(response: &str) -> &str {
    let trimmed = response.trim();

    // Try to extract ```json ... ```
    if let Some(start) = trimmed.find("```json") {
        let json_start = start + 7;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim();
        }
    }

    // Try to extract ``` ... ```
    if let Some(start) = trimmed.find("```") {
        let json_start = start + 3;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim();
        }
    }

    // Return raw response
    trimmed
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

/// Parse JSON-formatted synthesize response (Task 10 format).
fn parse_synthesize_json_response(response: &str) -> Vec<SkillDraft> {
    // Extract JSON from response (handle markdown code blocks)
    let json_str = extract_json_from_response(response);

    // Parse JSON
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
        let skills: Vec<SkillDraft> = parsed
            .get("skills")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let name = v.get("name").and_then(|n| n.as_str())?;
                        let description = v
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string();
                        let content = v
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        let trigger_keywords = v
                            .get("trigger_keywords")
                            .and_then(|t| t.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|k| k.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();

                        Some(SkillDraft {
                            name: name.to_string(),
                            description,
                            content,
                            trigger_keywords,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        if !skills.is_empty() {
            return skills;
        }
    }

    // Fallback to legacy parsing if JSON fails
    parse_synthesize_response(response)
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
        let mut runner = CycleRunner::new(llm, CycleConfig::default());

        let result = runner.phase_perceive(&make_target()).await.unwrap();
        assert_eq!(result.output.phase, EvoPhase::Perceive);
        // Echo provider returns text, parser may or may not extract structured data
    }

    #[tokio::test]
    async fn test_cycle_runner_phase_research() {
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let mut runner = CycleRunner::new(llm, CycleConfig::default());

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
        let mut runner = CycleRunner::new(llm, CycleConfig::default());

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
        let mut runner = CycleRunner::new(mock.clone(), CycleConfig::default());

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
        let mut runner = CycleRunner::new(mock.clone(), config);

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
        let mut runner = CycleRunner::new(mock.clone(), config);

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
