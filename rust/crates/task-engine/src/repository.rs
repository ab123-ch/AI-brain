use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    ActualUsage, BudgetAccount, BudgetLimits, BudgetRequest, BudgetReservation,
    BudgetReservationState, DurableTaskResult, InstanceRun, InstanceRunState, NewBudgetAccount,
    NewTaskRun, NodeCompletion, NodeKind, NodeState, RecoveryReport, Result, StartedNode,
    TaskArtifact, TaskEngineError, TaskEvent, TaskEventKind, TaskNode, TaskRun, TaskRunState,
};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy)]
enum FailureBudgetSettlement {
    Release,
    SettleAfterExecution(Option<ActualUsage>),
}

#[derive(Debug, Clone)]
pub struct TaskRepository {
    database_path: PathBuf,
}

impl TaskRepository {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref().to_path_buf();
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let repository = Self { database_path };
        repository.initialize()?;
        Ok(repository)
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.database_path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;\nPRAGMA synchronous = FULL;")?;
        Ok(connection)
    }

    #[allow(clippy::too_many_lines)]
    fn initialize(&self) -> Result<()> {
        let connection = self.connect()?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS task_engine_schema (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 version INTEGER NOT NULL
             );
             INSERT INTO task_engine_schema(singleton, version) VALUES (1, 1)
                 ON CONFLICT(singleton) DO NOTHING;

             CREATE TABLE IF NOT EXISTS task_config_snapshots (
                 config_snapshot_id TEXT PRIMARY KEY,
                 config_version TEXT NOT NULL,
                 resolved_config_json TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS budget_accounts (
                 budget_account_id TEXT PRIMARY KEY,
                 parent_budget_account_id TEXT REFERENCES budget_accounts(budget_account_id),
                 owner_kind TEXT NOT NULL,
                 owner_id TEXT NOT NULL,
                 input_limit INTEGER NOT NULL CHECK(input_limit >= 0),
                 output_limit INTEGER NOT NULL CHECK(output_limit >= 0),
                 reserved_input INTEGER NOT NULL DEFAULT 0 CHECK(reserved_input >= 0),
                 reserved_output INTEGER NOT NULL DEFAULT 0 CHECK(reserved_output >= 0),
                 consumed_input INTEGER NOT NULL DEFAULT 0 CHECK(consumed_input >= 0),
                 consumed_output INTEGER NOT NULL DEFAULT 0 CHECK(consumed_output >= 0),
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 UNIQUE(owner_kind, owner_id)
             );

             CREATE TABLE IF NOT EXISTS budget_reservations (
                 reservation_id TEXT PRIMARY KEY,
                 budget_account_id TEXT NOT NULL REFERENCES budget_accounts(budget_account_id),
                 idempotency_key TEXT NOT NULL,
                 reserved_input INTEGER NOT NULL CHECK(reserved_input >= 0),
                 reserved_output INTEGER NOT NULL CHECK(reserved_output >= 0),
                 actual_input INTEGER,
                 actual_output INTEGER,
                 state TEXT NOT NULL,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 settled_at TEXT,
                 UNIQUE(budget_account_id, idempotency_key)
             );
             CREATE INDEX IF NOT EXISTS budget_reservations_account_state_idx
                 ON budget_reservations(budget_account_id, state);

             CREATE TABLE IF NOT EXISTS budget_entries (
                 entry_id TEXT PRIMARY KEY,
                 budget_account_id TEXT NOT NULL REFERENCES budget_accounts(budget_account_id),
                 reservation_id TEXT NOT NULL REFERENCES budget_reservations(reservation_id),
                 kind TEXT NOT NULL,
                 reserved_input_delta INTEGER NOT NULL,
                 reserved_output_delta INTEGER NOT NULL,
                 consumed_input_delta INTEGER NOT NULL,
                 consumed_output_delta INTEGER NOT NULL,
                 idempotency_key TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(budget_account_id, idempotency_key)
             );

             CREATE TABLE IF NOT EXISTS task_runs (
                 task_run_id TEXT PRIMARY KEY,
                 workflow TEXT NOT NULL,
                 objective TEXT NOT NULL,
                 origin_kind TEXT NOT NULL,
                 origin_id TEXT NOT NULL,
                 room_id TEXT,
                 state TEXT NOT NULL,
                 version INTEGER NOT NULL DEFAULT 1,
                 config_snapshot_id TEXT NOT NULL REFERENCES task_config_snapshots(config_snapshot_id),
                 budget_account_id TEXT NOT NULL REFERENCES budget_accounts(budget_account_id),
                 latest_event_seq INTEGER NOT NULL DEFAULT 0,
                 cancel_requested INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 UNIQUE(origin_kind, origin_id)
             );
             CREATE INDEX IF NOT EXISTS task_runs_state_idx ON task_runs(state, created_at);

             CREATE TABLE IF NOT EXISTS task_nodes (
                 node_id TEXT PRIMARY KEY,
                 task_run_id TEXT NOT NULL REFERENCES task_runs(task_run_id) ON DELETE CASCADE,
                 kind TEXT NOT NULL,
                 state TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 profile TEXT NOT NULL,
                 room_id TEXT,
                 member_id TEXT,
                 reserve_input INTEGER NOT NULL CHECK(reserve_input >= 0),
                 reserve_output INTEGER NOT NULL CHECK(reserve_output >= 0),
                 retryable INTEGER NOT NULL,
                 side_effecting INTEGER NOT NULL,
                 current_instance_run_id TEXT,
                 output_artifact_id TEXT,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS task_nodes_ready_idx
                 ON task_nodes(state, created_at, node_id);
             CREATE INDEX IF NOT EXISTS task_nodes_task_idx
                 ON task_nodes(task_run_id, created_at, node_id);

             CREATE TABLE IF NOT EXISTS task_node_dependencies (
                 task_run_id TEXT NOT NULL REFERENCES task_runs(task_run_id) ON DELETE CASCADE,
                 node_id TEXT NOT NULL REFERENCES task_nodes(node_id) ON DELETE CASCADE,
                 depends_on_node_id TEXT NOT NULL REFERENCES task_nodes(node_id) ON DELETE CASCADE,
                 PRIMARY KEY(node_id, depends_on_node_id),
                 CHECK(node_id <> depends_on_node_id)
             );
             CREATE INDEX IF NOT EXISTS task_node_dependencies_parent_idx
                 ON task_node_dependencies(depends_on_node_id, node_id);

             CREATE TABLE IF NOT EXISTS instance_runs (
                 instance_run_id TEXT PRIMARY KEY,
                 task_run_id TEXT NOT NULL REFERENCES task_runs(task_run_id),
                 node_id TEXT NOT NULL REFERENCES task_nodes(node_id),
                 reservation_id TEXT NOT NULL REFERENCES budget_reservations(reservation_id),
                 state TEXT NOT NULL,
                 artifact_id TEXT,
                 error TEXT,
                 input_tokens INTEGER,
                 output_tokens INTEGER,
                 version INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 completed_at TEXT
             );
             CREATE INDEX IF NOT EXISTS instance_runs_state_idx
                 ON instance_runs(state, created_at);
             CREATE INDEX IF NOT EXISTS instance_runs_node_idx
                 ON instance_runs(node_id, created_at);

             CREATE TABLE IF NOT EXISTS task_artifacts (
                 artifact_id TEXT PRIMARY KEY,
                 task_run_id TEXT NOT NULL REFERENCES task_runs(task_run_id),
                 node_id TEXT NOT NULL REFERENCES task_nodes(node_id),
                 instance_run_id TEXT NOT NULL REFERENCES instance_runs(instance_run_id),
                 content TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 media_type TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(instance_run_id, content_hash)
             );
             CREATE INDEX IF NOT EXISTS task_artifacts_task_idx
                 ON task_artifacts(task_run_id, created_at);

             CREATE TABLE IF NOT EXISTS task_events (
                 event_id TEXT PRIMARY KEY,
                 task_run_id TEXT NOT NULL REFERENCES task_runs(task_run_id) ON DELETE CASCADE,
                 sequence INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 payload_json TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(task_run_id, sequence)
             );
             CREATE INDEX IF NOT EXISTS task_events_task_seq_idx
                 ON task_events(task_run_id, sequence);",
        )?;
        let version: u32 = connection.query_row(
            "SELECT version FROM task_engine_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if version != SCHEMA_VERSION {
            return Err(TaskEngineError::Invalid(format!(
                "unsupported task engine schema version {version}"
            )));
        }
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    pub fn create_task(&self, request: NewTaskRun) -> Result<TaskRun> {
        validate_task(&request)?;
        let config_json = serde_json::to_string(&request.resolved_config).map_err(|error| {
            TaskEngineError::Invalid(format!("invalid resolved config: {error}"))
        })?;
        let config_hash = sha256_hex(config_json.as_bytes());
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing_id) = transaction
            .query_row(
                "SELECT task_run_id FROM task_runs WHERE origin_kind = ?1 AND origin_id = ?2",
                params![request.origin_kind, request.origin_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let existing = task_from_connection(&transaction, &existing_id)?;
            if existing.task_run_id != request.task_run_id
                || existing.config_content_hash != config_hash
                || existing.objective != request.objective
            {
                return Err(TaskEngineError::Invalid(format!(
                    "task origin `{}/{}` is already bound to incompatible task `{}`",
                    request.origin_kind, request.origin_id, existing.task_run_id
                )));
            }
            transaction.commit()?;
            return Ok(existing);
        }

        if transaction
            .query_row(
                "SELECT 1 FROM task_runs WHERE task_run_id = ?1",
                [&request.task_run_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(TaskEngineError::Invalid(format!(
                "task_run_id `{}` already exists",
                request.task_run_id
            )));
        }

        let now = Utc::now().to_rfc3339();
        let config_snapshot_id = format!("config-{}", request.task_run_id);
        let budget_account_id = format!("budget-{}", request.task_run_id);
        transaction.execute(
            "INSERT INTO task_config_snapshots(
                 config_snapshot_id, config_version, resolved_config_json, content_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                config_snapshot_id,
                request.config_version,
                config_json,
                config_hash,
                now,
            ],
        )?;
        insert_budget_account(
            &transaction,
            &NewBudgetAccount {
                budget_account_id: budget_account_id.clone(),
                parent_budget_account_id: request.parent_budget_account_id.clone(),
                owner_kind: "task".into(),
                owner_id: request.task_run_id.clone(),
                limits: request.budget,
            },
            &now,
        )?;
        transaction.execute(
            "INSERT INTO task_runs(
                 task_run_id, workflow, objective, origin_kind, origin_id, room_id,
                 state, version, config_snapshot_id, budget_account_id,
                 latest_event_seq, cancel_requested, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', 1, ?7, ?8, 0, 0, ?9, ?9)",
            params![
                request.task_run_id,
                request.workflow,
                request.objective,
                request.origin_kind,
                request.origin_id,
                request.room_id,
                config_snapshot_id,
                budget_account_id,
                now,
            ],
        )?;
        for node in &request.nodes {
            let state = if node.dependencies.is_empty() {
                NodeState::Ready
            } else {
                NodeState::WaitingDependency
            };
            transaction.execute(
                "INSERT INTO task_nodes(
                     node_id, task_run_id, kind, state, provider, model, profile,
                     room_id, member_id, reserve_input, reserve_output, retryable,
                     side_effecting, current_instance_run_id, output_artifact_id,
                     version, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, NULL, NULL, 1, ?14, ?14)",
                params![
                    node.node_id,
                    request.task_run_id,
                    node.kind.as_db(),
                    state.as_db(),
                    node.provider,
                    node.model,
                    node.profile,
                    node.room_id,
                    node.member_id,
                    node.reservation.input_tokens,
                    node.reservation.output_tokens,
                    node.retryable,
                    node.side_effecting,
                    now,
                ],
            )?;
        }
        for node in &request.nodes {
            for dependency in &node.dependencies {
                transaction.execute(
                    "INSERT INTO task_node_dependencies(task_run_id, node_id, depends_on_node_id)
                     VALUES (?1, ?2, ?3)",
                    params![request.task_run_id, node.node_id, dependency],
                )?;
            }
        }
        append_event(
            &transaction,
            &request.task_run_id,
            TaskEventKind::TaskCreated,
            &json!({
                "workflow": request.workflow,
                "config_snapshot_id": config_snapshot_id,
                "config_content_hash": config_hash
            }),
        )?;
        let created = task_from_connection(&transaction, &request.task_run_id)?;
        transaction.commit()?;
        Ok(created)
    }

    pub fn task(&self, task_run_id: &str) -> Result<TaskRun> {
        let connection = self.connect()?;
        task_from_connection(&connection, task_run_id)
    }

    pub fn node(&self, node_id: &str) -> Result<TaskNode> {
        let connection = self.connect()?;
        node_from_connection(&connection, node_id)
    }

    pub fn nodes(&self, task_run_id: &str) -> Result<Vec<TaskNode>> {
        let connection = self.connect()?;
        node_ids_for_task(&connection, task_run_id)?
            .into_iter()
            .map(|node_id| node_from_connection(&connection, &node_id))
            .collect()
    }

    pub fn ready_nodes(&self, limit: usize) -> Result<Vec<TaskNode>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT n.node_id
             FROM task_nodes n
             JOIN task_runs t ON t.task_run_id = n.task_run_id
             WHERE n.state = 'ready' AND t.state IN ('queued', 'running')
               AND t.cancel_requested = 0
             ORDER BY n.created_at, n.node_id
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        let ids = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        ids.into_iter()
            .map(|node_id| node_from_connection(&connection, &node_id))
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    pub fn start_node(
        &self,
        node_id: &str,
        expected_version: u64,
        instance_run_id: &str,
    ) -> Result<StartedNode> {
        require_non_empty("instance_run_id", instance_run_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if instance_exists(&transaction, instance_run_id)? {
            let instance = instance_from_connection(&transaction, instance_run_id)?;
            let node = node_from_connection(&transaction, &instance.node_id)?;
            if instance.node_id == node_id
                && instance.state == InstanceRunState::Running
                && node.current_instance_run_id.as_deref() == Some(instance_run_id)
            {
                let task = task_from_connection(&transaction, &instance.task_run_id)?;
                let reservation =
                    reservation_from_connection(&transaction, &instance.reservation_id)?;
                transaction.commit()?;
                return Ok(StartedNode {
                    task,
                    node,
                    instance,
                    reservation,
                });
            }
            return Err(TaskEngineError::Invalid(format!(
                "instance_run_id `{instance_run_id}` is already bound to another execution"
            )));
        }

        let node = node_from_connection(&transaction, node_id)?;
        ensure_version("task node", node_id, expected_version, node.version)?;
        if node.state != NodeState::Ready {
            return Err(invalid_transition(
                "task node",
                node_id,
                node.state.as_db(),
                NodeState::Running.as_db(),
            ));
        }
        let task = task_from_connection(&transaction, &node.task_run_id)?;
        if task.cancel_requested || task.state.is_terminal() {
            return Err(invalid_transition(
                "task run",
                &task.task_run_id,
                task.state.as_db(),
                TaskRunState::Running.as_db(),
            ));
        }

        let reservation_id = format!("reservation-{instance_run_id}");
        let reservation = reserve_budget_tx(
            &transaction,
            &task.budget_account_id,
            &reservation_id,
            &format!("instance:{instance_run_id}"),
            node.reservation,
        )?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO instance_runs(
                 instance_run_id, task_run_id, node_id, reservation_id, state,
                 artifact_id, error, input_tokens, output_tokens, version, created_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, 'running', NULL, NULL, NULL, NULL, 1, ?5, NULL)",
            params![
                instance_run_id,
                node.task_run_id,
                node.node_id,
                reservation_id,
                now,
            ],
        )?;
        let updated = transaction.execute(
            "UPDATE task_nodes
             SET state = 'running', current_instance_run_id = ?1,
                 version = version + 1, updated_at = ?2
             WHERE node_id = ?3 AND state = 'ready' AND version = ?4",
            params![instance_run_id, now, node_id, expected_version],
        )?;
        if updated != 1 {
            let actual = node_version(&transaction, node_id)?;
            return Err(TaskEngineError::CasConflict {
                entity: "task node",
                id: node_id.into(),
                expected: expected_version,
                actual,
            });
        }
        if task.state == TaskRunState::Queued {
            let updated = transaction.execute(
                "UPDATE task_runs
                 SET state = 'running', version = version + 1, updated_at = ?1
                 WHERE task_run_id = ?2 AND state = 'queued' AND version = ?3",
                params![now, task.task_run_id, task.version],
            )?;
            if updated != 1 {
                let actual = task_version(&transaction, &task.task_run_id)?;
                return Err(TaskEngineError::CasConflict {
                    entity: "task run",
                    id: task.task_run_id,
                    expected: task.version,
                    actual,
                });
            }
            append_event(
                &transaction,
                &node.task_run_id,
                TaskEventKind::TaskRunning,
                &json!({"instance_run_id": instance_run_id}),
            )?;
        }
        append_event(
            &transaction,
            &node.task_run_id,
            TaskEventKind::NodeStarted,
            &json!({
                "node_id": node_id,
                "instance_run_id": instance_run_id,
                "reservation_id": reservation_id
            }),
        )?;

        let result = StartedNode {
            task: task_from_connection(&transaction, &node.task_run_id)?,
            node: node_from_connection(&transaction, node_id)?,
            instance: instance_from_connection(&transaction, instance_run_id)?,
            reservation,
        };
        transaction.commit()?;
        Ok(result)
    }

    #[allow(clippy::too_many_lines)]
    pub fn complete_node(
        &self,
        instance_run_id: &str,
        expected_version: u64,
        usage: ActualUsage,
        artifact_id: Option<&str>,
    ) -> Result<NodeCompletion> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let instance = instance_from_connection(&transaction, instance_run_id)?;
        if instance.state == InstanceRunState::Succeeded {
            if instance.usage == Some(usage) && instance.artifact_id.as_deref() == artifact_id {
                let node = node_from_connection(&transaction, &instance.node_id)?;
                let task = task_from_connection(&transaction, &instance.task_run_id)?;
                let reservation =
                    reservation_from_connection(&transaction, &instance.reservation_id)?;
                transaction.commit()?;
                return Ok(NodeCompletion {
                    task,
                    node,
                    instance,
                    reservation,
                    newly_ready_node_ids: Vec::new(),
                });
            }
            return Err(invalid_transition(
                "instance run",
                instance_run_id,
                instance.state.as_db(),
                InstanceRunState::Succeeded.as_db(),
            ));
        }
        ensure_version(
            "instance run",
            instance_run_id,
            expected_version,
            instance.version,
        )?;
        if instance.state != InstanceRunState::Running {
            return Err(invalid_transition(
                "instance run",
                instance_run_id,
                instance.state.as_db(),
                InstanceRunState::Succeeded.as_db(),
            ));
        }
        let task = task_from_connection(&transaction, &instance.task_run_id)?;
        if task.cancel_requested || task.state == TaskRunState::Cancelled {
            return Err(invalid_transition(
                "task run",
                &task.task_run_id,
                task.state.as_db(),
                TaskRunState::Completed.as_db(),
            ));
        }
        let reservation = reservation_from_connection(&transaction, &instance.reservation_id)?;
        let settled = settle_budget_tx(&transaction, &reservation, reservation.version, usage)?;
        let now = Utc::now().to_rfc3339();
        let updated = transaction.execute(
            "UPDATE instance_runs
             SET state = 'succeeded', artifact_id = ?1, input_tokens = ?2,
                 output_tokens = ?3, version = version + 1, completed_at = ?4
             WHERE instance_run_id = ?5 AND state = 'running' AND version = ?6",
            params![
                artifact_id,
                usage.input_tokens,
                usage.output_tokens,
                now,
                instance_run_id,
                expected_version,
            ],
        )?;
        if updated != 1 {
            let actual = instance_version(&transaction, instance_run_id)?;
            return Err(TaskEngineError::CasConflict {
                entity: "instance run",
                id: instance_run_id.into(),
                expected: expected_version,
                actual,
            });
        }
        transaction.execute(
            "UPDATE task_nodes
             SET state = 'completed', output_artifact_id = ?1,
                 version = version + 1, updated_at = ?2
             WHERE node_id = ?3 AND state = 'running' AND current_instance_run_id = ?4",
            params![artifact_id, now, instance.node_id, instance_run_id],
        )?;
        append_event(
            &transaction,
            &instance.task_run_id,
            TaskEventKind::NodeCompleted,
            &json!({
                "node_id": instance.node_id,
                "instance_run_id": instance_run_id,
                "artifact_id": artifact_id,
                "usage": usage
            }),
        )?;

        let newly_ready = dependency_ready_node_ids(&transaction, &instance.task_run_id)?;
        for node_id in &newly_ready {
            transaction.execute(
                "UPDATE task_nodes
                 SET state = 'ready', version = version + 1, updated_at = ?1
                 WHERE node_id = ?2 AND state = 'waiting_dependency'",
                params![now, node_id],
            )?;
            append_event(
                &transaction,
                &instance.task_run_id,
                TaskEventKind::NodeReady,
                &json!({"node_id": node_id}),
            )?;
        }

        let incomplete: usize = transaction.query_row(
            "SELECT COUNT(*) FROM task_nodes
             WHERE task_run_id = ?1 AND state <> 'completed'",
            [&instance.task_run_id],
            |row| row.get(0),
        )?;
        if incomplete == 0 {
            let current = task_from_connection(&transaction, &instance.task_run_id)?;
            let updated = transaction.execute(
                "UPDATE task_runs
                 SET state = 'completed', version = version + 1, updated_at = ?1
                 WHERE task_run_id = ?2 AND state = 'running' AND version = ?3",
                params![now, instance.task_run_id, current.version],
            )?;
            if updated != 1 {
                let actual = task_version(&transaction, &instance.task_run_id)?;
                return Err(TaskEngineError::CasConflict {
                    entity: "task run",
                    id: instance.task_run_id,
                    expected: current.version,
                    actual,
                });
            }
            append_event(
                &transaction,
                &current.task_run_id,
                TaskEventKind::TaskCompleted,
                &json!({"final_node_id": instance.node_id, "artifact_id": artifact_id}),
            )?;
        }

        let result = NodeCompletion {
            task: task_from_connection(&transaction, &instance.task_run_id)?,
            node: node_from_connection(&transaction, &instance.node_id)?,
            instance: instance_from_connection(&transaction, instance_run_id)?,
            reservation: settled,
            newly_ready_node_ids: newly_ready,
        };
        transaction.commit()?;
        Ok(result)
    }

    pub fn pause_task_for_budget(&self, task_run_id: &str, reason: &str) -> Result<TaskRun> {
        require_non_empty("budget pause reason", reason)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = task_from_connection(&transaction, task_run_id)?;
        if task.state == TaskRunState::PausedBudget {
            transaction.commit()?;
            return Ok(task);
        }
        if !matches!(task.state, TaskRunState::Queued | TaskRunState::Running) {
            return Err(invalid_transition(
                "task run",
                task_run_id,
                task.state.as_db(),
                TaskRunState::PausedBudget.as_db(),
            ));
        }
        let updated = transaction.execute(
            "UPDATE task_runs
             SET state = 'paused_budget', version = version + 1, updated_at = ?1
             WHERE task_run_id = ?2 AND version = ?3 AND state IN ('queued', 'running')",
            params![Utc::now().to_rfc3339(), task_run_id, task.version],
        )?;
        if updated != 1 {
            return Err(TaskEngineError::CasConflict {
                entity: "task run",
                id: task_run_id.into(),
                expected: task.version,
                actual: task_version(&transaction, task_run_id)?,
            });
        }
        append_event(
            &transaction,
            task_run_id,
            TaskEventKind::TaskPausedBudget,
            &json!({"reason": reason}),
        )?;
        let paused = task_from_connection(&transaction, task_run_id)?;
        transaction.commit()?;
        Ok(paused)
    }

    pub fn cancel_task(&self, task_run_id: &str, expected_version: u64) -> Result<TaskRun> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = task_from_connection(&transaction, task_run_id)?;
        if task.state == TaskRunState::Cancelled {
            transaction.commit()?;
            return Ok(task);
        }
        ensure_version("task run", task_run_id, expected_version, task.version)?;
        if task.state.is_terminal() {
            return Err(invalid_transition(
                "task run",
                task_run_id,
                task.state.as_db(),
                TaskRunState::Cancelled.as_db(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        let updated = transaction.execute(
            "UPDATE task_runs
             SET state = 'cancelled', cancel_requested = 1,
                 version = version + 1, updated_at = ?1
             WHERE task_run_id = ?2 AND version = ?3 AND cancel_requested = 0",
            params![now, task_run_id, expected_version],
        )?;
        if updated != 1 {
            return Err(TaskEngineError::CasConflict {
                entity: "task run",
                id: task_run_id.into(),
                expected: expected_version,
                actual: task_version(&transaction, task_run_id)?,
            });
        }
        transaction.execute(
            "UPDATE task_nodes
             SET state = 'cancelled', version = version + 1, updated_at = ?1
             WHERE task_run_id = ?2 AND state IN ('waiting_dependency', 'ready')",
            params![now, task_run_id],
        )?;
        append_event(
            &transaction,
            task_run_id,
            TaskEventKind::TaskCancelled,
            &json!({"expected_version": expected_version}),
        )?;
        let cancelled = task_from_connection(&transaction, task_run_id)?;
        transaction.commit()?;
        Ok(cancelled)
    }

    pub fn store_artifact(
        &self,
        instance_run_id: &str,
        content: &str,
        media_type: &str,
    ) -> Result<TaskArtifact> {
        require_non_empty("artifact content", content)?;
        require_non_empty("artifact media_type", media_type)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let instance = instance_from_connection(&transaction, instance_run_id)?;
        if instance.state != InstanceRunState::Running
            && instance.state != InstanceRunState::Succeeded
        {
            return Err(invalid_transition(
                "instance run",
                instance_run_id,
                instance.state.as_db(),
                "artifact_stored",
            ));
        }
        let content_hash = sha256_hex(content.as_bytes());
        if let Some(artifact_id) = transaction
            .query_row(
                "SELECT artifact_id FROM task_artifacts
                 WHERE instance_run_id = ?1 AND content_hash = ?2",
                params![instance_run_id, content_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let artifact = artifact_from_connection(&transaction, &artifact_id)?;
            transaction.commit()?;
            return Ok(artifact);
        }
        let artifact_id = format!("artifact-{instance_run_id}-{}", &content_hash[..12]);
        transaction.execute(
            "INSERT INTO task_artifacts(
                 artifact_id, task_run_id, node_id, instance_run_id,
                 content, content_hash, media_type, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                artifact_id,
                instance.task_run_id,
                instance.node_id,
                instance_run_id,
                content,
                content_hash,
                media_type,
                Utc::now().to_rfc3339(),
            ],
        )?;
        let artifact = artifact_from_connection(&transaction, &artifact_id)?;
        transaction.commit()?;
        Ok(artifact)
    }

    pub fn artifact(&self, artifact_id: &str) -> Result<TaskArtifact> {
        let connection = self.connect()?;
        artifact_from_connection(&connection, artifact_id)
    }

    pub fn instance(&self, instance_run_id: &str) -> Result<InstanceRun> {
        let connection = self.connect()?;
        instance_from_connection(&connection, instance_run_id)
    }

    pub fn completed_results(&self, origin_kind: &str) -> Result<Vec<DurableTaskResult>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT t.task_run_id, t.origin_kind, t.origin_id,
                    i.instance_run_id, a.artifact_id
             FROM task_runs t
             JOIN task_nodes n ON n.task_run_id = t.task_run_id
             JOIN instance_runs i ON i.instance_run_id = n.current_instance_run_id
             JOIN task_artifacts a ON a.artifact_id = n.output_artifact_id
             WHERE t.origin_kind = ?1 AND t.state = 'completed'
               AND n.state = 'completed' AND i.state = 'succeeded'
             ORDER BY t.created_at, t.task_run_id, n.created_at",
        )?;
        let rows = statement.query_map([origin_kind], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let raw = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        raw.into_iter()
            .map(
                |(task_run_id, origin_kind, origin_id, instance_run_id, artifact_id)| {
                    Ok(DurableTaskResult {
                        task_run_id,
                        origin_kind,
                        origin_id,
                        instance_run_id,
                        artifact: artifact_from_connection(&connection, &artifact_id)?,
                    })
                },
            )
            .collect()
    }

    pub fn fail_node(
        &self,
        instance_run_id: &str,
        expected_version: u64,
        error: &str,
        cancelled: bool,
    ) -> Result<InstanceRun> {
        self.finish_failed_node(
            instance_run_id,
            expected_version,
            error,
            cancelled,
            FailureBudgetSettlement::Release,
        )
    }

    pub fn fail_node_after_execution(
        &self,
        instance_run_id: &str,
        expected_version: u64,
        error: &str,
        usage: Option<ActualUsage>,
        cancelled: bool,
    ) -> Result<InstanceRun> {
        self.finish_failed_node(
            instance_run_id,
            expected_version,
            error,
            cancelled,
            FailureBudgetSettlement::SettleAfterExecution(usage),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn finish_failed_node(
        &self,
        instance_run_id: &str,
        expected_version: u64,
        error: &str,
        cancelled: bool,
        budget_settlement: FailureBudgetSettlement,
    ) -> Result<InstanceRun> {
        require_non_empty("instance failure", error)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let instance = instance_from_connection(&transaction, instance_run_id)?;
        if instance.state != InstanceRunState::Running {
            transaction.commit()?;
            return Ok(instance);
        }
        ensure_version(
            "instance run",
            instance_run_id,
            expected_version,
            instance.version,
        )?;
        let reservation = reservation_from_connection(&transaction, &instance.reservation_id)?;
        let (usage, usage_basis) = match budget_settlement {
            FailureBudgetSettlement::Release => {
                if reservation.state == BudgetReservationState::Active {
                    release_budget_tx(&transaction, &reservation, reservation.version)?;
                }
                (None, None)
            }
            FailureBudgetSettlement::SettleAfterExecution(actual_usage) => {
                let (usage, basis) = actual_usage.map_or_else(
                    || {
                        (
                            ActualUsage {
                                input_tokens: reservation.reserved.input_tokens,
                                output_tokens: reservation.reserved.output_tokens,
                            },
                            "reservation_upper_bound",
                        )
                    },
                    |usage| (usage, "runtime_reported"),
                );
                if reservation.state == BudgetReservationState::Active {
                    settle_budget_tx(&transaction, &reservation, reservation.version, usage)?;
                } else if reservation.state != BudgetReservationState::Settled
                    || reservation.actual != Some(usage)
                {
                    return Err(invalid_transition(
                        "budget reservation",
                        &reservation.reservation_id,
                        reservation.state.as_db(),
                        BudgetReservationState::Settled.as_db(),
                    ));
                }
                (Some(usage), Some(basis))
            }
        };
        let current_task = task_from_connection(&transaction, &instance.task_run_id)?;
        let cancelled = cancelled
            || current_task.cancel_requested
            || current_task.state == TaskRunState::Cancelled;
        let now = Utc::now().to_rfc3339();
        let instance_state = if cancelled {
            InstanceRunState::Cancelled
        } else {
            InstanceRunState::Failed
        };
        let node_state = if cancelled {
            NodeState::Cancelled
        } else {
            NodeState::Failed
        };
        let task_state = if cancelled {
            TaskRunState::Cancelled
        } else {
            TaskRunState::Failed
        };
        transaction.execute(
            "UPDATE instance_runs
             SET state = ?1, error = ?2, input_tokens = ?3, output_tokens = ?4,
                 version = version + 1, completed_at = ?5
             WHERE instance_run_id = ?6 AND state = 'running' AND version = ?7",
            params![
                instance_state.as_db(),
                error,
                usage.map(|value| value.input_tokens),
                usage.map(|value| value.output_tokens),
                now,
                instance_run_id,
                expected_version,
            ],
        )?;
        transaction.execute(
            "UPDATE task_nodes
             SET state = ?1, version = version + 1, updated_at = ?2
             WHERE node_id = ?3 AND state = 'running' AND current_instance_run_id = ?4",
            params![node_state.as_db(), now, instance.node_id, instance_run_id],
        )?;
        let task_changed =
            current_task.state != task_state || current_task.cancel_requested != cancelled;
        if task_changed {
            transaction.execute(
                "UPDATE task_runs
                 SET state = ?1, cancel_requested = ?2, version = version + 1, updated_at = ?3
                 WHERE task_run_id = ?4 AND version = ?5",
                params![
                    task_state.as_db(),
                    cancelled,
                    now,
                    instance.task_run_id,
                    current_task.version,
                ],
            )?;
        }
        append_event(
            &transaction,
            &instance.task_run_id,
            if cancelled {
                TaskEventKind::NodeCancelled
            } else {
                TaskEventKind::NodeFailed
            },
            &json!({
                "node_id": instance.node_id,
                "instance_run_id": instance_run_id,
                "error": error,
                "usage": usage,
                "usage_basis": usage_basis
            }),
        )?;
        if task_changed {
            append_event(
                &transaction,
                &instance.task_run_id,
                if cancelled {
                    TaskEventKind::TaskCancelled
                } else {
                    TaskEventKind::TaskFailed
                },
                &json!({
                    "instance_run_id": instance_run_id,
                    "error": error,
                    "usage": usage,
                    "usage_basis": usage_basis
                }),
            )?;
        }
        let failed = instance_from_connection(&transaction, instance_run_id)?;
        transaction.commit()?;
        Ok(failed)
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn create_budget_account(&self, request: NewBudgetAccount) -> Result<BudgetAccount> {
        validate_budget_account(&request)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = budget_account_optional(&transaction, &request.budget_account_id)? {
            if existing.parent_budget_account_id == request.parent_budget_account_id
                && existing.owner_kind == request.owner_kind
                && existing.owner_id == request.owner_id
                && existing.limits == request.limits
            {
                transaction.commit()?;
                return Ok(existing);
            }
            return Err(TaskEngineError::Invalid(format!(
                "budget account `{}` already exists with different configuration",
                request.budget_account_id
            )));
        }
        let now = Utc::now().to_rfc3339();
        insert_budget_account(&transaction, &request, &now)?;
        let account = budget_account_from_connection(&transaction, &request.budget_account_id)?;
        transaction.commit()?;
        Ok(account)
    }

    pub fn budget_account(&self, budget_account_id: &str) -> Result<BudgetAccount> {
        let connection = self.connect()?;
        budget_account_from_connection(&connection, budget_account_id)
    }

    pub fn budget_reservation(&self, reservation_id: &str) -> Result<BudgetReservation> {
        let connection = self.connect()?;
        reservation_from_connection(&connection, reservation_id)
    }

    pub fn reserve_budget(
        &self,
        budget_account_id: &str,
        reservation_id: &str,
        idempotency_key: &str,
        request: BudgetRequest,
    ) -> Result<BudgetReservation> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reservation = reserve_budget_tx(
            &transaction,
            budget_account_id,
            reservation_id,
            idempotency_key,
            request,
        )?;
        transaction.commit()?;
        Ok(reservation)
    }

    pub fn settle_budget(
        &self,
        reservation_id: &str,
        expected_version: u64,
        usage: ActualUsage,
    ) -> Result<BudgetReservation> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reservation = reservation_from_connection(&transaction, reservation_id)?;
        let settled = settle_budget_tx(&transaction, &reservation, expected_version, usage)?;
        transaction.commit()?;
        Ok(settled)
    }

    pub fn release_budget(
        &self,
        reservation_id: &str,
        expected_version: u64,
    ) -> Result<BudgetReservation> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reservation = reservation_from_connection(&transaction, reservation_id)?;
        let released = release_budget_tx(&transaction, &reservation, expected_version)?;
        transaction.commit()?;
        Ok(released)
    }

    pub fn events(&self, task_run_id: &str, after_sequence: u64) -> Result<Vec<TaskEvent>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT event_id, task_run_id, sequence, kind, payload_json, created_at
             FROM task_events
             WHERE task_run_id = ?1 AND sequence > ?2
             ORDER BY sequence",
        )?;
        let rows = statement.query_map(params![task_run_id, after_sequence], map_task_event)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn recover_inflight(&self) -> Result<RecoveryReport> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT instance_run_id FROM instance_runs
             WHERE state = 'running' ORDER BY created_at, instance_run_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let instance_ids = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);

        let mut report = RecoveryReport::default();
        for instance_run_id in instance_ids {
            let instance = instance_from_connection(&transaction, &instance_run_id)?;
            let node = node_from_connection(&transaction, &instance.node_id)?;
            let task = task_from_connection(&transaction, &instance.task_run_id)?;
            let reservation = reservation_from_connection(&transaction, &instance.reservation_id)?;
            if reservation.state == BudgetReservationState::Active {
                release_budget_tx(&transaction, &reservation, reservation.version)?;
            }
            let now = Utc::now().to_rfc3339();
            transaction.execute(
                "UPDATE instance_runs
                 SET state = 'interrupted', error = ?1, version = version + 1,
                     completed_at = ?2
                 WHERE instance_run_id = ?3 AND state = 'running' AND version = ?4",
                params![
                    "process interrupted before durable completion",
                    now,
                    instance_run_id,
                    instance.version,
                ],
            )?;
            report.interrupted += 1;

            let (node_state, task_state, event_kind) =
                if task.cancel_requested || task.state == TaskRunState::Cancelled {
                    report.cancelled += 1;
                    (
                        NodeState::Cancelled,
                        TaskRunState::Cancelled,
                        TaskEventKind::TaskCancelled,
                    )
                } else if node.retryable && !node.side_effecting {
                    report.requeued += 1;
                    (
                        NodeState::Ready,
                        TaskRunState::Queued,
                        TaskEventKind::NodeInterrupted,
                    )
                } else {
                    report.needs_input += 1;
                    (
                        NodeState::NeedsInput,
                        TaskRunState::NeedsInput,
                        TaskEventKind::TaskNeedsInput,
                    )
                };
            transaction.execute(
                "UPDATE task_nodes
                 SET state = ?1, current_instance_run_id = NULL,
                     version = version + 1, updated_at = ?2
                 WHERE node_id = ?3 AND state = 'running' AND version = ?4",
                params![node_state.as_db(), now, node.node_id, node.version],
            )?;
            let current_task_version = task_version(&transaction, &task.task_run_id)?;
            transaction.execute(
                "UPDATE task_runs
                 SET state = ?1, version = version + 1, updated_at = ?2
                 WHERE task_run_id = ?3 AND version = ?4",
                params![
                    task_state.as_db(),
                    now,
                    task.task_run_id,
                    current_task_version,
                ],
            )?;
            append_event(
                &transaction,
                &task.task_run_id,
                event_kind,
                &json!({
                    "node_id": node.node_id,
                    "instance_run_id": instance_run_id,
                    "recovered_state": node_state
                }),
            )?;
        }
        transaction.commit()?;
        Ok(report)
    }
}

