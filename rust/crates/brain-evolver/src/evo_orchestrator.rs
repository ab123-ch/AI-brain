//! EvoOrchestrator — 进化脑独立编排器
//!
//! 独立于主 Orchestrator 的进化脑运行时。包含：
//! - 独立 LLM-B 实例（与主脑 LLM-A 完全隔离）
//! - 独立 ConversationHistory
//! - 独立 System Prompt (进化专用)
//! - 共享只读: MCP Pool / SkillCatalog
//! - 共享读写: 记忆存储 / SkillWriter
//!
//! 由主 Orchestrator 的 `spawn_evolution()` 创建，tokio::spawn 后台运行，
//! 完成后自动销毁释放资源。

use crate::coordinator::{EvoConfig, EvoTargetCandidate, EvolutionCoordinator};
use crate::cycle_runner::{CycleConfig, CycleResult, CycleRunner};
use crate::error::{EvolverError, Result};
use crate::evo_log::PhaseRecord;
use crate::evo_prompt::{self, EvoPromptContext};
use crate::memory_access::MemoryAccess;
use crate::web_search::WebSearch;
use brain_llm::provider::LlmProvider;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Shared resources passed from the main Orchestrator to EvoOrchestrator.
/// These are read-only references to shared infrastructure.
#[derive(Clone)]
pub struct SharedResources {
    /// MCP client pool (read-only).
    pub mcp_pool_info: String,
    /// Skill catalog info (read-only).
    pub skill_names: Vec<String>,
    /// Memory access (shared with main brain).
    pub memory: Arc<dyn MemoryAccess>,
    /// Web search capability (shared MCP tools).
    pub web_search: Arc<dyn WebSearch>,
}

/// The evolution orchestrator — runs independently from the main brain.
pub struct EvoOrchestrator {
    /// Independent LLM-B instance.
    llm: Arc<dyn LlmProvider>,
    /// Evolution system prompt.
    system_prompt: String,
    /// The cycle runner that drives the six-phase loop.
    cycle_runner: CycleRunner,
    /// The coordinator for target management and logging.
    coordinator: EvolutionCoordinator,
    /// Base directory for evolution data.
    base_dir: PathBuf,
    /// Evolution config.
    config: EvoConfig,
}

impl EvoOrchestrator {
    /// Create a new EvoOrchestrator.
    ///
    /// This sets up:
    /// - Independent LLM-B connection
    /// - Evolution-specific system prompt
    /// - CycleRunner with the given LLM
    /// - Coordinator for data management
    pub fn new(
        llm: Arc<dyn LlmProvider>,
        base_dir: &Path,
        config: EvoConfig,
        shared: SharedResources,
    ) -> Result<Self> {
        let coordinator = EvolutionCoordinator::new(base_dir, config.clone()).map_err(|e| {
            EvolverError::InvalidState(format!("Failed to create coordinator: {e}"))
        })?;

        // Build the system prompt (initial, will be updated per target)
        let system_prompt = evo_prompt::build_evo_system_prompt(&EvoPromptContext {
            existing_skill_names: shared.skill_names.clone(),
            token_budget: config.token_budget_per_target,
            ..Default::default()
        });

        // Build the cycle runner with shared resources
        let cycle_config = CycleConfig {
            max_iterations: config.max_iterations_per_target,
            token_budget_per_target: config.token_budget_per_target,
            verify_threshold: config.verify_threshold,
            system_prompt: system_prompt.clone(),
            ..CycleConfig::default()
        };
        let cycle_runner = CycleRunner::with_resources(
            llm.clone(),
            cycle_config,
            shared.memory.clone(),
            shared.web_search.clone(),
        );

        Ok(Self {
            llm,
            system_prompt,
            cycle_runner,
            coordinator,
            base_dir: base_dir.to_path_buf(),
            config,
        })
    }

