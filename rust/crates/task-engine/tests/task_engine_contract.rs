use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use task_engine::{
    ActualUsage, AdmissionRequest, BudgetLimits, BudgetRequest, BudgetReservationState,
    InstanceRunState, NewBudgetAccount, NewTaskNode, NewTaskRun, NodeKind, NodeState, Scheduler,
    SchedulerLimits, TaskCoordinator, TaskEngineError, TaskEventKind, TaskRepository, TaskRunState,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn repository() -> (TempDir, TaskRepository) {
    let directory = tempfile::tempdir().unwrap();
    let repository = TaskRepository::open(directory.path().join("runtime.db")).unwrap();
    (directory, repository)
}

fn node(node_id: &str, dependencies: &[&str]) -> NewTaskNode {
    NewTaskNode {
        node_id: node_id.into(),
        kind: NodeKind::Model,
        dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
        provider: "mock".into(),
        model: "mock-model".into(),
        profile: "test.writer".into(),
        room_id: Some("room-1".into()),
        member_id: Some("member-a".into()),
        reservation: BudgetRequest {
            input_tokens: 100,
            output_tokens: 40,
        },
        retryable: true,
        side_effecting: false,
    }
}

fn task(task_run_id: &str) -> NewTaskRun {
    NewTaskRun {
        task_run_id: task_run_id.into(),
        workflow: "test.two-step".into(),
        objective: "produce and verify an artifact".into(),
        origin_kind: "test".into(),
        origin_id: format!("origin-{task_run_id}"),
        room_id: Some("room-1".into()),
        config_version: "test-config-v1".into(),
        resolved_config: json!({
            "profiles": {"test.writer": {"model": "mock-model"}},
            "policy": {"retry": 1}
        }),
        parent_budget_account_id: None,
        budget: BudgetLimits {
            input_tokens: 500,
            output_tokens: 200,
        },
        nodes: vec![node("draft", &[]), node("verify", &["draft"])],
    }
}

#[test]
fn persists_config_and_advances_dependency_dag_with_cas() {
    let (directory, repository) = repository();
    let created = repository.create_task(task("task-dag")).unwrap();

    assert_eq!(created.state, TaskRunState::Queued);
    assert!(!created.config_content_hash.is_empty());
    let nodes = repository.nodes("task-dag").unwrap();
    assert_eq!(nodes[0].node_id, "draft");
    assert_eq!(nodes[0].state, NodeState::Ready);
    assert_eq!(nodes[1].node_id, "verify");
    assert_eq!(nodes[1].state, NodeState::WaitingDependency);

    let started = repository
        .start_node("draft", nodes[0].version, "instance-draft-1")
        .unwrap();
    assert_eq!(started.node.state, NodeState::Running);
    assert_eq!(started.reservation.state, BudgetReservationState::Active);
    assert!(matches!(
        repository.start_node("draft", nodes[0].version, "stale-run"),
        Err(TaskEngineError::CasConflict { .. })
    ));

    let completion = repository
        .complete_node(
            "instance-draft-1",
            started.instance.version,
            ActualUsage {
                input_tokens: 37,
                output_tokens: 13,
            },
            Some("artifact-draft"),
        )
        .unwrap();
    assert_eq!(completion.node.state, NodeState::Completed);
    assert_eq!(repository.node("verify").unwrap().state, NodeState::Ready);
    let account = repository
        .budget_account(&created.budget_account_id)
        .unwrap();
    assert_eq!(account.reserved_input_tokens, 0);
    assert_eq!(account.reserved_output_tokens, 0);
    assert_eq!(account.consumed_input_tokens, 37);
    assert_eq!(account.consumed_output_tokens, 13);

    let verify = repository.node("verify").unwrap();
    let started = repository
        .start_node("verify", verify.version, "instance-verify-1")
        .unwrap();
    repository
        .complete_node(
            "instance-verify-1",
            started.instance.version,
            ActualUsage {
                input_tokens: 20,
                output_tokens: 8,
            },
            Some("artifact-verified"),
        )
        .unwrap();
    assert_eq!(
        repository.task("task-dag").unwrap().state,
        TaskRunState::Completed
    );
    assert!(repository
        .events("task-dag", 0)
        .unwrap()
        .iter()
        .any(|event| event.kind == TaskEventKind::TaskCompleted));

    drop(repository);
    let reopened = TaskRepository::open(directory.path().join("runtime.db")).unwrap();
    assert_eq!(
        reopened.task("task-dag").unwrap().resolved_config,
        task("task-dag").resolved_config
    );
    assert!(reopened.ready_nodes(10).unwrap().is_empty());
    assert_eq!(reopened.recover_inflight().unwrap().interrupted, 0);
}

#[test]
#[allow(clippy::too_many_lines)]
fn budget_reserve_settle_and_release_are_atomic_across_parent_chain() {
    let (_directory, repository) = repository();
    repository
        .create_budget_account(NewBudgetAccount {
            budget_account_id: "workspace-budget".into(),
            parent_budget_account_id: None,
            owner_kind: "workspace".into(),
            owner_id: "workspace-1".into(),
            limits: BudgetLimits {
                input_tokens: 100,
                output_tokens: 50,
            },
        })
        .unwrap();
    repository
        .create_budget_account(NewBudgetAccount {
            budget_account_id: "task-budget".into(),
            parent_budget_account_id: Some("workspace-budget".into()),
            owner_kind: "task".into(),
            owner_id: "task-1".into(),
            limits: BudgetLimits {
                input_tokens: 80,
                output_tokens: 40,
            },
        })
        .unwrap();

    let reservation = repository
        .reserve_budget(
            "task-budget",
            "reservation-1",
            "call-1",
            BudgetRequest {
                input_tokens: 60,
                output_tokens: 30,
            },
        )
        .unwrap();
    assert_eq!(
        repository
            .budget_account("workspace-budget")
            .unwrap()
            .reserved_input_tokens,
        60
    );
    assert!(matches!(
        repository.reserve_budget(
            "task-budget",
            "reservation-2",
            "call-2",
            BudgetRequest {
                input_tokens: 30,
                output_tokens: 1,
            }
        ),
        Err(TaskEngineError::BudgetExceeded { .. })
    ));

    let settled = repository
        .settle_budget(
            &reservation.reservation_id,
            reservation.version,
            ActualUsage {
                input_tokens: 25,
                output_tokens: 10,
            },
        )
        .unwrap();
    assert_eq!(settled.state, BudgetReservationState::Settled);
    for account_id in ["workspace-budget", "task-budget"] {
        let account = repository.budget_account(account_id).unwrap();
        assert_eq!(account.reserved_input_tokens, 0);
        assert_eq!(account.reserved_output_tokens, 0);
        assert_eq!(account.consumed_input_tokens, 25);
        assert_eq!(account.consumed_output_tokens, 10);
    }
    let idempotent = repository
        .settle_budget(
            &reservation.reservation_id,
            reservation.version,
            ActualUsage {
                input_tokens: 25,
                output_tokens: 10,
            },
        )
        .unwrap();
    assert_eq!(idempotent, settled);

    let released = repository
        .reserve_budget(
            "task-budget",
            "reservation-release",
            "call-release",
            BudgetRequest {
                input_tokens: 10,
                output_tokens: 5,
            },
        )
        .unwrap();
    repository
        .release_budget(&released.reservation_id, released.version)
        .unwrap();
    assert_eq!(
        repository
            .budget_reservation(&released.reservation_id)
            .unwrap()
            .state,
        BudgetReservationState::Released
    );
}

#[test]
fn concurrent_budget_reservations_cannot_oversubscribe_an_account() {
    let (directory, repository) = repository();
    repository
        .create_budget_account(NewBudgetAccount {
            budget_account_id: "shared".into(),
            parent_budget_account_id: None,
            owner_kind: "workspace".into(),
            owner_id: "workspace-1".into(),
            limits: BudgetLimits {
                input_tokens: 100,
                output_tokens: 100,
            },
        })
        .unwrap();
    drop(repository);

    let database_path = directory.path().join("runtime.db");
    let threads = (0..2)
        .map(|index| {
            let database_path = database_path.clone();
            std::thread::spawn(move || {
                let repository = TaskRepository::open(database_path).unwrap();
                repository.reserve_budget(
                    "shared",
                    &format!("reservation-{index}"),
                    &format!("call-{index}"),
                    BudgetRequest {
                        input_tokens: 70,
                        output_tokens: 1,
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    let results = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(TaskEngineError::BudgetExceeded { .. })))
            .count(),
        1
    );
}

#[test]
fn recovery_requeues_only_retryable_side_effect_free_nodes() {
    let (directory, repository) = repository();
    repository.create_task(task("task-recover")).unwrap();
    let draft = repository.node("draft").unwrap();
    let first = repository
        .start_node("draft", draft.version, "instance-before-crash")
        .unwrap();
    assert_eq!(first.node.state, NodeState::Running);
    drop(repository);

    let reopened = TaskRepository::open(directory.path().join("runtime.db")).unwrap();
    let recovery = reopened.recover_inflight().unwrap();
    assert_eq!(recovery.interrupted, 1);
    assert_eq!(recovery.requeued, 1);
    assert_eq!(reopened.node("draft").unwrap().state, NodeState::Ready);
    assert_eq!(
        reopened
            .budget_reservation(&first.reservation.reservation_id)
            .unwrap()
            .state,
        BudgetReservationState::Released
    );

    let mut unsafe_task = task("task-unsafe");
    unsafe_task.nodes = vec![NewTaskNode {
        side_effecting: true,
        ..node("publish", &[])
    }];
    reopened.create_task(unsafe_task).unwrap();
    let publish = reopened.node("publish").unwrap();
    reopened
        .start_node("publish", publish.version, "instance-publish")
        .unwrap();
    let recovery = reopened.recover_inflight().unwrap();
    assert_eq!(recovery.needs_input, 1);
    assert_eq!(
        reopened.node("publish").unwrap().state,
        NodeState::NeedsInput
    );
    assert_eq!(
        reopened.task("task-unsafe").unwrap().state,
        TaskRunState::NeedsInput
    );
    assert!(!reopened
        .ready_nodes(10)
        .unwrap()
        .iter()
        .any(|node| node.node_id == "publish"));
}

#[test]
fn pre_execution_failure_atomically_fails_queued_task_and_unstarted_nodes() {
    let (_directory, repository) = repository();
    let created = repository.create_task(task("task-pre-execution")).unwrap();

    let failed = repository
        .fail_task_before_execution("task-pre-execution", created.version, "冻结工作目录不可用")
        .unwrap();

    assert_eq!(failed.state, TaskRunState::Failed);
    assert!(repository
        .nodes("task-pre-execution")
        .unwrap()
        .iter()
        .all(|node| node.state == NodeState::Failed));
    assert!(repository.ready_nodes(10).unwrap().is_empty());
    let events = repository.events("task-pre-execution", 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == TaskEventKind::NodeFailed)
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == TaskEventKind::TaskFailed)
            .count(),
        1
    );

    let event_count = events.len();
    let repeated = repository
        .fail_task_before_execution(
            "task-pre-execution",
            created.version,
            "重复结算不应覆盖原状态",
        )
        .unwrap();
    assert_eq!(repeated.state, TaskRunState::Failed);
    assert_eq!(
        repository.events("task-pre-execution", 0).unwrap().len(),
        event_count
    );
}

#[test]
fn pre_execution_failure_fails_recovered_task_and_preserves_interrupted_instance() {
    let (directory, repository) = repository();
    repository
        .create_task(task("task-recovered-failure"))
        .unwrap();
    let draft = repository.node("draft").unwrap();
    let started = repository
        .start_node("draft", draft.version, "instance-recovered-pre-execution")
        .unwrap();
    drop(repository);

    let reopened = TaskRepository::open(directory.path().join("runtime.db")).unwrap();
    let recovery = reopened.recover_inflight().unwrap();
    assert_eq!(recovery.requeued, 1);
    let recovered_task = reopened.task("task-recovered-failure").unwrap();
    assert_eq!(recovered_task.state, TaskRunState::Queued);
    assert_eq!(reopened.node("draft").unwrap().state, NodeState::Ready);

    reopened
        .fail_task_before_execution(
            "task-recovered-failure",
            recovered_task.version,
            "冻结工作目录已删除",
        )
        .unwrap();

    assert_eq!(
        reopened.task("task-recovered-failure").unwrap().state,
        TaskRunState::Failed
    );
    assert!(reopened
        .nodes("task-recovered-failure")
        .unwrap()
        .iter()
        .all(|node| node.state == NodeState::Failed));
    assert_eq!(
        reopened
            .instance(&started.instance.instance_run_id)
            .unwrap()
            .state,
        InstanceRunState::Interrupted
    );
}

#[test]
fn pre_execution_failure_preserves_completed_and_cancelled_tasks() {
    let (_directory, repository) = repository();

    let mut completed_request = task("task-completed-before-failure");
    completed_request.nodes = vec![node("completed-only", &[])];
    repository.create_task(completed_request).unwrap();
    let completed_node = repository.node("completed-only").unwrap();
    let completed_instance = repository
        .start_node(
            "completed-only",
            completed_node.version,
            "instance-completed-before-failure",
        )
        .unwrap();
    let completed = repository
        .complete_node(
            &completed_instance.instance.instance_run_id,
            completed_instance.instance.version,
            ActualUsage {
                input_tokens: 1,
                output_tokens: 1,
            },
            None,
        )
        .unwrap()
        .task;
    assert_eq!(completed.state, TaskRunState::Completed);
    let preserved = repository
        .fail_task_before_execution(&completed.task_run_id, completed.version, "不得覆盖完成态")
        .unwrap();
    assert_eq!(preserved.state, TaskRunState::Completed);
    assert_eq!(
        repository.node("completed-only").unwrap().state,
        NodeState::Completed
    );

    let cancelled = repository
        .create_task(task("task-cancelled-before-failure"))
        .unwrap();
    let cancelled = repository
        .cancel_task(&cancelled.task_run_id, cancelled.version)
        .unwrap();
    let preserved = repository
        .fail_task_before_execution(&cancelled.task_run_id, cancelled.version, "不得覆盖取消态")
        .unwrap();
    assert_eq!(preserved.state, TaskRunState::Cancelled);
    assert!(repository
        .nodes(&cancelled.task_run_id)
        .unwrap()
        .iter()
        .all(|node| node.state == NodeState::Cancelled));
}

#[test]
fn completed_artifact_is_recoverable_without_reexecuting_the_node() {
    let (directory, repository) = repository();
    let mut request = task("task-artifact");
    request.nodes = vec![node("artifact-node", &[])];
    repository.create_task(request).unwrap();
    let node = repository.node("artifact-node").unwrap();
    let started = repository
        .start_node("artifact-node", node.version, "instance-artifact")
        .unwrap();
    let artifact = repository
        .store_artifact(
            "instance-artifact",
            "durable final answer",
            "text/plain; charset=utf-8",
        )
        .unwrap();
    repository
        .complete_node(
            "instance-artifact",
            started.instance.version,
            ActualUsage {
                input_tokens: 12,
                output_tokens: 4,
            },
            Some(&artifact.artifact_id),
        )
        .unwrap();
    drop(repository);

    let reopened = TaskRepository::open(directory.path().join("runtime.db")).unwrap();
    let results = reopened.completed_results("test").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].origin_id, "origin-task-artifact");
    assert_eq!(results[0].artifact.content, "durable final answer");
    assert!(reopened.ready_nodes(10).unwrap().is_empty());
    assert_eq!(reopened.recover_inflight().unwrap().interrupted, 0);
}