fn validate_task(request: &NewTaskRun) -> Result<()> {
    for (field, value) in [
        ("task_run_id", request.task_run_id.as_str()),
        ("workflow", request.workflow.as_str()),
        ("objective", request.objective.as_str()),
        ("origin_kind", request.origin_kind.as_str()),
        ("origin_id", request.origin_id.as_str()),
        ("config_version", request.config_version.as_str()),
    ] {
        require_non_empty(field, value)?;
    }
    if request.nodes.is_empty() {
        return Err(TaskEngineError::Invalid(
            "task must contain at least one node".into(),
        ));
    }
    if request.budget.input_tokens == 0 || request.budget.output_tokens == 0 {
        return Err(TaskEngineError::Invalid(
            "task input and output budget limits must be greater than zero".into(),
        ));
    }

    let mut node_ids = BTreeSet::new();
    for node in &request.nodes {
        for (field, value) in [
            ("node_id", node.node_id.as_str()),
            ("provider", node.provider.as_str()),
            ("model", node.model.as_str()),
            ("profile", node.profile.as_str()),
        ] {
            require_non_empty(field, value)?;
        }
        if !node_ids.insert(node.node_id.clone()) {
            return Err(TaskEngineError::Invalid(format!(
                "duplicate task node `{}`",
                node.node_id
            )));
        }
        if node.kind != NodeKind::Deterministic
            && (node.reservation.input_tokens == 0 || node.reservation.output_tokens == 0)
        {
            return Err(TaskEngineError::Invalid(format!(
                "model node `{}` requires non-zero input and output reservations",
                node.node_id
            )));
        }
    }
    for node in &request.nodes {
        for dependency in &node.dependencies {
            if !node_ids.contains(dependency) {
                return Err(TaskEngineError::Invalid(format!(
                    "node `{}` depends on missing node `{dependency}`",
                    node.node_id
                )));
            }
            if dependency == &node.node_id {
                return Err(TaskEngineError::Invalid(format!(
                    "node `{}` cannot depend on itself",
                    node.node_id
                )));
            }
        }
    }
    validate_acyclic(request)
}

