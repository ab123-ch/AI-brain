use crate::backlog::{BacklogEntry, BacklogStatus, EvolutionBacklog};
use crate::capability_tree::CapabilityTree;
use crate::cycle_runner::CycleResult;
use crate::error::{EvolverError, Result};
use crate::evo_log::{EvoCycleStatus, EvoLogEntry, EvoLogStore, PhaseRecord};
use crate::memory_access::MemoryAccess;
use crate::target::{EvoTarget, EvoTargetQueue, TargetStatus};
use crate::web_search::WebSearch;
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// EvoTargetCandidate — the thing the coordinator picks as the next target
// ---------------------------------------------------------------------------

/// A candidate for the next evolution cycle, coming from different sources.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum EvoTargetCandidate {
    /// A user-defined evolution direction.
    UserTarget(EvoTarget),
    /// An entry from the runtime problem backlog.
    BacklogEntry(BacklogEntry),
    /// Code self-check (chosen when all user targets are satisfied).
    CodeSelfCheck,
    /// A detected capability gap.
    CapabilityGap {
        domain: String,
        missing: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// EvoConfig — tuning knobs for the coordinator
// ---------------------------------------------------------------------------

/// Configuration for evolution cycle budgets and thresholds.
#[derive(Clone, Debug)]
pub struct EvoConfig {
    pub token_budget_per_target: u64,
    pub token_budget_per_night: u64,
    pub max_iterations_per_target: u32,
    pub max_duration_per_target_secs: u64,
    pub max_night_duration_secs: u64,
    pub verify_threshold: f64,
}

impl Default for EvoConfig {
    fn default() -> Self {
        Self {
            token_budget_per_target: 100_000,
            token_budget_per_night: 500_000,
            max_iterations_per_target: 3,
            max_duration_per_target_secs: 3600,
            max_night_duration_secs: 14400,
            verify_threshold: 70.0,
        }
    }
}

// ---------------------------------------------------------------------------
// NightSessionResult — summary of a night session
// ---------------------------------------------------------------------------

/// Summary statistics for a completed night evolution session.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct NightSessionResult {
    pub targets_processed: u32,
    pub skills_created: Vec<String>,
    pub backlog_resolved: Vec<String>,
    pub total_tokens: u64,
    pub duration_secs: u64,
    pub blocked_targets: Vec<String>,
}

// ---------------------------------------------------------------------------
// EvolutionCoordinator
// ---------------------------------------------------------------------------

/// The central coordinator for evolution targets.
///
/// It integrates all data models (Task 1-4) and is responsible for:
/// - Picking the next evolution target by priority
/// - Logging cycle start/end
/// - Marking targets as resolved or blocked
/// - Producing night session summaries
pub struct EvolutionCoordinator {
    target_queue: EvoTargetQueue,
    backlog: EvolutionBacklog,
    capability_tree: CapabilityTree,
    log: EvoLogStore,
    config: EvoConfig,
    base_dir: PathBuf,
}

impl EvolutionCoordinator {
    /// Initialise all sub-components from the given base directory.
    pub fn new(base_dir: &Path, config: EvoConfig) -> Result<Self> {
        let evolver_dir = base_dir.join("evolver");
        std::fs::create_dir_all(&evolver_dir)?;

        Ok(Self {
            target_queue: EvoTargetQueue::new(&evolver_dir),
            backlog: EvolutionBacklog::new(&evolver_dir),
            capability_tree: CapabilityTree::load(&evolver_dir)
                .map_err(|e| EvolverError::Persistence(format!("load capability tree: {e}")))?,
            log: EvoLogStore::new(&evolver_dir),
            config,
            base_dir: evolver_dir,
        })
    }

    // -- Target selection ---------------------------------------------------