#[test]
fn failed_instance_releases_reservation_and_closes_task() {
    let (_directory, repository) = repository();
    let mut request = task("task-failed");
    request.nodes = vec![node("failed-node", &[])];
    let task = repository.create_task(request).unwrap();
    let node = repository.node("failed-node").unwrap();
    let started = repository
        .start_node("failed-node", node.version, "instance-failed")
        .unwrap();

    let failed = repository
        .fail_node(
            "instance-failed",
            started.instance.version,
            "provider unavailable",
            false,
        )
        .unwrap();

    assert_eq!(failed.state, task_engine::InstanceRunState::Failed);
    assert_eq!(
        repository.task("task-failed").unwrap().state,
        TaskRunState::Failed
    );
    assert_eq!(
        repository
            .budget_reservation(&started.reservation.reservation_id)
            .unwrap()
            .state,
        BudgetReservationState::Released
    );
    let account = repository.budget_account(&task.budget_account_id).unwrap();
    assert_eq!(account.reserved_input_tokens, 0);
    assert_eq!(account.reserved_output_tokens, 0);
}

#[test]
fn post_execution_failure_settles_known_usage_instead_of_releasing_it() {
    let (_directory, repository) = repository();
    let mut request = task("task-failed-after-usage");
    request.nodes = vec![node("failed-after-usage-node", &[])];
    let task = repository.create_task(request).unwrap();
    let node = repository.node("failed-after-usage-node").unwrap();
    let started = repository
        .start_node(
            "failed-after-usage-node",
            node.version,
            "instance-failed-after-usage",
        )
        .unwrap();
    let usage = ActualUsage {
        input_tokens: 63,
        output_tokens: 17,
    };

    let failed = repository
        .fail_node_after_execution(
            "instance-failed-after-usage",
            started.instance.version,
            "artifact persistence failed",
            Some(usage),
            false,
        )
        .unwrap();

    assert_eq!(failed.state, task_engine::InstanceRunState::Failed);
    assert_eq!(failed.usage, Some(usage));
    let reservation = repository
        .budget_reservation(&started.reservation.reservation_id)
        .unwrap();
    assert_eq!(reservation.state, BudgetReservationState::Settled);
    assert_eq!(reservation.actual, Some(usage));
    let account = repository.budget_account(&task.budget_account_id).unwrap();
    assert_eq!(account.reserved_input_tokens, 0);
    assert_eq!(account.reserved_output_tokens, 0);
    assert_eq!(account.consumed_input_tokens, usage.input_tokens);
    assert_eq!(account.consumed_output_tokens, usage.output_tokens);
    let failure_event = repository
        .events(&task.task_run_id, 0)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == TaskEventKind::NodeFailed)
        .unwrap();
    assert_eq!(failure_event.payload["usage"], json!(usage));
    assert_eq!(failure_event.payload["usage_basis"], "runtime_reported");
}