    /// Run evolution for a specific target candidate.
    ///
    /// This is the main entry point called by `spawn_evolution()`.
    /// Returns the cycle result after completion.
    pub async fn run_evolution(&mut self, target: &EvoTargetCandidate) -> Result<CycleResult> {
        // Update system prompt for this specific target
        self.update_system_prompt(target);

        // Pass the updated prompt to CycleRunner
        self.cycle_runner.set_system_prompt(self.system_prompt.clone());

        // Log cycle start
        let target_id = target_id_from_candidate(target);
        let log_id = self.coordinator.log_cycle_start(&target_id);

        // Run the evolution cycle
        let result = self.cycle_runner.run(target).await;

        // Process the result
        match &result {
            Ok(cycle_result) => {
                let (phases, tokens, skills, resolved_backlog, status) =
                    extract_cycle_metadata(cycle_result);

                self.coordinator.log_cycle_end(
                    &log_id,
                    phases,
                    tokens,
                    skills.clone(),
                    resolved_backlog,
                    status,
                )?;

                // If successful, mark target as resolved
                if cycle_result.is_success() {
                    self.coordinator
                        .resolve_target(&target_id, &log_id, skills)?;
                }
            }
            Err(e) => {
                // Log the error as a blocked cycle
                self.coordinator.log_cycle_end(
                    &log_id,
                    vec![],
                    0,
                    vec![],
                    vec![],
                    crate::evo_log::EvoCycleStatus::Blocked,
                )?;
                tracing::error!("Evolution cycle failed: {e}");
            }
        }

        result
    }

    /// Run a full night session — process multiple targets until budget exhausted.
    pub async fn run_night_session(&mut self) -> NightSessionOutput {
        let mut output = NightSessionOutput::default();
        let mut total_tokens: u64 = 0;
        let session_start = std::time::Instant::now();

        while total_tokens < self.config.token_budget_per_night {
            // Check max night duration
            if session_start.elapsed().as_secs() > self.config.max_night_duration_secs {
                output.stop_reason = "night duration limit".into();
                break;
            }

            // Pick next target
            let target = match self.coordinator.pick_next_target() {
                Some(t) => t,
                None => {
                    output.stop_reason = "no more targets".into();
                    break;
                }
            };

            // Run evolution
            match self.run_evolution(&target).await {
                Ok(result) => {
                    total_tokens += result.total_tokens();
                    output.total_tokens += result.total_tokens();
                    output.targets_attempted += 1;

                    match result {
                        CycleResult::Success { skills_created, .. } => {
                            output.targets_completed += 1;
                            output.skills_created.extend(skills_created);
                        }
                        CycleResult::Blocked { feedback, .. } => {
                            output.targets_blocked += 1;
                            output.blocked_reasons.push(feedback);
                        }
                        CycleResult::Cancelled { reason, .. } => {
                            output.stop_reason = reason;
                            break;
                        }
                    }
                }
                Err(e) => {
                    output.targets_failed += 1;
                    output.errors.push(e.to_string());
                }
            }
        }

        if output.stop_reason.is_empty() {
            output.stop_reason = "token budget exhausted".into();
        }
        output.duration_secs = session_start.elapsed().as_secs();

        output
    }

    /// Update the system prompt for a specific target.
    fn update_system_prompt(&mut self, target: &EvoTargetCandidate) {
        let ctx = EvoPromptContext {
            current_target: crate::cycle_runner::describe_target(target),
            existing_skill_names: self
                .coordinator
                .capability_tree()
                .domains
                .iter()
                .flat_map(|d| d.skills.clone())
                .collect(),
            related_backlog_entries: self
                .coordinator
                .backlog()
                .query_sorted_by_priority()
                .iter()
                .map(|e| e.description.clone())
                .take(5)
                .collect(),
            token_budget: self.config.token_budget_per_target,
            ..Default::default()
        };
        self.system_prompt = evo_prompt::build_evo_system_prompt(&ctx);
    }

    /// Access the coordinator.
    pub fn coordinator(&self) -> &EvolutionCoordinator {
        &self.coordinator
    }

    /// Access the system prompt.
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Access the LLM provider.
    pub fn llm(&self) -> &Arc<dyn LlmProvider> {
        &self.llm
    }

    /// Access the base directory.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Access the config.
    pub fn config(&self) -> &EvoConfig {
        &self.config
    }
}

/// Output of a night evolution session.
#[derive(Clone, Debug, Default)]
pub struct NightSessionOutput {
    pub targets_attempted: u32,
    pub targets_completed: u32,
    pub targets_blocked: u32,
    pub targets_failed: u32,
    pub skills_created: Vec<String>,
    pub blocked_reasons: Vec<String>,
    pub errors: Vec<String>,
    pub total_tokens: u64,
    pub duration_secs: u64,
    pub stop_reason: String,
}

// -- Helpers ----------------------------------------------------------------

fn target_id_from_candidate(candidate: &EvoTargetCandidate) -> String {
    match candidate {
        EvoTargetCandidate::UserTarget(t) => t.id.clone(),
        EvoTargetCandidate::BacklogEntry(e) => e.id.clone(),
        EvoTargetCandidate::CodeSelfCheck => "code-self-check".into(),
        EvoTargetCandidate::CapabilityGap { domain, .. } => {
            format!("gap-{domain}")
        }
    }
}