    /// Pick the next evolution target candidate using priority ordering:
    ///
    /// 1. `InProgress` user targets (resume)
    /// 2. Highest-priority `Pending` user target
    /// 3. Highest-frequency backlog entry
    /// 4. Capability gap (when all user targets are satisfied)
    pub fn pick_next_target(&self) -> Option<EvoTargetCandidate> {
        // Priority 1: InProgress user targets (resume unfinished work)
        if let Some(t) = self
            .target_queue
            .list_targets()
            .iter()
            .find(|t| t.status == TargetStatus::InProgress)
        {
            return Some(EvoTargetCandidate::UserTarget(t.clone()));
        }

        // Priority 2: Highest-priority Pending user target
        let pending: Vec<&EvoTarget> = self
            .target_queue
            .list_targets()
            .iter()
            .filter(|t| t.status == TargetStatus::Pending)
            .collect();
        if let Some(best) = pending.iter().min_by_key(|t| t.priority) {
            return Some(EvoTargetCandidate::UserTarget((*best).clone()));
        }

        // Priority 3: Highest-frequency backlog entry
        let backlog_pending: Vec<&BacklogEntry> = self
            .backlog
            .query_sorted_by_priority()
            .into_iter()
            .filter(|e| e.status == BacklogStatus::Pending || e.status == BacklogStatus::InProgress)
            .collect();
        if let Some(entry) = backlog_pending.first() {
            return Some(EvoTargetCandidate::BacklogEntry((*entry).clone()));
        }

        // Priority 4: Capability gap (user targets all satisfied)
        if let Some(gap) = self.capability_tree.gaps.first() {
            return Some(EvoTargetCandidate::CapabilityGap {
                domain: gap.domain.clone(),
                missing: gap.missing.clone(),
            });
        }

        None
    }

    /// Returns true if there is any evolution work to be done.
    pub fn has_pending_work(&self) -> bool {
        // Check user targets
        let has_user_targets = self
            .target_queue
            .list_targets()
            .iter()
            .any(|t| t.status == TargetStatus::Pending || t.status == TargetStatus::InProgress);

        if has_user_targets {
            return true;
        }

        // Check backlog
        let has_backlog = self
            .backlog
            .query_by_status(BacklogStatus::Pending)
            .iter()
            .chain(
                self.backlog
                    .query_by_status(BacklogStatus::InProgress)
                    .iter(),
            )
            .count()
            > 0;

        if has_backlog {
            return true;
        }

        // Check capability gaps
        !self.capability_tree.gaps.is_empty()
    }

    // -- State mutations ----------------------------------------------------

    /// Mark a target as completed, update the capability tree, and persist.
    pub fn resolve_target(
        &mut self,
        target_id: &str,
        evo_log_id: &str,
        skills: Vec<String>,
    ) -> Result<()> {
        // Update the user target queue if this is a user target.
        if self
            .target_queue
            .list_targets()
            .iter()
            .any(|t| t.id == target_id)
        {
            self.target_queue
                .update_status(target_id, TargetStatus::Completed)
                .map_err(EvolverError::Persistence)?;
        }

        // Try to resolve a matching backlog entry.
        let backlog_entries: Vec<String> = self
            .backlog
            .query_sorted_by_priority()
            .iter()
            .filter(|e| e.id == target_id)
            .map(|e| e.id.clone())
            .collect();
        for bid in &backlog_entries {
            self.backlog
                .resolve_entry(bid, evo_log_id)
                .map_err(EvolverError::Persistence)?;
        }

        // Update capability tree with newly created skills.
        for skill in &skills {
            self.capability_tree.update_with_new_skill(skill, None);
        }
        self.capability_tree
            .save(&self.base_dir)
            .map_err(|e| EvolverError::Persistence(format!("save capability tree: {e}")))?;

        Ok(())
    }