#[test]
fn post_execution_failure_without_usage_settles_reservation_upper_bound() {
    let (_directory, repository) = repository();
    let mut request = task("task-failed-without-usage");
    request.nodes = vec![node("failed-without-usage-node", &[])];
    let task = repository.create_task(request).unwrap();
    let node = repository.node("failed-without-usage-node").unwrap();
    let started = repository
        .start_node(
            "failed-without-usage-node",
            node.version,
            "instance-failed-without-usage",
        )
        .unwrap();

    let failed = repository
        .fail_node_after_execution(
            "instance-failed-without-usage",
            started.instance.version,
            "provider disconnected after request dispatch",
            None,
            false,
        )
        .unwrap();

    let estimated_usage = ActualUsage {
        input_tokens: started.reservation.reserved.input_tokens,
        output_tokens: started.reservation.reserved.output_tokens,
    };
    assert_eq!(failed.state, task_engine::InstanceRunState::Failed);
    assert_eq!(failed.usage, Some(estimated_usage));
    let reservation = repository
        .budget_reservation(&started.reservation.reservation_id)
        .unwrap();
    assert_eq!(reservation.state, BudgetReservationState::Settled);
    assert_eq!(reservation.actual, Some(estimated_usage));
    let account = repository.budget_account(&task.budget_account_id).unwrap();
    assert_eq!(account.reserved_input_tokens, 0);
    assert_eq!(account.reserved_output_tokens, 0);
    assert_eq!(account.consumed_input_tokens, estimated_usage.input_tokens);
    assert_eq!(
        account.consumed_output_tokens,
        estimated_usage.output_tokens
    );
    let failure_event = repository
        .events(&task.task_run_id, 0)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == TaskEventKind::NodeFailed)
        .unwrap();
    assert_eq!(failure_event.payload["usage"], json!(estimated_usage));
    assert_eq!(
        failure_event.payload["usage_basis"],
        "reservation_upper_bound"
    );
}

