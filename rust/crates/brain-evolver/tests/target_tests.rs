use brain_evolver::target::*;
use chrono::Utc;
use tempfile::TempDir;

fn make_target(id: &str, priority: u32) -> EvoTarget {
    EvoTarget {
        id: id.to_string(),
        direction: "improve reasoning".to_string(),
        description: format!("target {id}"),
        priority,
        status: TargetStatus::Pending,
        checkpoints: vec![Checkpoint {
            desc: "checkpoint 1".to_string(),
            met: false,
        }],
        created_at: Utc::now(),
        related_skills: vec![],
    }
}

#[test]
fn test_add_and_list_targets() {
    let tmp = TempDir::new().unwrap();
    let mut queue = EvoTargetQueue::new(tmp.path());

    assert!(queue.list_targets().is_empty());

    queue.add_target(make_target("t1", 1)).unwrap();
    queue.add_target(make_target("t2", 2)).unwrap();

    let targets = queue.list_targets();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].id, "t1");
    assert_eq!(targets[1].id, "t2");
}

#[test]
fn test_priority_ordering() {
    let tmp = TempDir::new().unwrap();
    let mut queue = EvoTargetQueue::new(tmp.path());

    queue.add_target(make_target("low", 5)).unwrap();
    queue.add_target(make_target("high", 1)).unwrap();
    queue.add_target(make_target("mid", 3)).unwrap();

    let sorted = queue.sorted_targets();
    assert_eq!(sorted[0].id, "high");
    assert_eq!(sorted[1].id, "mid");
    assert_eq!(sorted[2].id, "low");
}

#[test]
fn test_update_status() {
    let tmp = TempDir::new().unwrap();
    let mut queue = EvoTargetQueue::new(tmp.path());

    queue.add_target(make_target("t1", 1)).unwrap();

    queue.update_status("t1", TargetStatus::InProgress).unwrap();
    assert_eq!(queue.list_targets()[0].status, TargetStatus::InProgress);

    queue.update_status("t1", TargetStatus::Completed).unwrap();
    assert_eq!(queue.list_targets()[0].status, TargetStatus::Completed);

    // Unknown id returns error.
    assert!(queue
        .update_status("nonexistent", TargetStatus::Blocked)
        .is_err());
}

#[test]
fn test_update_checkpoint() {
    let tmp = TempDir::new().unwrap();
    let mut queue = EvoTargetQueue::new(tmp.path());

    let mut target = make_target("t1", 1);
    target.checkpoints = vec![
        Checkpoint {
            desc: "step 1".to_string(),
            met: false,
        },
        Checkpoint {
            desc: "step 2".to_string(),
            met: false,
        },
    ];
    queue.add_target(target).unwrap();

    queue.update_checkpoint("t1", 0, true).unwrap();
    assert!(queue.list_targets()[0].checkpoints[0].met);
    assert!(!queue.list_targets()[0].checkpoints[1].met);

    queue.update_checkpoint("t1", 1, true).unwrap();
    assert!(queue.list_targets()[0].checkpoints[1].met);

    // Out of bounds index returns error.
    assert!(queue.update_checkpoint("t1", 99, true).is_err());
    // Unknown id returns error.
    assert!(queue.update_checkpoint("nonexistent", 0, true).is_err());
}

#[test]
fn test_remove_target() {
    let tmp = TempDir::new().unwrap();
    let mut queue = EvoTargetQueue::new(tmp.path());

    queue.add_target(make_target("t1", 1)).unwrap();
    queue.add_target(make_target("t2", 2)).unwrap();

    queue.remove_target("t1").unwrap();
    assert_eq!(queue.list_targets().len(), 1);
    assert_eq!(queue.list_targets()[0].id, "t2");

    // Removing again returns error.
    assert!(queue.remove_target("t1").is_err());
    // Unknown id returns error.
    assert!(queue.remove_target("nonexistent").is_err());
}