    /// Mark a target as blocked with a reason.
    pub fn block_target(&mut self, target_id: &str, reason: &str) -> Result<()> {
        // Try user target queue first.
        let is_user_target = self
            .target_queue
            .list_targets()
            .iter()
            .any(|t| t.id == target_id);

        if is_user_target {
            self.target_queue
                .update_status(target_id, TargetStatus::Blocked)
                .map_err(EvolverError::Persistence)?;
            return Ok(());
        }

        // Try backlog entries — set status to Blocked via block_entry().
        let found_in_backlog = {
            let entries = self.backlog.query_by_status(BacklogStatus::Pending);
            entries
                .iter()
                .chain(
                    self.backlog
                        .query_by_status(BacklogStatus::InProgress)
                        .iter(),
                )
                .any(|e| e.id == target_id)
        };

        if found_in_backlog {
            self.backlog
                .block_entry(target_id, reason)
                .map_err(EvolverError::Persistence)?;
            return Ok(());
        }

        Err(EvolverError::NotFound(format!(
            "target not found in queue or backlog: {target_id}"
        )))
    }

    // -- Logging ------------------------------------------------------------

    /// Create a new EvoLogEntry at the start of a cycle and return its id.
    pub fn log_cycle_start(&mut self, target_id: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let log_id = format!("evo-{}-{seq}", Utc::now().format("%Y%m%d%H%M%S"));
        let entry = EvoLogEntry {
            id: log_id.clone(),
            target_id: target_id.to_string(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            phases: Vec::new(),
            total_tokens: 0,
            skills_created: Vec::new(),
            backlog_resolved: Vec::new(),
            status: EvoCycleStatus::Running,
        };
        self.log.append(entry);
        log_id
    }

    /// Update an existing log entry at the end of a cycle.
    pub fn log_cycle_end(
        &mut self,
        log_id: &str,
        phases: Vec<PhaseRecord>,
        tokens: u64,
        skills: Vec<String>,
        resolved_backlog: Vec<String>,
        status: EvoCycleStatus,
    ) -> Result<()> {
        self.log
            .update_entry(log_id, phases, tokens, skills, resolved_backlog, status)
            .map_err(EvolverError::Persistence)
    }

    // -- Night session summary ----------------------------------------------

    /// Compute summary statistics for the current night session.
    ///
    /// Aggregates data from log entries created today.
    pub fn night_session_summary(&self) -> NightSessionResult {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let today_entries = self.log.query_by_date(&today);

        let mut result = NightSessionResult::default();

        for entry in today_entries {
            if entry.status == EvoCycleStatus::Completed {
                result.targets_processed += 1;
            }
            if entry.status == EvoCycleStatus::Blocked {
                result.blocked_targets.push(entry.target_id.clone());
            }
            result.total_tokens += entry.total_tokens;
            for skill in &entry.skills_created {
                if !result.skills_created.contains(skill) {
                    result.skills_created.push(skill.clone());
                }
            }
            for bl in &entry.backlog_resolved {
                if !result.backlog_resolved.contains(bl) {
                    result.backlog_resolved.push(bl.clone());
                }
            }
        }

        result
    }

    // -- Accessors ----------------------------------------------------------

    /// Access the target queue.
    pub fn target_queue(&self) -> &EvoTargetQueue {
        &self.target_queue
    }

    /// Mutable access to the target queue.
    pub fn target_queue_mut(&mut self) -> &mut EvoTargetQueue {
        &mut self.target_queue
    }

    /// Access the backlog.
    pub fn backlog(&self) -> &EvolutionBacklog {
        &self.backlog
    }

    /// Mutable access to the backlog.
    pub fn backlog_mut(&mut self) -> &mut EvolutionBacklog {
        &mut self.backlog
    }

    /// Access the capability tree.
    pub fn capability_tree(&self) -> &CapabilityTree {
        &self.capability_tree
    }

    /// Access the log store.
    pub fn log_store(&self) -> &EvoLogStore {
        &self.log
    }

    /// Access the config.
    pub fn config(&self) -> &EvoConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// SharedResources — shared read-only resources for evolution
// ---------------------------------------------------------------------------

/// Shared resources passed from the main Orchestrator to evolution runners.
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

// ---------------------------------------------------------------------------
// NightSessionOutput — summary of a night evolution session
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a stable target ID from an `EvoTargetCandidate`.
pub fn target_id_from_candidate(candidate: &EvoTargetCandidate) -> String {
    match candidate {
        EvoTargetCandidate::UserTarget(t) => t.id.clone(),
        EvoTargetCandidate::BacklogEntry(e) => e.id.clone(),
        EvoTargetCandidate::CodeSelfCheck => "code-self-check".into(),
        EvoTargetCandidate::CapabilityGap { domain, .. } => {
            format!("gap-{domain}")
        }
    }
}

/// Extract metadata from a `CycleResult` for logging.
pub fn extract_cycle_metadata(
    result: &CycleResult,
) -> (
    Vec<PhaseRecord>,
    u64,
    Vec<String>,
    Vec<String>,
    EvoCycleStatus,
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
            EvoCycleStatus::Completed,
        ),
        CycleResult::Blocked { total_tokens, .. } => (
            vec![],
            *total_tokens,
            vec![],
            vec![],
            EvoCycleStatus::Blocked,
        ),
        CycleResult::Cancelled { total_tokens, .. } => (
            vec![],
            *total_tokens,
            vec![],
            vec![],
            EvoCycleStatus::Cancelled,
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::{BacklogCategory, BacklogSource, Severity};
    use tempfile::TempDir;

    fn make_target(id: &str, priority: u32, status: TargetStatus) -> EvoTarget {
        EvoTarget {
            id: id.to_string(),
            direction: format!("direction-{id}"),
            description: format!("description-{id}"),
            priority,
            status,
            checkpoints: Vec::new(),
            created_at: Utc::now(),
            related_skills: Vec::new(),
        }
    }

    fn make_backlog_entry(id: &str, frequency: u32, status: BacklogStatus) -> BacklogEntry {
        BacklogEntry {
            id: id.to_string(),
            source: BacklogSource::SelfDiagnosis,
            category: BacklogCategory::KnowledgeGap,
            description: format!("backlog description-{id}"),
            severity: Severity::Medium,
            frequency,
            status,
            created_at: Utc::now(),
            context_snapshot: None,
            resolved_at: None,
            evolution_log_id: None,
        }
    }

    // -- pick_next_target tests ---------------------------------------------

    #[test]
    fn test_pick_next_target_in_progress_first() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        // Add a Pending target with higher priority (lower number).
        coord
            .target_queue_mut()
            .add_target(make_target("pending-high", 1, TargetStatus::Pending))
            .unwrap();
        // Add an InProgress target with lower priority.
        coord
            .target_queue_mut()
            .add_target(make_target("in-progress", 5, TargetStatus::InProgress))
            .unwrap();

        let candidate = coord.pick_next_target().unwrap();
        match candidate {
            EvoTargetCandidate::UserTarget(t) => {
                assert_eq!(t.id, "in-progress");
                assert_eq!(t.status, TargetStatus::InProgress);
            }
            other => panic!("expected UserTarget, got {:?}", other),
        }
    }

    #[test]
    fn test_pick_next_target_user_priority() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        // Add two pending targets; priority 3 is lower priority than 1.
        coord
            .target_queue_mut()
            .add_target(make_target("low-prio", 3, TargetStatus::Pending))
            .unwrap();
        coord
            .target_queue_mut()
            .add_target(make_target("high-prio", 1, TargetStatus::Pending))
            .unwrap();

        let candidate = coord.pick_next_target().unwrap();
        match candidate {
            EvoTargetCandidate::UserTarget(t) => {
                assert_eq!(t.id, "high-prio");
                assert_eq!(t.priority, 1);
            }
            other => panic!("expected UserTarget, got {:?}", other),
        }
    }