#[test]
fn queued_task_cancellation_uses_cas_and_never_becomes_ready_again() {
    let (_directory, repository) = repository();
    let created = repository.create_task(task("task-cancel")).unwrap();

    assert!(matches!(
        repository.cancel_task("task-cancel", created.version + 1),
        Err(TaskEngineError::CasConflict { .. })
    ));
    let cancelled = repository
        .cancel_task("task-cancel", created.version)
        .unwrap();
    assert_eq!(cancelled.state, TaskRunState::Cancelled);
    assert!(cancelled.cancel_requested);
    assert!(repository
        .nodes("task-cancel")
        .unwrap()
        .iter()
        .all(|node| node.state == NodeState::Cancelled));
    assert!(repository.ready_nodes(10).unwrap().is_empty());
    assert_eq!(
        repository
            .cancel_task("task-cancel", created.version)
            .unwrap()
            .state,
        TaskRunState::Cancelled
    );
}

#[test]
fn running_task_cancellation_is_durable_before_worker_settlement() {
    let (_directory, repository) = repository();
    let mut request = task("task-running-cancel");
    request.nodes = vec![node("running-cancel-node", &[])];
    repository.create_task(request).unwrap();
    let node = repository.node("running-cancel-node").unwrap();
    let started = repository
        .start_node(
            "running-cancel-node",
            node.version,
            "running-cancel-instance",
        )
        .unwrap();
    let running = repository.task("task-running-cancel").unwrap();

    let cancelled = repository
        .cancel_task("task-running-cancel", running.version)
        .unwrap();
    assert_eq!(cancelled.state, TaskRunState::Cancelled);
    assert!(cancelled.cancel_requested);
    assert_eq!(
        repository
            .budget_reservation(&started.reservation.reservation_id)
            .unwrap()
            .state,
        BudgetReservationState::Active
    );

    let instance = repository
        .fail_node(
            "running-cancel-instance",
            started.instance.version,
            "worker observed cancellation",
            false,
        )
        .unwrap();
    assert_eq!(instance.state, task_engine::InstanceRunState::Cancelled);
    assert_eq!(
        repository
            .budget_reservation(&started.reservation.reservation_id)
            .unwrap()
            .state,
        BudgetReservationState::Released
    );
    assert_eq!(
        repository
            .events("task-running-cancel", 0)
            .unwrap()
            .iter()
            .filter(|event| event.kind == TaskEventKind::TaskCancelled)
            .count(),
        1
    );
}