fn extract_cycle_metadata(
    result: &CycleResult,
) -> (
    Vec<PhaseRecord>,
    u64,
    Vec<String>,
    Vec<String>,
    crate::evo_log::EvoCycleStatus,
) {
    match result {
        CycleResult::Success {
            skills_created,
            total_tokens,
            ..
        } => (
            vec![],
            *total_tokens,
            skills_created.clone(),
            vec![],
            crate::evo_log::EvoCycleStatus::Completed,
        ),
        CycleResult::Blocked { total_tokens, .. } => (
            vec![],
            *total_tokens,
            vec![],
            vec![],
            crate::evo_log::EvoCycleStatus::Blocked,
        ),
        CycleResult::Cancelled { total_tokens, .. } => (
            vec![],
            *total_tokens,
            vec![],
            vec![],
            crate::evo_log::EvoCycleStatus::Cancelled,
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{EvoTarget, TargetStatus};
    use brain_llm::echo::EchoLlmProvider;
    use chrono::Utc;
    use tempfile::TempDir;

    fn make_evo_config() -> EvoConfig {
        EvoConfig {
            token_budget_per_target: 10_000,
            token_budget_per_night: 50_000,
            max_iterations_per_target: 2,
            max_duration_per_target_secs: 60,
            max_night_duration_secs: 300,
            verify_threshold: 70.0,
        }
    }

    fn make_shared() -> SharedResources {
        SharedResources {
            mcp_pool_info: String::new(),
            skill_names: vec!["rust-basics".into()],
            memory: Arc::new(crate::memory_access::MockMemoryAccess::empty()),
            web_search: Arc::new(crate::web_search::MockWebSearch::empty()),
        }
    }

    fn make_target() -> EvoTargetCandidate {
        EvoTargetCandidate::UserTarget(EvoTarget {
            id: "test-target".into(),
            direction: "test".into(),
            description: "test desc".into(),
            priority: 1,
            status: TargetStatus::Pending,
            checkpoints: vec![],
            created_at: Utc::now(),
            related_skills: vec![],
        })
    }

    #[test]
    fn test_evo_orchestrator_creation() {
        let dir = TempDir::new().unwrap();
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let evo = EvoOrchestrator::new(llm, dir.path(), make_evo_config(), make_shared());
        assert!(evo.is_ok());
    }

    #[test]
    fn test_evo_orchestrator_has_system_prompt() {
        let dir = TempDir::new().unwrap();
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let evo = EvoOrchestrator::new(llm, dir.path(), make_evo_config(), make_shared()).unwrap();
        assert!(!evo.system_prompt().is_empty());
        assert!(evo.system_prompt().contains("进化子系统"));
    }

    #[test]
    fn test_evo_orchestrator_coordinator() {
        let dir = TempDir::new().unwrap();
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let evo = EvoOrchestrator::new(llm, dir.path(), make_evo_config(), make_shared()).unwrap();
        assert!(!evo.coordinator().has_pending_work());
    }

    #[test]
    fn test_target_id_from_candidate_user() {
        let target = make_target();
        let id = target_id_from_candidate(&target);
        assert_eq!(id, "test-target");
    }

    #[test]
    fn test_target_id_from_candidate_gap() {
        let target = EvoTargetCandidate::CapabilityGap {
            domain: "Rust".into(),
            missing: vec!["async".into()],
        };
        let id = target_id_from_candidate(&target);
        assert_eq!(id, "gap-Rust");
    }

    #[test]
    fn test_target_id_from_candidate_self_check() {
        let id = target_id_from_candidate(&EvoTargetCandidate::CodeSelfCheck);
        assert_eq!(id, "code-self-check");
    }

    #[tokio::test]
    async fn test_run_evolution_with_echo() {
        let dir = TempDir::new().unwrap();
        let llm = Arc::new(EchoLlmProvider::new("test-model"));
        let mut evo =
            EvoOrchestrator::new(llm, dir.path(), make_evo_config(), make_shared()).unwrap();

        let result = evo.run_evolution(&make_target()).await;
        assert!(result.is_ok());
        // EchoLlmProvider won't produce valid SKILL.md, so result is likely Blocked
        let cycle_result = result.unwrap();
        assert!(cycle_result.total_tokens() > 0);
    }

    #[test]
    fn test_night_session_output_default() {
        let output = NightSessionOutput::default();
        assert_eq!(output.targets_attempted, 0);
        assert_eq!(output.stop_reason, "");
    }
}