    #[test]
    fn test_pick_next_target_backlog_when_no_user_targets() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        // No user targets; add a backlog entry.
        coord
            .backlog_mut()
            .add_entry(make_backlog_entry("backlog-1", 5, BacklogStatus::Pending))
            .unwrap();

        let candidate = coord.pick_next_target().unwrap();
        match candidate {
            EvoTargetCandidate::BacklogEntry(e) => {
                assert_eq!(e.id, "backlog-1");
            }
            other => panic!("expected BacklogEntry, got {:?}", other),
        }
    }

    #[test]
    fn test_pick_next_target_capability_gap_when_all_satisfied() {
        let dir = TempDir::new().unwrap();
        // No user targets, no backlog, but a capability gap.
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        // Manually inject a gap into the capability tree.
        coord
            .capability_tree
            .gaps
            .push(crate::capability_tree::SkillGap {
                domain: "Rust".to_string(),
                missing: vec!["async patterns".to_string()],
            });
        coord.capability_tree.save(&coord.base_dir).unwrap();

        let candidate = coord.pick_next_target().unwrap();
        match candidate {
            EvoTargetCandidate::CapabilityGap { domain, missing } => {
                assert_eq!(domain, "Rust");
                assert_eq!(missing, vec!["async patterns".to_string()]);
            }
            other => panic!("expected CapabilityGap, got {:?}", other),
        }
    }

    #[test]
    fn test_pick_next_target_returns_none_when_empty() {
        let dir = TempDir::new().unwrap();
        let coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        assert!(coord.pick_next_target().is_none());
    }

    // -- has_pending_work tests ---------------------------------------------

    #[test]
    fn test_has_pending_work_with_pending_target() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();
        assert!(!coord.has_pending_work());

        coord
            .target_queue_mut()
            .add_target(make_target("t1", 1, TargetStatus::Pending))
            .unwrap();
        assert!(coord.has_pending_work());
    }

    #[test]
    fn test_has_pending_work_with_backlog() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        coord
            .backlog_mut()
            .add_entry(make_backlog_entry("b1", 1, BacklogStatus::Pending))
            .unwrap();
        assert!(coord.has_pending_work());
    }

    #[test]
    fn test_has_pending_work_with_gap() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        // Manually add a gap.
        coord
            .capability_tree
            .gaps
            .push(crate::capability_tree::SkillGap {
                domain: "Testing".to_string(),
                missing: vec!["property-based testing".to_string()],
            });
        assert!(coord.has_pending_work());
    }

    #[test]
    fn test_has_pending_work_false_when_completed_only() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        coord
            .target_queue_mut()
            .add_target(make_target("done", 1, TargetStatus::Completed))
            .unwrap();
        assert!(!coord.has_pending_work());
    }

    // -- resolve_target tests -----------------------------------------------

    #[test]
    fn test_resolve_target_updates_tree() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        coord
            .target_queue_mut()
            .add_target(make_target("t-resolve", 1, TargetStatus::InProgress))
            .unwrap();

        coord
            .resolve_target(
                "t-resolve",
                "log-001",
                vec!["skill-a".to_string(), "skill-b".to_string()],
            )
            .unwrap();

        // Target should be completed.
        let target = coord
            .target_queue()
            .list_targets()
            .iter()
            .find(|t| t.id == "t-resolve")
            .unwrap();
        assert_eq!(target.status, TargetStatus::Completed);

        // Capability tree should have the new skills.
        let tree = coord.capability_tree();
        assert_eq!(tree.domains.len(), 1);
        assert!(tree.domains[0].skills.contains(&"skill-a".to_string()));
        assert!(tree.domains[0].skills.contains(&"skill-b".to_string()));
    }

    // -- block_target tests -------------------------------------------------

    #[test]
    fn test_block_target_user_target() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        coord
            .target_queue_mut()
            .add_target(make_target("t-block", 1, TargetStatus::InProgress))
            .unwrap();

        coord.block_target("t-block", "dependency missing").unwrap();

        let target = coord
            .target_queue()
            .list_targets()
            .iter()
            .find(|t| t.id == "t-block")
            .unwrap();
        assert_eq!(target.status, TargetStatus::Blocked);
    }

    #[test]
    fn test_block_target_backlog_entry() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        coord
            .backlog_mut()
            .add_entry(make_backlog_entry("b-block", 3, BacklogStatus::Pending))
            .unwrap();

        coord.block_target("b-block", "cannot fix").unwrap();

        // Backlog entry should be Blocked, not Resolved.
        let blocked = coord.backlog().query_by_status(BacklogStatus::Blocked);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, "b-block");
        let pending = coord.backlog().query_by_status(BacklogStatus::Pending);
        assert!(pending.is_empty());
    }

    #[test]
    fn test_block_target_not_found() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        let result = coord.block_target("nonexistent", "no reason");
        assert!(result.is_err());
    }

    // -- log_cycle_start_and_end tests --------------------------------------

    #[test]
    fn test_log_cycle_start_and_end() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        // Start a cycle.
        let log_id = coord.log_cycle_start("target-xyz");
        assert!(!log_id.is_empty());

        // Verify the log entry was created with Running status.
        let latest = coord.log_store().latest().unwrap();
        assert_eq!(latest.target_id, "target-xyz");
        assert_eq!(latest.status, EvoCycleStatus::Running);
        assert!(latest.finished_at.is_none());

        // End the cycle.
        coord
            .log_cycle_end(
                &log_id,
                vec![PhaseRecord {
                    phase: crate::evo_log::EvoPhase::Learn,
                    duration_secs: 42,
                    summary: "learned something".to_string(),
                    tokens_used: 5000,
                }],
                5000,
                vec!["new-skill".to_string()],
                vec!["old-backlog-id".to_string()],
                EvoCycleStatus::Completed,
            )
            .unwrap();

        // Verify the log entry was updated.
        let latest = coord.log_store().latest().unwrap();
        assert_eq!(latest.id, log_id);
        assert_eq!(latest.status, EvoCycleStatus::Completed);
        assert!(latest.finished_at.is_some());
        assert_eq!(latest.phases.len(), 1);
        assert_eq!(latest.total_tokens, 5000);
        assert_eq!(latest.skills_created, vec!["new-skill".to_string()]);
        assert_eq!(latest.backlog_resolved, vec!["old-backlog-id".to_string()]);
    }

    // -- night_session_summary tests ----------------------------------------

    #[test]
    fn test_night_session_summary_aggregates() {
        let dir = TempDir::new().unwrap();
        let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

        // Start and complete a cycle.
        let log_id = coord.log_cycle_start("target-1");
        coord
            .log_cycle_end(
                &log_id,
                vec![],
                1000,
                vec!["skill-x".to_string()],
                vec![],
                EvoCycleStatus::Completed,
            )
            .unwrap();

        // Start and block another cycle.
        let log_id2 = coord.log_cycle_start("target-2");
        coord
            .log_cycle_end(
                &log_id2,
                vec![],
                500,
                vec![],
                vec!["bl-1".to_string()],
                EvoCycleStatus::Blocked,
            )
            .unwrap();

        let summary = coord.night_session_summary();
        assert_eq!(summary.targets_processed, 1);
        assert_eq!(summary.blocked_targets, vec!["target-2".to_string()]);
        assert_eq!(summary.total_tokens, 1500);
        assert_eq!(summary.skills_created, vec!["skill-x".to_string()]);
        assert_eq!(summary.backlog_resolved, vec!["bl-1".to_string()]);
    }

    // -- target_id_from_candidate tests (migrated from evo_orchestrator) ---

    #[test]
    fn test_target_id_from_candidate_user() {
        let target =
            EvoTargetCandidate::UserTarget(make_target("test-target", 1, TargetStatus::Pending));
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

    #[test]
    fn test_night_session_output_default() {
        let output = NightSessionOutput::default();
        assert_eq!(output.targets_attempted, 0);
        assert_eq!(output.stop_reason, "");
    }
}
