use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use novel_domain::{
    apply_canon_delta, CommitReport, NovelArtifactReceipt, NovelProject, NovelPublicationRecord,
    NovelPublicationStatus, NovelTaskCheckpoint, NovelTaskEvent, NovelTaskState,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::projection::{projection_envelope, NovelProjectionEnvelope};
use crate::{NovelApplicationError, Result};

const SCHEMA_VERSION: i64 = 1;

#[derive(Clone)]
pub struct NovelDomainStore {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
pub struct NovelOutboxRecord {
    pub event_id: String,
    pub aggregate_id: String,
    pub sequence: u64,
    pub source_hash: String,
    pub envelope: NovelProjectionEnvelope,
    pub memory_published: bool,
    pub graph_published: bool,
}

impl NovelDomainStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        initialize_schema(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn import_project(&self, project: &NovelProject) -> Result<bool> {
        project.validate()?;
        let payload = serde_json::to_string(project)?;
        let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
        let envelope = projection_envelope(project, 1, &content_hash)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing_hash) = transaction
            .query_row(
                "SELECT content_hash FROM novel_projects WHERE project_id = ?1",
                [&project.project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing_hash == content_hash {
                transaction.commit()?;
                return Ok(false);
            }
            return Err(NovelApplicationError::Conflict(format!(
                "legacy project {} changed after cutover: existing={existing_hash}, incoming={content_hash}",
                project.project_id
            )));
        }
        transaction.execute(
            "INSERT INTO novel_projects(project_id, canon_revision, content_hash, payload, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.project_id,
                project.canon_revision,
                content_hash,
                payload,
                project.updated_at
            ],
        )?;
        insert_outbox(&transaction, project, &content_hash, &envelope)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn create_project(&self, project: &NovelProject) -> Result<()> {
        project.validate()?;
        let payload = serde_json::to_string(project)?;
        let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
        let envelope = projection_envelope(project, 1, &content_hash)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if transaction
            .query_row(
                "SELECT 1 FROM novel_projects WHERE project_id = ?1",
                [&project.project_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(NovelApplicationError::Conflict(format!(
                "project {} already exists",
                project.project_id
            )));
        }
        transaction.execute(
            "INSERT INTO novel_projects(project_id, canon_revision, content_hash, payload, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.project_id,
                project.canon_revision,
                content_hash,
                payload,
                project.updated_at
            ],
        )?;
        insert_outbox(&transaction, project, &content_hash, &envelope)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_project(&self, previous: &NovelProject, next: &NovelProject) -> Result<()> {
        previous.validate()?;
        next.validate()?;
        if previous.project_id != next.project_id {
            return Err(NovelApplicationError::Conflict(
                "project identity cannot change".into(),
            ));
        }
        let previous_payload = serde_json::to_string(previous)?;
        let previous_hash = knowledge_core::sha256_hex(previous_payload.as_bytes());
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        update_project_with_outbox_cas(&transaction, next, &previous_hash)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn import_checkpoint(
        &self,
        checkpoint: &NovelTaskCheckpoint,
        archived: bool,
    ) -> Result<bool> {
        let payload = serde_json::to_string(checkpoint)?;
        let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
        let connection = self.lock()?;
        if let Some(existing_hash) = connection
            .query_row(
                "SELECT content_hash FROM novel_checkpoints WHERE task_id = ?1",
                [&checkpoint.task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing_hash == content_hash {
                return Ok(false);
            }
            return Err(NovelApplicationError::Conflict(format!(
                "legacy checkpoint {} changed after cutover",
                checkpoint.task_id
            )));
        }
        connection.execute(
            "INSERT INTO novel_checkpoints(
                 task_id, project_id, phase, terminal, archived, draft_version,
                 content_hash, payload, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                checkpoint.task_id,
                checkpoint.project_id,
                serde_json::to_string(&checkpoint.phase)?,
                checkpoint.phase.is_terminal(),
                archived,
                checkpoint.draft_version,
                content_hash,
                payload,
                checkpoint.updated_at
            ],
        )?;
        Ok(true)
    }

    pub fn import_task_event(&self, event: &NovelTaskEvent) -> Result<bool> {
        let payload = serde_json::to_string(event)?;
        let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
        let connection = self.lock()?;
        if let Some(existing_hash) = connection
            .query_row(
                "SELECT content_hash FROM novel_task_events WHERE event_id = ?1",
                [&event.event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing_hash == content_hash {
                return Ok(false);
            }
            return Err(NovelApplicationError::Conflict(format!(
                "legacy event {} changed after cutover",
                event.event_id
            )));
        }
        connection.execute(
            "INSERT INTO novel_task_events(
                 event_id, task_id, project_id, phase, content_hash, payload, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.event_id,
                event.task_id,
                event.project_id,
                serde_json::to_string(&event.phase)?,
                content_hash,
                payload,
                event.created_at
            ],
        )?;
        Ok(true)
    }

    pub fn import_publication(&self, publication: &NovelPublicationRecord) -> Result<bool> {
        let payload = serde_json::to_string(publication)?;
        let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
        let connection = self.lock()?;
        if let Some(existing_hash) = connection
            .query_row(
                "SELECT content_hash FROM novel_publications WHERE publication_id = ?1",
                [&publication.publication_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing_hash == content_hash {
                return Ok(false);
            }
            return Err(NovelApplicationError::Conflict(format!(
                "legacy publication {} changed after cutover",
                publication.publication_id
            )));
        }
        connection.execute(
            "INSERT INTO novel_publications(
                 publication_id, task_id, project_id, status, content_hash, payload, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                publication.publication_id,
                publication.task_id,
                publication.project_id,
                serde_json::to_string(&publication.status)?,
                content_hash,
                payload,
                publication.updated_at
            ],
        )?;
        Ok(true)
    }

    pub fn load_project(&self, project_id: &str) -> Result<NovelProject> {
        let connection = self.lock()?;
        load_json_required(
            &connection,
            "SELECT payload FROM novel_projects WHERE project_id = ?1",
            project_id,
            "project",
        )
    }

    pub fn list_projects(&self) -> Result<Vec<NovelProject>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT payload FROM novel_projects ORDER BY updated_at DESC, project_id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn load_checkpoint(&self, task_id: &str) -> Result<Option<NovelTaskCheckpoint>> {
        let connection = self.lock()?;
        load_json_optional(
            &connection,
            "SELECT payload FROM novel_checkpoints WHERE task_id = ?1",
            task_id,
        )
    }

    pub fn active_checkpoint_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<NovelTaskCheckpoint>> {
        let connection = self.lock()?;
        let payload = connection
            .query_row(
                "SELECT payload FROM novel_checkpoints
                 WHERE project_id = ?1 AND terminal = 0 AND archived = 0
                 ORDER BY updated_at DESC LIMIT 1",
                [project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload
            .map(|value| Ok(serde_json::from_str(&value)?))
            .transpose()
    }

    pub fn active_checkpoints(&self) -> Result<Vec<NovelTaskCheckpoint>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT payload FROM novel_checkpoints
             WHERE terminal = 0 AND archived = 0 ORDER BY updated_at DESC, task_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn save_checkpoint(&self, checkpoint: &NovelTaskCheckpoint) -> Result<()> {
        let payload = serde_json::to_string(checkpoint)?;
        let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO novel_checkpoints(
                 task_id, project_id, phase, terminal, archived, draft_version,
                 content_hash, payload, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8)
             ON CONFLICT(task_id) DO UPDATE SET
                 project_id = excluded.project_id,
                 phase = excluded.phase,
                 terminal = excluded.terminal,
                 archived = 0,
                 draft_version = excluded.draft_version,
                 content_hash = excluded.content_hash,
                 payload = excluded.payload,
                 updated_at = excluded.updated_at",
            params![
                checkpoint.task_id,
                checkpoint.project_id,
                serde_json::to_string(&checkpoint.phase)?,
                checkpoint.phase.is_terminal(),
                checkpoint.draft_version,
                content_hash,
                payload,
                checkpoint.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn append_task_event(&self, event: &NovelTaskEvent) -> Result<()> {
        let _ = self.import_task_event(event)?;
        Ok(())
    }

    pub fn persist_state_event(
        &self,
        state: &NovelTaskState,
        event: &NovelTaskEvent,
    ) -> Result<()> {
        let checkpoint = state.checkpoint()?;
        let checkpoint_payload = serde_json::to_string(&checkpoint)?;
        let checkpoint_hash = knowledge_core::sha256_hex(checkpoint_payload.as_bytes());
        let event_payload = serde_json::to_string(event)?;
        let event_hash = knowledge_core::sha256_hex(event_payload.as_bytes());
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO novel_checkpoints(
                 task_id, project_id, phase, terminal, archived, draft_version,
                 content_hash, payload, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8)
             ON CONFLICT(task_id) DO UPDATE SET
                 project_id = excluded.project_id,
                 phase = excluded.phase,
                 terminal = excluded.terminal,
                 archived = 0,
                 draft_version = excluded.draft_version,
                 content_hash = excluded.content_hash,
                 payload = excluded.payload,
                 updated_at = excluded.updated_at",
            params![
                checkpoint.task_id,
                checkpoint.project_id,
                serde_json::to_string(&checkpoint.phase)?,
                checkpoint.phase.is_terminal(),
                checkpoint.draft_version,
                checkpoint_hash,
                checkpoint_payload,
                checkpoint.updated_at
            ],
        )?;
        if let Some(existing_hash) = transaction
            .query_row(
                "SELECT content_hash FROM novel_task_events WHERE event_id = ?1",
                [&event.event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing_hash != event_hash {
                return Err(NovelApplicationError::Conflict(format!(
                    "task event {} already exists with different content",
                    event.event_id
                )));
            }
        } else {
            transaction.execute(
                "INSERT INTO novel_task_events(
                     event_id, task_id, project_id, phase, content_hash, payload, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id,
                    event.task_id,
                    event.project_id,
                    serde_json::to_string(&event.phase)?,
                    event_hash,
                    event_payload,
                    event.created_at
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn archive_task(&self, task_id: &str) -> Result<bool> {
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE novel_checkpoints SET archived = 1 WHERE task_id = ?1 AND archived = 0",
            [task_id],
        )? > 0)
    }

    pub fn load_task_events(&self, task_id: &str) -> Result<Vec<NovelTaskEvent>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT payload FROM novel_task_events WHERE task_id = ?1 ORDER BY created_at, event_id",
        )?;
        let rows = statement.query_map([task_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn load_publication(&self, publication_id: &str) -> Result<NovelPublicationRecord> {
        let connection = self.lock()?;
        load_json_required(
            &connection,
            "SELECT payload FROM novel_publications WHERE publication_id = ?1",
            publication_id,
            "publication",
        )
    }

    pub fn begin_publication(&self, publication: &NovelPublicationRecord) -> Result<()> {
        if publication.status != NovelPublicationStatus::Pending
            || publication.project_id != publication.delta.project_id
            || publication.output_path.trim().is_empty()
            || publication.content_sha256.trim().is_empty()
        {
            return Err(NovelApplicationError::Conflict(
                "invalid pending Novel publication contract".into(),
            ));
        }
        let project = self.load_project(&publication.project_id)?;
        if publication.expected_revision != project.canon_revision
            || publication.delta.expected_revision != project.canon_revision
        {
            return Err(NovelApplicationError::Conflict(format!(
                "Novel project revision changed: expected={}, actual={}",
                publication.expected_revision, project.canon_revision
            )));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = serde_json::to_string(&NovelPublicationStatus::Pending)?;
        let artifact_saved = serde_json::to_string(&NovelPublicationStatus::ArtifactSaved)?;
        let existing = transaction.query_row(
            "SELECT COUNT(*) FROM novel_publications
             WHERE project_id = ?1 AND status IN (?2, ?3)",
            params![publication.project_id, pending, artifact_saved],
            |row| row.get::<_, u64>(0),
        )?;
        if existing > 0 {
            return Err(NovelApplicationError::Conflict(format!(
                "project {} already has pending publication",
                publication.project_id
            )));
        }
        insert_or_validate_publication(&transaction, publication)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn complete_publication(
        &self,
        publication_id: &str,
        artifact: NovelArtifactReceipt,
    ) -> Result<CommitReport> {
        let mut publication = self.load_publication(publication_id)?;
        if publication.status == NovelPublicationStatus::Completed {
            return publication.commit_report.ok_or_else(|| {
                NovelApplicationError::Conflict(
                    "completed publication has no Canon commit report".into(),
                )
            });
        }
        if !matches!(
            publication.status,
            NovelPublicationStatus::Pending | NovelPublicationStatus::ArtifactSaved
        ) || artifact.canonical_path != publication.output_path
            || artifact.sha256 != publication.content_sha256
        {
            return Err(NovelApplicationError::Conflict(
                "Artifact receipt does not match the reviewed publication".into(),
            ));
        }

        publication.artifact = Some(artifact);
        publication.status = NovelPublicationStatus::ArtifactSaved;
        publication.updated_at = now_millis();
        self.save_publication(&publication)?;

        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project: NovelProject = load_json_required(
            &transaction,
            "SELECT payload FROM novel_projects WHERE project_id = ?1",
            &publication.project_id,
            "project",
        )?;
        let outcome = apply_canon_delta(
            &project,
            &publication.delta,
            Some(publication_id),
            now_millis(),
        )?;
        if outcome.should_persist {
            update_project_with_outbox(&transaction, &outcome.project)?;
        }
        publication.status = NovelPublicationStatus::Completed;
        publication.commit_report = Some(outcome.report.clone());
        publication.error = None;
        publication.updated_at = now_millis();
        update_publication(&transaction, &publication)?;
        transaction.commit()?;
        Ok(outcome.report)
    }

    pub fn abort_publication(&self, publication_id: &str, reason: &str) -> Result<()> {
        if reason.trim().is_empty() {
            return Err(NovelApplicationError::Conflict(
                "publication abort reason is required".into(),
            ));
        }
        let mut publication = self.load_publication(publication_id)?;
        if publication.status == NovelPublicationStatus::Completed {
            return Err(NovelApplicationError::Conflict(
                "completed publication cannot be aborted".into(),
            ));
        }
        publication.status = NovelPublicationStatus::Aborted;
        publication.error = Some(reason.to_owned());
        publication.updated_at = now_millis();
        self.save_publication(&publication)
    }

    pub fn pending_publications(&self) -> Result<Vec<NovelPublicationRecord>> {
        let connection = self.lock()?;
        let pending = serde_json::to_string(&NovelPublicationStatus::Pending)?;
        let artifact_saved = serde_json::to_string(&NovelPublicationStatus::ArtifactSaved)?;
        let mut statement = connection.prepare(
            "SELECT payload FROM novel_publications
             WHERE status IN (?1, ?2) ORDER BY updated_at, publication_id",
        )?;
        let rows = statement.query_map(params![pending, artifact_saved], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn pending_outbox(&self, limit: usize) -> Result<Vec<NovelOutboxRecord>> {
        if limit == 0 {
            return Err(NovelApplicationError::Conflict(
                "outbox limit must be positive".into(),
            ));
        }
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT event_id, aggregate_id, sequence, source_hash, payload,
                    memory_published, graph_published
             FROM novel_outbox
             WHERE memory_published = 0 OR graph_published = 0
             ORDER BY aggregate_id, sequence LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as u64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (event_id, aggregate_id, sequence, source_hash, payload, memory, graph) = row?;
            Ok(NovelOutboxRecord {
                event_id,
                aggregate_id,
                sequence,
                source_hash,
                envelope: serde_json::from_str(&payload)?,
                memory_published: memory,
                graph_published: graph,
            })
        })
        .collect()
    }

    pub fn mark_memory_published(&self, event_id: &str) -> Result<()> {
        self.mark_sink(event_id, "memory_published")
    }

    pub fn mark_graph_published(&self, event_id: &str) -> Result<()> {
        self.mark_sink(event_id, "graph_published")
    }

    fn mark_sink(&self, event_id: &str, column: &str) -> Result<()> {
        let sql = match column {
            "memory_published" => {
                "UPDATE novel_outbox SET memory_published = 1 WHERE event_id = ?1"
            }
            "graph_published" => "UPDATE novel_outbox SET graph_published = 1 WHERE event_id = ?1",
            _ => unreachable!("sink column is internal"),
        };
        let connection = self.lock()?;
        if connection.execute(sql, [event_id])? == 0 {
            return Err(NovelApplicationError::NotFound(format!(
                "outbox event {event_id}"
            )));
        }
        Ok(())
    }

    fn save_publication(&self, publication: &NovelPublicationRecord) -> Result<()> {
        let connection = self.lock()?;
        update_publication(&connection, publication)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| NovelApplicationError::Storage("Novel database lock poisoned".into()))
    }
}

fn insert_outbox(
    transaction: &Transaction<'_>,
    project: &NovelProject,
    source_hash: &str,
    envelope: &NovelProjectionEnvelope,
) -> Result<()> {
    let payload = serde_json::to_string(envelope)?;
    transaction.execute(
        "INSERT INTO novel_outbox(
             event_id, aggregate_id, sequence, source_hash, payload,
             memory_published, graph_published, created_at
         ) VALUES (?1, ?2, 1, ?3, ?4, 0, 0, ?5)",
        params![
            envelope.source_event.event_id,
            project.project_id,
            source_hash,
            payload,
            project.updated_at
        ],
    )?;
    Ok(())
}

fn update_project_with_outbox(transaction: &Transaction<'_>, project: &NovelProject) -> Result<()> {
    let payload = serde_json::to_string(project)?;
    let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
    transaction.execute(
        "UPDATE novel_projects
         SET canon_revision = ?2, content_hash = ?3, payload = ?4, updated_at = ?5
         WHERE project_id = ?1",
        params![
            project.project_id,
            project.canon_revision,
            content_hash,
            payload,
            project.updated_at
        ],
    )?;
    let sequence = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM novel_outbox WHERE aggregate_id = ?1",
        [&project.project_id],
        |row| row.get::<_, u64>(0),
    )?;
    let envelope = projection_envelope(project, sequence, &content_hash)?;
    transaction.execute(
        "INSERT INTO novel_outbox(
             event_id, aggregate_id, sequence, source_hash, payload,
             memory_published, graph_published, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6)",
        params![
            envelope.source_event.event_id,
            project.project_id,
            sequence,
            content_hash,
            serde_json::to_string(&envelope)?,
            project.updated_at
        ],
    )?;
    Ok(())
}

fn update_project_with_outbox_cas(
    transaction: &Transaction<'_>,
    project: &NovelProject,
    expected_hash: &str,
) -> Result<()> {
    let payload = serde_json::to_string(project)?;
    let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
    if transaction.execute(
        "UPDATE novel_projects
         SET canon_revision = ?2, content_hash = ?3, payload = ?4, updated_at = ?5
         WHERE project_id = ?1 AND content_hash = ?6",
        params![
            project.project_id,
            project.canon_revision,
            content_hash,
            payload,
            project.updated_at,
            expected_hash
        ],
    )? == 0
    {
        return Err(NovelApplicationError::Conflict(format!(
            "project {} changed concurrently",
            project.project_id
        )));
    }
    let sequence = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM novel_outbox WHERE aggregate_id = ?1",
        [&project.project_id],
        |row| row.get::<_, u64>(0),
    )?;
    let envelope = projection_envelope(project, sequence, &content_hash)?;
    transaction.execute(
        "INSERT INTO novel_outbox(
             event_id, aggregate_id, sequence, source_hash, payload,
             memory_published, graph_published, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6)",
        params![
            envelope.source_event.event_id,
            project.project_id,
            sequence,
            content_hash,
            serde_json::to_string(&envelope)?,
            project.updated_at
        ],
    )?;
    Ok(())
}

fn insert_or_validate_publication(
    transaction: &Transaction<'_>,
    publication: &NovelPublicationRecord,
) -> Result<()> {
    let payload = serde_json::to_string(publication)?;
    let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
    if let Some(existing_hash) = transaction
        .query_row(
            "SELECT content_hash FROM novel_publications WHERE publication_id = ?1",
            [&publication.publication_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        if existing_hash == content_hash {
            return Ok(());
        }
        return Err(NovelApplicationError::Conflict(format!(
            "publication {} already exists with different content",
            publication.publication_id
        )));
    }
    transaction.execute(
        "INSERT INTO novel_publications(
             publication_id, task_id, project_id, status, content_hash, payload, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            publication.publication_id,
            publication.task_id,
            publication.project_id,
            serde_json::to_string(&publication.status)?,
            content_hash,
            payload,
            publication.updated_at
        ],
    )?;
    Ok(())
}

fn update_publication(connection: &Connection, publication: &NovelPublicationRecord) -> Result<()> {
    let payload = serde_json::to_string(publication)?;
    let content_hash = knowledge_core::sha256_hex(payload.as_bytes());
    if connection.execute(
        "UPDATE novel_publications
         SET status = ?2, content_hash = ?3, payload = ?4, updated_at = ?5
         WHERE publication_id = ?1",
        params![
            publication.publication_id,
            serde_json::to_string(&publication.status)?,
            content_hash,
            payload,
            publication.updated_at
        ],
    )? == 0
    {
        return Err(NovelApplicationError::NotFound(format!(
            "publication {}",
            publication.publication_id
        )));
    }
    Ok(())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn load_json_optional<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
) -> Result<Option<T>> {
    let payload = connection
        .query_row(sql, [id], |row| row.get::<_, String>(0))
        .optional()?;
    payload
        .map(|payload| Ok(serde_json::from_str(&payload)?))
        .transpose()
}

fn load_json_required<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
    kind: &str,
) -> Result<T> {
    load_json_optional(connection, sql, id)?
        .ok_or_else(|| NovelApplicationError::NotFound(format!("{kind} {id}")))
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS novel_schema (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             version INTEGER NOT NULL
         );
         INSERT INTO novel_schema(singleton, version) VALUES (1, 1)
         ON CONFLICT(singleton) DO NOTHING;
         CREATE TABLE IF NOT EXISTS novel_projects (
             project_id TEXT PRIMARY KEY,
             canon_revision INTEGER NOT NULL,
             content_hash TEXT NOT NULL,
             payload TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS novel_checkpoints (
             task_id TEXT PRIMARY KEY,
             project_id TEXT NOT NULL,
             phase TEXT NOT NULL,
             terminal INTEGER NOT NULL,
             archived INTEGER NOT NULL DEFAULT 0,
             draft_version INTEGER NOT NULL,
             content_hash TEXT NOT NULL,
             payload TEXT NOT NULL,
             updated_at INTEGER NOT NULL,
             FOREIGN KEY(project_id) REFERENCES novel_projects(project_id)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS novel_one_active_task_per_project
             ON novel_checkpoints(project_id)
             WHERE terminal = 0 AND archived = 0;
         CREATE TABLE IF NOT EXISTS novel_task_events (
             event_id TEXT PRIMARY KEY,
             task_id TEXT NOT NULL,
             project_id TEXT NOT NULL,
             phase TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             payload TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             FOREIGN KEY(task_id) REFERENCES novel_checkpoints(task_id),
             FOREIGN KEY(project_id) REFERENCES novel_projects(project_id)
         );
         CREATE TABLE IF NOT EXISTS novel_publications (
             publication_id TEXT PRIMARY KEY,
             task_id TEXT NOT NULL,
             project_id TEXT NOT NULL,
             status TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             payload TEXT NOT NULL,
             updated_at INTEGER NOT NULL,
             FOREIGN KEY(task_id) REFERENCES novel_checkpoints(task_id),
             FOREIGN KEY(project_id) REFERENCES novel_projects(project_id)
         );
         CREATE TABLE IF NOT EXISTS novel_outbox (
             event_id TEXT PRIMARY KEY,
             aggregate_id TEXT NOT NULL,
             sequence INTEGER NOT NULL,
             source_hash TEXT NOT NULL,
             payload TEXT NOT NULL,
             memory_published INTEGER NOT NULL DEFAULT 0,
             graph_published INTEGER NOT NULL DEFAULT 0,
             created_at INTEGER NOT NULL,
             UNIQUE(aggregate_id, sequence),
             FOREIGN KEY(aggregate_id) REFERENCES novel_projects(project_id)
         );",
    )?;
    let version = connection.query_row(
        "SELECT version FROM novel_schema WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if version != SCHEMA_VERSION {
        return Err(NovelApplicationError::Storage(format!(
            "unsupported Novel schema version: {version}"
        )));
    }
    Ok(())
}
