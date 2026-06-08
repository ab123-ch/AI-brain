use brain_evolver::evo_log::*;
use tempfile::TempDir;

fn make_entry(id: &str, target_id: &str, started_at: &str) -> EvoLogEntry {
    EvoLogEntry {
        id: id.to_string(),
        target_id: target_id.to_string(),
        started_at: started_at.to_string(),
        finished_at: None,
        phases: vec![PhaseRecord {
            phase: EvoPhase::Perceive,
            duration_secs: 10,
            summary: "perceived issue".to_string(),
            tokens_used: 500,
        }],
        total_tokens: 500,
        skills_created: Vec::new(),
        backlog_resolved: Vec::new(),
        status: EvoCycleStatus::Running,
    }
}

#[test]
fn test_append_and_query() {
    let tmp = TempDir::new().unwrap();
    let mut store = EvoLogStore::new(tmp.path());

    store.append(make_entry("e1", "t1", "2026-06-08T10:00:00Z"));
    store.append(make_entry("e2", "t2", "2026-06-08T14:30:00Z"));
    store.append(make_entry("e3", "t3", "2026-06-09T09:00:00Z"));

    // Reload from disk to verify persistence.
    let store2 = EvoLogStore::new(tmp.path());
    assert_eq!(store2.query_by_date("2026-06-08").len(), 2);
    assert_eq!(store2.query_by_date("2026-06-09").len(), 1);
}

#[test]
fn test_query_by_date() {
    let tmp = TempDir::new().unwrap();
    let mut store = EvoLogStore::new(tmp.path());

    store.append(make_entry("e1", "t1", "2026-06-07T08:00:00Z"));
    store.append(make_entry("e2", "t2", "2026-06-08T10:00:00Z"));
    store.append(make_entry("e3", "t3", "2026-06-08T15:00:00Z"));
    store.append(make_entry("e4", "t4", "2026-06-10T12:00:00Z"));

    let june8 = store.query_by_date("2026-06-08");
    assert_eq!(june8.len(), 2);
    assert_eq!(june8[0].id, "e2");
    assert_eq!(june8[1].id, "e3");

    let june11 = store.query_by_date("2026-06-11");
    assert!(june11.is_empty());
}

#[test]
fn test_latest() {
    let tmp = TempDir::new().unwrap();
    let mut store = EvoLogStore::new(tmp.path());

    assert!(store.latest().is_none());

    store.append(make_entry("e1", "t1", "2026-06-08T10:00:00Z"));
    store.append(make_entry("e2", "t2", "2026-06-08T14:00:00Z"));

    let latest = store.latest().unwrap();
    assert_eq!(latest.id, "e2");
    assert_eq!(latest.target_id, "t2");
}