fn validate_acyclic(request: &NewTaskRun) -> Result<()> {
    let mut indegree = BTreeMap::<String, usize>::new();
    let mut dependents = BTreeMap::<String, Vec<String>>::new();
    for node in &request.nodes {
        indegree.insert(node.node_id.clone(), node.dependencies.len());
        for dependency in &node.dependencies {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(node.node_id.clone());
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(node_id, _)| node_id.clone())
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(node_id) = ready.pop_front() {
        visited += 1;
        for dependent in dependents.get(&node_id).into_iter().flatten() {
            let degree = indegree
                .get_mut(dependent)
                .expect("validated dependency node exists");
            *degree -= 1;
            if *degree == 0 {
                ready.push_back(dependent.clone());
            }
        }
    }
    if visited != request.nodes.len() {
        return Err(TaskEngineError::Invalid("task DAG contains a cycle".into()));
    }
    Ok(())
}

fn validate_budget_account(request: &NewBudgetAccount) -> Result<()> {
    require_non_empty("budget_account_id", &request.budget_account_id)?;
    require_non_empty("budget owner_kind", &request.owner_kind)?;
    require_non_empty("budget owner_id", &request.owner_id)?;
    if request.limits.input_tokens == 0 || request.limits.output_tokens == 0 {
        return Err(TaskEngineError::Invalid(
            "budget input and output limits must be greater than zero".into(),
        ));
    }
    if request.parent_budget_account_id.as_deref() == Some(&request.budget_account_id) {
        return Err(TaskEngineError::Invalid(
            "budget account cannot be its own parent".into(),
        ));
    }
    Ok(())
}

fn insert_budget_account(
    transaction: &Transaction<'_>,
    request: &NewBudgetAccount,
    now: &str,
) -> Result<()> {
    validate_budget_account(request)?;
    if let Some(parent) = &request.parent_budget_account_id {
        budget_account_from_connection(transaction, parent)?;
    }
    transaction.execute(
        "INSERT INTO budget_accounts(
             budget_account_id, parent_budget_account_id, owner_kind, owner_id,
             input_limit, output_limit, reserved_input, reserved_output,
             consumed_input, consumed_output, version, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, 0, 1, ?7, ?7)",
        params![
            request.budget_account_id,
            request.parent_budget_account_id,
            request.owner_kind,
            request.owner_id,
            request.limits.input_tokens,
            request.limits.output_tokens,
            now,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn reserve_budget_tx(
    transaction: &Transaction<'_>,
    budget_account_id: &str,
    reservation_id: &str,
    idempotency_key: &str,
    request: BudgetRequest,
) -> Result<BudgetReservation> {
    require_non_empty("budget_account_id", budget_account_id)?;
    require_non_empty("reservation_id", reservation_id)?;
    require_non_empty("budget idempotency_key", idempotency_key)?;
    if request.input_tokens == 0 && request.output_tokens == 0 {
        return Err(TaskEngineError::Invalid(
            "budget reservation cannot be empty".into(),
        ));
    }
    if let Some(existing_id) = transaction
        .query_row(
            "SELECT reservation_id FROM budget_reservations
             WHERE budget_account_id = ?1 AND idempotency_key = ?2",
            params![budget_account_id, idempotency_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        let existing = reservation_from_connection(transaction, &existing_id)?;
        if existing.reserved == request {
            return Ok(existing);
        }
        return Err(TaskEngineError::Invalid(format!(
            "budget idempotency key `{idempotency_key}` was reused with a different request"
        )));
    }
    if transaction
        .query_row(
            "SELECT 1 FROM budget_reservations WHERE reservation_id = ?1",
            [reservation_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(TaskEngineError::Invalid(format!(
            "reservation_id `{reservation_id}` already exists"
        )));
    }

    let accounts = budget_account_chain(transaction, budget_account_id)?;
    for account in &accounts {
        let available_input = account
            .limits
            .input_tokens
            .saturating_sub(account.reserved_input_tokens)
            .saturating_sub(account.consumed_input_tokens);
        if request.input_tokens > available_input {
            return Err(TaskEngineError::BudgetExceeded {
                account_id: account.budget_account_id.clone(),
                resource: "input tokens",
                requested: request.input_tokens,
                available: available_input,
            });
        }
        let available_output = account
            .limits
            .output_tokens
            .saturating_sub(account.reserved_output_tokens)
            .saturating_sub(account.consumed_output_tokens);
        if request.output_tokens > available_output {
            return Err(TaskEngineError::BudgetExceeded {
                account_id: account.budget_account_id.clone(),
                resource: "output tokens",
                requested: request.output_tokens,
                available: available_output,
            });
        }
    }

    let now = Utc::now().to_rfc3339();
    transaction.execute(
        "INSERT INTO budget_reservations(
             reservation_id, budget_account_id, idempotency_key,
             reserved_input, reserved_output, actual_input, actual_output,
             state, version, created_at, settled_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, 'active', 1, ?6, NULL)",
        params![
            reservation_id,
            budget_account_id,
            idempotency_key,
            request.input_tokens,
            request.output_tokens,
            now,
        ],
    )?;
    for account in &accounts {
        update_account_reservation(
            transaction,
            account,
            i128::from(request.input_tokens),
            i128::from(request.output_tokens),
            0,
            0,
            &now,
        )?;
        insert_budget_entry(
            transaction,
            &account.budget_account_id,
            reservation_id,
            "reserve",
            i128::from(request.input_tokens),
            i128::from(request.output_tokens),
            0,
            0,
            &format!("reserve:{reservation_id}"),
            &now,
        )?;
    }
    reservation_from_connection(transaction, reservation_id)
}

fn settle_budget_tx(
    transaction: &Transaction<'_>,
    reservation: &BudgetReservation,
    expected_version: u64,
    usage: ActualUsage,
) -> Result<BudgetReservation> {
    if reservation.state == BudgetReservationState::Settled {
        if reservation.actual == Some(usage) {
            return Ok(reservation.clone());
        }
        return Err(TaskEngineError::Invalid(format!(
            "reservation `{}` was already settled with different usage",
            reservation.reservation_id
        )));
    }
    ensure_version(
        "budget reservation",
        &reservation.reservation_id,
        expected_version,
        reservation.version,
    )?;
    if reservation.state != BudgetReservationState::Active {
        return Err(invalid_transition(
            "budget reservation",
            &reservation.reservation_id,
            reservation.state.as_db(),
            BudgetReservationState::Settled.as_db(),
        ));
    }
    if usage.input_tokens > reservation.reserved.input_tokens
        || usage.output_tokens > reservation.reserved.output_tokens
    {
        return Err(TaskEngineError::UsageExceedsReservation {
            reservation_id: reservation.reservation_id.clone(),
            reserved_input: reservation.reserved.input_tokens,
            reserved_output: reservation.reserved.output_tokens,
            actual_input: usage.input_tokens,
            actual_output: usage.output_tokens,
        });
    }
    let accounts = budget_account_chain(transaction, &reservation.budget_account_id)?;
    let now = Utc::now().to_rfc3339();
    for account in &accounts {
        update_account_reservation(
            transaction,
            account,
            -i128::from(reservation.reserved.input_tokens),
            -i128::from(reservation.reserved.output_tokens),
            i128::from(usage.input_tokens),
            i128::from(usage.output_tokens),
            &now,
        )?;
        insert_budget_entry(
            transaction,
            &account.budget_account_id,
            &reservation.reservation_id,
            "settle",
            -i128::from(reservation.reserved.input_tokens),
            -i128::from(reservation.reserved.output_tokens),
            i128::from(usage.input_tokens),
            i128::from(usage.output_tokens),
            &format!("settle:{}", reservation.reservation_id),
            &now,
        )?;
    }
    let updated = transaction.execute(
        "UPDATE budget_reservations
         SET actual_input = ?1, actual_output = ?2, state = 'settled',
             version = version + 1, settled_at = ?3
         WHERE reservation_id = ?4 AND state = 'active' AND version = ?5",
        params![
            usage.input_tokens,
            usage.output_tokens,
            now,
            reservation.reservation_id,
            expected_version,
        ],
    )?;
    if updated != 1 {
        let actual = reservation_version(transaction, &reservation.reservation_id)?;
        return Err(TaskEngineError::CasConflict {
            entity: "budget reservation",
            id: reservation.reservation_id.clone(),
            expected: expected_version,
            actual,
        });
    }
    reservation_from_connection(transaction, &reservation.reservation_id)
}

fn release_budget_tx(
    transaction: &Transaction<'_>,
    reservation: &BudgetReservation,
    expected_version: u64,
) -> Result<BudgetReservation> {
    if reservation.state == BudgetReservationState::Released {
        return Ok(reservation.clone());
    }
    ensure_version(
        "budget reservation",
        &reservation.reservation_id,
        expected_version,
        reservation.version,
    )?;
    if reservation.state != BudgetReservationState::Active {
        return Err(invalid_transition(
            "budget reservation",
            &reservation.reservation_id,
            reservation.state.as_db(),
            BudgetReservationState::Released.as_db(),
        ));
    }
    let accounts = budget_account_chain(transaction, &reservation.budget_account_id)?;
    let now = Utc::now().to_rfc3339();
    for account in &accounts {
        update_account_reservation(
            transaction,
            account,
            -i128::from(reservation.reserved.input_tokens),
            -i128::from(reservation.reserved.output_tokens),
            0,
            0,
            &now,
        )?;
        insert_budget_entry(
            transaction,
            &account.budget_account_id,
            &reservation.reservation_id,
            "release",
            -i128::from(reservation.reserved.input_tokens),
            -i128::from(reservation.reserved.output_tokens),
            0,
            0,
            &format!("release:{}", reservation.reservation_id),
            &now,
        )?;
    }
    let updated = transaction.execute(
        "UPDATE budget_reservations
         SET state = 'released', version = version + 1, settled_at = ?1
         WHERE reservation_id = ?2 AND state = 'active' AND version = ?3",
        params![now, reservation.reservation_id, expected_version],
    )?;
    if updated != 1 {
        let actual = reservation_version(transaction, &reservation.reservation_id)?;
        return Err(TaskEngineError::CasConflict {
            entity: "budget reservation",
            id: reservation.reservation_id.clone(),
            expected: expected_version,
            actual,
        });
    }
    reservation_from_connection(transaction, &reservation.reservation_id)
}

#[allow(clippy::too_many_arguments)]
fn update_account_reservation(
    transaction: &Transaction<'_>,
    account: &BudgetAccount,
    reserved_input_delta: i128,
    reserved_output_delta: i128,
    consumed_input_delta: i128,
    consumed_output_delta: i128,
    now: &str,
) -> Result<()> {
    let next_reserved_input = apply_delta(account.reserved_input_tokens, reserved_input_delta)?;
    let next_reserved_output = apply_delta(account.reserved_output_tokens, reserved_output_delta)?;
    let next_consumed_input = apply_delta(account.consumed_input_tokens, consumed_input_delta)?;
    let next_consumed_output = apply_delta(account.consumed_output_tokens, consumed_output_delta)?;
    let updated = transaction.execute(
        "UPDATE budget_accounts
         SET reserved_input = ?1, reserved_output = ?2,
             consumed_input = ?3, consumed_output = ?4,
             version = version + 1, updated_at = ?5
         WHERE budget_account_id = ?6 AND version = ?7",
        params![
            next_reserved_input,
            next_reserved_output,
            next_consumed_input,
            next_consumed_output,
            now,
            account.budget_account_id,
            account.version,
        ],
    )?;
    if updated != 1 {
        let actual: u64 = transaction.query_row(
            "SELECT version FROM budget_accounts WHERE budget_account_id = ?1",
            [&account.budget_account_id],
            |row| row.get(0),
        )?;
        return Err(TaskEngineError::CasConflict {
            entity: "budget account",
            id: account.budget_account_id.clone(),
            expected: account.version,
            actual,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_budget_entry(
    transaction: &Transaction<'_>,
    account_id: &str,
    reservation_id: &str,
    kind: &str,
    reserved_input_delta: i128,
    reserved_output_delta: i128,
    consumed_input_delta: i128,
    consumed_output_delta: i128,
    idempotency_key: &str,
    now: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO budget_entries(
             entry_id, budget_account_id, reservation_id, kind,
             reserved_input_delta, reserved_output_delta,
             consumed_input_delta, consumed_output_delta,
             idempotency_key, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            format!("budget-entry-{}", Uuid::new_v4()),
            account_id,
            reservation_id,
            kind,
            to_i64(reserved_input_delta)?,
            to_i64(reserved_output_delta)?,
            to_i64(consumed_input_delta)?,
            to_i64(consumed_output_delta)?,
            idempotency_key,
            now,
        ],
    )?;
    Ok(())
}

fn budget_account_chain(
    connection: &Connection,
    budget_account_id: &str,
) -> Result<Vec<BudgetAccount>> {
    let mut current = Some(budget_account_id.to_string());
    let mut seen = BTreeSet::new();
    let mut accounts = Vec::new();
    while let Some(account_id) = current.take() {
        if !seen.insert(account_id.clone()) {
            return Err(TaskEngineError::Invalid(format!(
                "budget parent cycle detected at `{account_id}`"
            )));
        }
        let account = budget_account_from_connection(connection, &account_id)?;
        current.clone_from(&account.parent_budget_account_id);
        accounts.push(account);
    }
    Ok(accounts)
}

fn dependency_ready_node_ids(
    transaction: &Transaction<'_>,
    task_run_id: &str,
) -> Result<Vec<String>> {
    let mut statement = transaction.prepare(
        "SELECT candidate.node_id
         FROM task_nodes candidate
         WHERE candidate.task_run_id = ?1
           AND candidate.state = 'waiting_dependency'
           AND NOT EXISTS (
               SELECT 1
               FROM task_node_dependencies dependency
               JOIN task_nodes parent ON parent.node_id = dependency.depends_on_node_id
               WHERE dependency.node_id = candidate.node_id
                 AND parent.state <> 'completed'
           )
         ORDER BY candidate.created_at, candidate.node_id",
    )?;
    let rows = statement.query_map([task_run_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn append_event(
    transaction: &Transaction<'_>,
    task_run_id: &str,
    kind: TaskEventKind,
    payload: &Value,
) -> Result<TaskEvent> {
    let updated = transaction.execute(
        "UPDATE task_runs SET latest_event_seq = latest_event_seq + 1
         WHERE task_run_id = ?1",
        [task_run_id],
    )?;
    if updated != 1 {
        return Err(not_found("task run", task_run_id));
    }
    let sequence: u64 = transaction.query_row(
        "SELECT latest_event_seq FROM task_runs WHERE task_run_id = ?1",
        [task_run_id],
        |row| row.get(0),
    )?;
    let event = TaskEvent {
        event_id: format!("task-event-{}", Uuid::new_v4()),
        task_run_id: task_run_id.into(),
        sequence,
        kind,
        payload: payload.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    transaction.execute(
        "INSERT INTO task_events(event_id, task_run_id, sequence, kind, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.event_id,
            event.task_run_id,
            event.sequence,
            event.kind.as_db(),
            serde_json::to_string(&event.payload).map_err(|error| {
                TaskEngineError::Invalid(format!("invalid task event payload: {error}"))
            })?,
            event.created_at,
        ],
    )?;
    Ok(event)
}

fn task_from_connection(connection: &Connection, task_run_id: &str) -> Result<TaskRun> {
    connection
        .query_row(
            "SELECT t.task_run_id, t.workflow, t.objective, t.origin_kind, t.origin_id,
                    t.room_id, t.state, t.version, t.config_snapshot_id,
                    c.config_version, c.content_hash, c.resolved_config_json,
                    t.budget_account_id, t.latest_event_seq, t.cancel_requested,
                    t.created_at, t.updated_at
             FROM task_runs t
             JOIN task_config_snapshots c ON c.config_snapshot_id = t.config_snapshot_id
             WHERE t.task_run_id = ?1",
            [task_run_id],
            map_task,
        )
        .optional()?
        .ok_or_else(|| not_found("task run", task_run_id))
}

fn map_task(row: &Row<'_>) -> rusqlite::Result<TaskRun> {
    let state = parse_row(6, TaskRunState::from_db(&row.get::<_, String>(6)?))?;
    let resolved_config = parse_row(
        11,
        serde_json::from_str::<Value>(&row.get::<_, String>(11)?),
    )?;
    Ok(TaskRun {
        task_run_id: row.get(0)?,
        workflow: row.get(1)?,
        objective: row.get(2)?,
        origin_kind: row.get(3)?,
        origin_id: row.get(4)?,
        room_id: row.get(5)?,
        state,
        version: row.get(7)?,
        config_snapshot_id: row.get(8)?,
        config_version: row.get(9)?,
        config_content_hash: row.get(10)?,
        resolved_config,
        budget_account_id: row.get(12)?,
        latest_event_seq: row.get(13)?,
        cancel_requested: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

fn node_from_connection(connection: &Connection, node_id: &str) -> Result<TaskNode> {
    let mut node = connection
        .query_row(
            "SELECT node_id, task_run_id, kind, state, provider, model, profile,
                    room_id, member_id, reserve_input, reserve_output, retryable,
                    side_effecting, current_instance_run_id, output_artifact_id,
                    version, created_at, updated_at
             FROM task_nodes WHERE node_id = ?1",
            [node_id],
            map_node,
        )
        .optional()?
        .ok_or_else(|| not_found("task node", node_id))?;
    let mut statement = connection.prepare(
        "SELECT depends_on_node_id FROM task_node_dependencies
         WHERE node_id = ?1 ORDER BY depends_on_node_id",
    )?;
    let rows = statement.query_map([node_id], |row| row.get::<_, String>(0))?;
    node.dependencies = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(node)
}

fn map_node(row: &Row<'_>) -> rusqlite::Result<TaskNode> {
    Ok(TaskNode {
        node_id: row.get(0)?,
        task_run_id: row.get(1)?,
        kind: parse_row(2, NodeKind::from_db(&row.get::<_, String>(2)?))?,
        state: parse_row(3, NodeState::from_db(&row.get::<_, String>(3)?))?,
        provider: row.get(4)?,
        model: row.get(5)?,
        profile: row.get(6)?,
        room_id: row.get(7)?,
        member_id: row.get(8)?,
        reservation: BudgetRequest {
            input_tokens: row.get(9)?,
            output_tokens: row.get(10)?,
        },
        retryable: row.get(11)?,
        side_effecting: row.get(12)?,
        current_instance_run_id: row.get(13)?,
        output_artifact_id: row.get(14)?,
        version: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
        dependencies: Vec::new(),
    })
}

fn node_ids_for_task(connection: &Connection, task_run_id: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT node_id FROM task_nodes
         WHERE task_run_id = ?1 ORDER BY created_at, rowid",
    )?;
    let rows = statement.query_map([task_run_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn budget_account_optional(
    connection: &Connection,
    budget_account_id: &str,
) -> Result<Option<BudgetAccount>> {
    Ok(connection
        .query_row(
            "SELECT budget_account_id, parent_budget_account_id, owner_kind, owner_id,
                    input_limit, output_limit, reserved_input, reserved_output,
                    consumed_input, consumed_output, version
             FROM budget_accounts WHERE budget_account_id = ?1",
            [budget_account_id],
            map_budget_account,
        )
        .optional()?)
}

fn budget_account_from_connection(
    connection: &Connection,
    budget_account_id: &str,
) -> Result<BudgetAccount> {
    budget_account_optional(connection, budget_account_id)?
        .ok_or_else(|| not_found("budget account", budget_account_id))
}

fn map_budget_account(row: &Row<'_>) -> rusqlite::Result<BudgetAccount> {
    Ok(BudgetAccount {
        budget_account_id: row.get(0)?,
        parent_budget_account_id: row.get(1)?,
        owner_kind: row.get(2)?,
        owner_id: row.get(3)?,
        limits: BudgetLimits {
            input_tokens: row.get(4)?,
            output_tokens: row.get(5)?,
        },
        reserved_input_tokens: row.get(6)?,
        reserved_output_tokens: row.get(7)?,
        consumed_input_tokens: row.get(8)?,
        consumed_output_tokens: row.get(9)?,
        version: row.get(10)?,
    })
}

fn reservation_from_connection(
    connection: &Connection,
    reservation_id: &str,
) -> Result<BudgetReservation> {
    connection
        .query_row(
            "SELECT reservation_id, budget_account_id, idempotency_key,
                    reserved_input, reserved_output, actual_input, actual_output,
                    state, version, created_at, settled_at
             FROM budget_reservations WHERE reservation_id = ?1",
            [reservation_id],
            map_reservation,
        )
        .optional()?
        .ok_or_else(|| not_found("budget reservation", reservation_id))
}

fn map_reservation(row: &Row<'_>) -> rusqlite::Result<BudgetReservation> {
    let actual_input = row.get::<_, Option<u64>>(5)?;
    let actual_output = row.get::<_, Option<u64>>(6)?;
    let actual = match (actual_input, actual_output) {
        (Some(input_tokens), Some(output_tokens)) => Some(ActualUsage {
            input_tokens,
            output_tokens,
        }),
        (None, None) => None,
        _ => {
            return Err(row_error(5, "budget reservation has partial actual usage"));
        }
    };
    Ok(BudgetReservation {
        reservation_id: row.get(0)?,
        budget_account_id: row.get(1)?,
        idempotency_key: row.get(2)?,
        reserved: BudgetRequest {
            input_tokens: row.get(3)?,
            output_tokens: row.get(4)?,
        },
        actual,
        state: parse_row(
            7,
            BudgetReservationState::from_db(&row.get::<_, String>(7)?),
        )?,
        version: row.get(8)?,
        created_at: row.get(9)?,
        settled_at: row.get(10)?,
    })
}

fn instance_from_connection(connection: &Connection, instance_run_id: &str) -> Result<InstanceRun> {
    connection
        .query_row(
            "SELECT instance_run_id, task_run_id, node_id, reservation_id, state,
                    artifact_id, error, input_tokens, output_tokens, version,
                    created_at, completed_at
             FROM instance_runs WHERE instance_run_id = ?1",
            [instance_run_id],
            map_instance,
        )
        .optional()?
        .ok_or_else(|| not_found("instance run", instance_run_id))
}

fn map_instance(row: &Row<'_>) -> rusqlite::Result<InstanceRun> {
    let input = row.get::<_, Option<u64>>(7)?;
    let output = row.get::<_, Option<u64>>(8)?;
    let usage = match (input, output) {
        (Some(input_tokens), Some(output_tokens)) => Some(ActualUsage {
            input_tokens,
            output_tokens,
        }),
        (None, None) => None,
        _ => return Err(row_error(7, "instance run has partial usage")),
    };
    Ok(InstanceRun {
        instance_run_id: row.get(0)?,
        task_run_id: row.get(1)?,
        node_id: row.get(2)?,
        reservation_id: row.get(3)?,
        state: parse_row(4, InstanceRunState::from_db(&row.get::<_, String>(4)?))?,
        artifact_id: row.get(5)?,
        error: row.get(6)?,
        usage,
        version: row.get(9)?,
        created_at: row.get(10)?,
        completed_at: row.get(11)?,
    })
}

fn artifact_from_connection(connection: &Connection, artifact_id: &str) -> Result<TaskArtifact> {
    connection
        .query_row(
            "SELECT artifact_id, task_run_id, node_id, instance_run_id,
                    content, content_hash, media_type, created_at
             FROM task_artifacts WHERE artifact_id = ?1",
            [artifact_id],
            |row| {
                Ok(TaskArtifact {
                    artifact_id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    node_id: row.get(2)?,
                    instance_run_id: row.get(3)?,
                    content: row.get(4)?,
                    content_hash: row.get(5)?,
                    media_type: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| not_found("task artifact", artifact_id))
}

fn map_task_event(row: &Row<'_>) -> rusqlite::Result<TaskEvent> {
    Ok(TaskEvent {
        event_id: row.get(0)?,
        task_run_id: row.get(1)?,
        sequence: row.get(2)?,
        kind: parse_row(3, TaskEventKind::from_db(&row.get::<_, String>(3)?))?,
        payload: parse_row(4, serde_json::from_str(&row.get::<_, String>(4)?))?,
        created_at: row.get(5)?,
    })
}

fn instance_exists(connection: &Connection, instance_run_id: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM instance_runs WHERE instance_run_id = ?1",
            [instance_run_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn task_version(connection: &Connection, task_run_id: &str) -> Result<u64> {
    connection
        .query_row(
            "SELECT version FROM task_runs WHERE task_run_id = ?1",
            [task_run_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| not_found("task run", task_run_id))
}

fn node_version(connection: &Connection, node_id: &str) -> Result<u64> {
    connection
        .query_row(
            "SELECT version FROM task_nodes WHERE node_id = ?1",
            [node_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| not_found("task node", node_id))
}

fn instance_version(connection: &Connection, instance_run_id: &str) -> Result<u64> {
    connection
        .query_row(
            "SELECT version FROM instance_runs WHERE instance_run_id = ?1",
            [instance_run_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| not_found("instance run", instance_run_id))
}

fn reservation_version(connection: &Connection, reservation_id: &str) -> Result<u64> {
    connection
        .query_row(
            "SELECT version FROM budget_reservations WHERE reservation_id = ?1",
            [reservation_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| not_found("budget reservation", reservation_id))
}

fn ensure_version(entity: &'static str, id: &str, expected: u64, actual: u64) -> Result<()> {
    if expected != actual {
        return Err(TaskEngineError::CasConflict {
            entity,
            id: id.into(),
            expected,
            actual,
        });
    }
    Ok(())
}

fn require_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(TaskEngineError::Invalid(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn invalid_transition(entity: &'static str, id: &str, from: &str, to: &str) -> TaskEngineError {
    TaskEngineError::InvalidTransition {
        entity,
        id: id.into(),
        from: from.into(),
        to: to.into(),
    }
}

fn not_found(entity: &'static str, id: &str) -> TaskEngineError {
    TaskEngineError::NotFound {
        entity,
        id: id.into(),
    }
}

fn apply_delta(value: u64, delta: i128) -> Result<u64> {
    let next = i128::from(value) + delta;
    u64::try_from(next).map_err(|_| {
        TaskEngineError::Invalid(format!(
            "budget ledger underflow or overflow: {value} + {delta}"
        ))
    })
}

fn to_i64(value: i128) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| TaskEngineError::Invalid(format!("ledger delta out of range: {value}")))
}

fn parse_row<T, E>(index: usize, result: std::result::Result<T, E>) -> rusqlite::Result<T>
where
    E: std::fmt::Display,
{
    result.map_err(|error| row_error(index, &error.to_string()))
}

fn row_error(index: usize, message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.to_string(),
        )),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