fn admission(
    request_id: &str,
    provider: &str,
    task_run_id: &str,
    member_id: &str,
) -> AdmissionRequest {
    AdmissionRequest {
        request_id: request_id.into(),
        task_run_id: task_run_id.into(),
        room_id: Some("room-1".into()),
        member_id: Some(member_id.into()),
        provider: provider.into(),
        profile: "test.writer".into(),
    }
}

#[tokio::test]
async fn scheduler_grants_one_composite_lease_without_head_of_line_blocking() {
    let scheduler = Arc::new(
        Scheduler::new(SchedulerLimits {
            max_workers: 2,
            max_global: 2,
            max_per_room: 2,
            max_per_member: 1,
            max_per_provider: 1,
            max_per_profile: 2,
            max_per_task: 1,
        })
        .unwrap(),
    );
    let cancellation = CancellationToken::new();
    let first = scheduler
        .admit(
            admission("request-a", "provider-a", "task-a", "member-a"),
            cancellation.clone(),
        )
        .await
        .unwrap();

    let blocked_scheduler = Arc::clone(&scheduler);
    let blocked = tokio::spawn(async move {
        blocked_scheduler
            .admit(
                admission("request-b", "provider-a", "task-b", "member-b"),
                CancellationToken::new(),
            )
            .await
    });
    tokio::task::yield_now().await;

    let independent = tokio::time::timeout(
        Duration::from_millis(200),
        scheduler.admit(
            admission("request-c", "provider-b", "task-c", "member-c"),
            cancellation,
        ),
    )
    .await
    .expect("an independent provider must not wait behind a blocked provider")
    .unwrap();
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.active_workers, 2);
    assert_eq!(snapshot.waiting, 1);

    drop(independent);
    tokio::task::yield_now().await;
    assert!(!blocked.is_finished());
    drop(first);
    let admitted = tokio::time::timeout(Duration::from_millis(200), blocked)
        .await
        .expect("provider capacity should wake the blocked request")
        .unwrap()
        .unwrap();
    assert_eq!(admitted.request().request_id, "request-b");
    drop(admitted);
    assert_eq!(scheduler.snapshot().active_workers, 0);
}

