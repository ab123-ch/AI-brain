use brain_evolver::backlog::{
    BacklogCategory, BacklogEntry, BacklogSource, BacklogStatus, Severity,
};
use brain_evolver::coordinator::{EvoConfig, EvoTargetCandidate, EvolutionCoordinator};
use brain_evolver::evo_log::{EvoCycleStatus, EvoPhase, PhaseRecord};
use brain_evolver::target::{EvoTarget, TargetStatus};
use chrono::Utc;
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

// ---------------------------------------------------------------------------
// pick_next_target tests
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// has_pending_work tests
// ---------------------------------------------------------------------------

#[test]
fn test_has_pending_work() {
    let dir = TempDir::new().unwrap();
    let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

    // Initially no work.
    assert!(!coord.has_pending_work());

    // Add a pending target.
    coord
        .target_queue_mut()
        .add_target(make_target("t1", 1, TargetStatus::Pending))
        .unwrap();
    assert!(coord.has_pending_work());

    // Resolve it.
    let log_id = coord.log_cycle_start("t1");
    coord.resolve_target("t1", &log_id, vec![]).unwrap();
    assert!(!coord.has_pending_work());

    // Add backlog entry.
    coord
        .backlog_mut()
        .add_entry(make_backlog_entry("b1", 1, BacklogStatus::Pending))
        .unwrap();
    assert!(coord.has_pending_work());
}

// ---------------------------------------------------------------------------
// resolve_target tests
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// block_target tests
// ---------------------------------------------------------------------------

#[test]
fn test_block_target() {
    let dir = TempDir::new().unwrap();
    let mut coord = EvolutionCoordinator::new(dir.path(), EvoConfig::default()).unwrap();

    // Block a user target.
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

    // Block a backlog entry.
    coord
        .backlog_mut()
        .add_entry(make_backlog_entry("b-block", 3, BacklogStatus::Pending))
        .unwrap();
    coord.block_target("b-block", "cannot fix").unwrap();
    let pending = coord.backlog().query_by_status(BacklogStatus::Pending);
    assert!(pending.is_empty());

    // Block nonexistent should fail.
    let result = coord.block_target("nonexistent", "no reason");
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// log_cycle_start_and_end tests
// ---------------------------------------------------------------------------

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
                phase: EvoPhase::Learn,
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
