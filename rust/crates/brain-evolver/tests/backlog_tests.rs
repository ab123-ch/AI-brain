use brain_evolver::backlog::*;
use chrono::Utc;
use tempfile::TempDir;

fn make_entry(id: &str, desc: &str, severity: Severity) -> BacklogEntry {
    BacklogEntry {
        id: id.to_string(),
        source: BacklogSource::Eval,
        category: BacklogCategory::KnowledgeGap,
        description: desc.to_string(),
        severity,
        frequency: 1,
        status: BacklogStatus::Pending,
        created_at: Utc::now(),
        context_snapshot: None,
        resolved_at: None,
        evolution_log_id: None,
    }
}

#[test]
fn test_add_and_query_backlog() {
    let tmp = TempDir::new().unwrap();
    let mut bl = EvolutionBacklog::new(tmp.path());

    bl.add_entry(make_entry(
        "e1",
        "missing knowledge about port codes",
        Severity::High,
    ))
    .unwrap();
    bl.add_entry(BacklogEntry {
        id: "e2".into(),
        source: BacklogSource::User,
        category: BacklogCategory::ToolMissing,
        description: "no tool for EDI parsing".into(),
        severity: Severity::Medium,
        frequency: 1,
        status: BacklogStatus::Pending,
        created_at: Utc::now(),
        context_snapshot: None,
        resolved_at: None,
        evolution_log_id: None,
    })
    .unwrap();

    let pending = bl.query_by_status(BacklogStatus::Pending);
    assert_eq!(pending.len(), 2);

    let resolved = bl.query_by_status(BacklogStatus::Resolved);
    assert!(resolved.is_empty());
}

#[test]
fn test_dedup_increments_frequency() {
    let tmp = TempDir::new().unwrap();
    let mut bl = EvolutionBacklog::new(tmp.path());

    bl.add_entry(make_entry(
        "e1",
        "missing knowledge about port codes",
        Severity::Medium,
    ))
    .unwrap();

    // Same source, category, and >60% keyword overlap -> dedup.
    bl.add_entry(make_entry(
        "e2",
        "missing knowledge about port codes",
        Severity::High,
    ))
    .unwrap();

    assert_eq!(bl.query_sorted_by_priority().len(), 1);
    let entry = &bl.query_sorted_by_priority()[0];
    assert_eq!(entry.frequency, 2);
    // Severity should have been upgraded.
    assert_eq!(entry.severity, Severity::High);
    // Original id preserved.
    assert_eq!(entry.id, "e1");
}

#[test]
fn test_resolve_entry() {
    let tmp = TempDir::new().unwrap();
    let mut bl = EvolutionBacklog::new(tmp.path());

    bl.add_entry(make_entry("e1", "some issue", Severity::Low))
        .unwrap();

    bl.resolve_entry("e1", "evo-001").unwrap();

    let resolved = bl.query_by_status(BacklogStatus::Resolved);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].evolution_log_id.as_deref(), Some("evo-001"));
    assert!(resolved[0].resolved_at.is_some());

    // Unknown id returns error.
    assert!(bl.resolve_entry("nonexistent", "evo-002").is_err());
}

#[test]
fn test_priority_sorting() {
    let tmp = TempDir::new().unwrap();
    let mut bl = EvolutionBacklog::new(tmp.path());

    let mut e1 = make_entry("e1", "low freq issue", Severity::Low);
    e1.frequency = 1;
    e1.category = BacklogCategory::CodeQuality;

    let mut e2 = make_entry("e2", "high freq issue", Severity::High);
    e2.frequency = 10;
    e2.category = BacklogCategory::ReasoningWeakness;

    let mut e3 = make_entry("e3", "medium freq issue", Severity::Medium);
    e3.frequency = 5;
    e3.category = BacklogCategory::ToolMissing;

    bl.add_entry(e1).unwrap();
    bl.add_entry(e2).unwrap();
    bl.add_entry(e3).unwrap();

    let sorted = bl.query_sorted_by_priority();
    assert_eq!(sorted[0].id, "e2");
    assert_eq!(sorted[1].id, "e3");
    assert_eq!(sorted[2].id, "e1");
}