#[tokio::test]
async fn cancelled_scheduler_wait_does_not_consume_capacity() {
    let scheduler = Arc::new(
        Scheduler::new(SchedulerLimits {
            max_workers: 1,
            max_global: 1,
            max_per_room: 1,
            max_per_member: 1,
            max_per_provider: 1,
            max_per_profile: 1,
            max_per_task: 1,
        })
        .unwrap(),
    );
    let held = scheduler
        .admit(
            admission("held", "provider-a", "task-a", "member-a"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    let waiting_scheduler = Arc::clone(&scheduler);
    let waiting_cancellation = cancellation.clone();
    let waiting = tokio::spawn(async move {
        waiting_scheduler
            .admit(
                admission("cancelled", "provider-b", "task-b", "member-b"),
                waiting_cancellation,
            )
            .await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();
    assert!(matches!(
        waiting.await.unwrap(),
        Err(TaskEngineError::Cancelled)
    ));
    assert_eq!(scheduler.snapshot().active_workers, 1);
    assert_eq!(scheduler.snapshot().waiting, 0);
    drop(held);
    assert_eq!(scheduler.snapshot().active_workers, 0);
}

#[tokio::test]
async fn coordinator_reserves_budget_only_after_composite_admission() {
    let (_directory, repository) = repository();
    let repository = Arc::new(repository);
    for (task_id, node_id, origin_id) in [
        ("task-coordinate-a", "coordinate-a", "coordinate-origin-a"),
        ("task-coordinate-b", "coordinate-b", "coordinate-origin-b"),
    ] {
        let mut request = task(task_id);
        request.origin_id = origin_id.into();
        request.nodes = vec![node(node_id, &[])];
        repository.create_task(request).unwrap();
    }
    let scheduler = Scheduler::new(SchedulerLimits {
        max_workers: 1,
        max_global: 1,
        max_per_room: 1,
        max_per_member: 1,
        max_per_provider: 1,
        max_per_profile: 1,
        max_per_task: 1,
    })
    .unwrap();
    let coordinator = Arc::new(TaskCoordinator::new(Arc::clone(&repository), scheduler));
    let first = coordinator
        .admit_node(
            "coordinate-a",
            "coordinate-instance-a",
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let waiting_coordinator = Arc::clone(&coordinator);
    let waiting = tokio::spawn(async move {
        waiting_coordinator
            .admit_node(
                "coordinate-b",
                "coordinate-instance-b",
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let waiting_task = repository.task("task-coordinate-b").unwrap();
    let waiting_budget = repository
        .budget_account(&waiting_task.budget_account_id)
        .unwrap();
    assert_eq!(waiting_budget.reserved_input_tokens, 0);
    assert_eq!(waiting_budget.reserved_output_tokens, 0);
    assert_eq!(
        repository.node("coordinate-b").unwrap().state,
        NodeState::Ready
    );
    assert_eq!(coordinator.scheduler().snapshot().active_workers, 1);
    assert_eq!(coordinator.scheduler().snapshot().waiting, 1);

    let (first_started, first_admission) = first.into_parts();
    drop(first_admission);
    let second = tokio::time::timeout(Duration::from_millis(200), waiting)
        .await
        .expect("second node should start after the composite lease is released")
        .unwrap()
        .unwrap();
    assert_eq!(second.started().node.state, NodeState::Running);
    assert!(
        repository
            .budget_account(&waiting_task.budget_account_id)
            .unwrap()
            .reserved_input_tokens
            > 0
    );

    repository
        .fail_node(
            &first_started.instance.instance_run_id,
            first_started.instance.version,
            "test cleanup",
            true,
        )
        .unwrap();
    let (second_started, second_admission) = second.into_parts();
    repository
        .fail_node(
            &second_started.instance.instance_run_id,
            second_started.instance.version,
            "test cleanup",
            true,
        )
        .unwrap();
    drop(second_admission);
}

#[tokio::test]
async fn coordinator_persists_paused_budget_without_holding_worker_capacity() {
    let (_directory, repository) = repository();
    repository
        .create_budget_account(NewBudgetAccount {
            budget_account_id: "small-parent".into(),
            parent_budget_account_id: None,
            owner_kind: "workspace".into(),
            owner_id: "small-workspace".into(),
            limits: BudgetLimits {
                input_tokens: 50,
                output_tokens: 50,
            },
        })
        .unwrap();
    let mut request = task("task-budget-paused");
    request.parent_budget_account_id = Some("small-parent".into());
    request.nodes = vec![node("budget-paused-node", &[])];
    repository.create_task(request).unwrap();
    let repository = Arc::new(repository);
    let coordinator = TaskCoordinator::new(
        Arc::clone(&repository),
        Scheduler::new(SchedulerLimits {
            max_workers: 1,
            max_global: 1,
            max_per_room: 1,
            max_per_member: 1,
            max_per_provider: 1,
            max_per_profile: 1,
            max_per_task: 1,
        })
        .unwrap(),
    );

    assert!(matches!(
        coordinator
            .admit_node(
                "budget-paused-node",
                "budget-paused-instance",
                CancellationToken::new(),
            )
            .await,
        Err(TaskEngineError::BudgetExceeded { .. })
    ));
    assert_eq!(
        repository.task("task-budget-paused").unwrap().state,
        TaskRunState::PausedBudget
    );
    assert_eq!(coordinator.scheduler().snapshot().active_workers, 0);
    assert_eq!(
        repository
            .budget_account("small-parent")
            .unwrap()
            .reserved_input_tokens,
        0
    );
}
