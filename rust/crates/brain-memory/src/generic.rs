use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use knowledge_core::{
    sha256_hex, ContentProviderId, ContentRef, KnowledgeError, KnowledgeOutboxEvent,
    KnowledgeSchemaRegistry, MemoryCommandPort, MemoryEntry, MemoryProposal, MemoryQuery,
    MemoryQueryPort, MemoryQueryResult, MemoryStatus, MemoryTypeId, NamespaceId, Provenance,
    ResourceTypeId, ScopeRef, ScopeTypeId, SourceRef, TenantId,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

const WRITER_QUEUE_CAPACITY: usize = 128;

#[derive(Clone)]
pub struct GenericMemoryStore {
    path: Arc<PathBuf>,
    schemas: Arc<KnowledgeSchemaRegistry>,
    writer: Arc<MemoryWriter>,
}

struct MemoryWriter {
    sender: mpsc::SyncSender<WriterCommand>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

enum WriterCommand {
    Submit {
        proposal: MemoryProposal,
        response: mpsc::Sender<Result<MemoryEntry, KnowledgeError>>,
    },
    Supersede {
        memory_entry_id: String,
        expected_version: u64,
        reason: String,
        response: mpsc::Sender<Result<MemoryEntry, KnowledgeError>>,
    },
    AcknowledgeOutbox {
        event_id: String,
        response: mpsc::Sender<Result<bool, KnowledgeError>>,
    },
    Shutdown,
}

impl Drop for MemoryWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(WriterCommand::Shutdown);
        if let Ok(join) = self.join.get_mut() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }
}

impl GenericMemoryStore {
    pub fn open(
        path: impl AsRef<Path>,
        schemas: Arc<KnowledgeSchemaRegistry>,
    ) -> Result<Self, KnowledgeError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(storage_error)?;
        }
        let connection = Connection::open(&path).map_err(storage_error)?;
        configure_writer(&connection)?;
        initialize_schema(&connection)?;
        drop(connection);

        let (sender, receiver) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let writer_path = path.clone();
        let join = thread::Builder::new()
            .name("memory-write-executor".into())
            .spawn(move || writer_loop(&writer_path, receiver))
            .map_err(storage_error)?;
        Ok(Self {
            path: Arc::new(path),
            schemas,
            writer: Arc::new(MemoryWriter {
                sender,
                join: Mutex::new(Some(join)),
            }),
        })
    }

    fn read_connection(&self) -> Result<Connection, KnowledgeError> {
        let connection = Connection::open_with_flags(
            self.path.as_ref(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(storage_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage_error)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(storage_error)?;
        Ok(connection)
    }

    fn validate_proposal(&self, proposal: &MemoryProposal) -> Result<(), KnowledgeError> {
        if proposal.proposal_id.trim().is_empty()
            || proposal.idempotency_key.trim().is_empty()
            || proposal.summary.trim().is_empty()
        {
            return Err(KnowledgeError::InvalidInput(
                "proposal id, idempotency key, and summary are required".into(),
            ));
        }
        if proposal.namespace != proposal.owner_scope.namespace {
            return Err(KnowledgeError::InvalidInput(
                "proposal namespace must match owner scope namespace".into(),
            ));
        }
        self.schemas
            .validate_memory_scope(&proposal.memory_type, &proposal.owner_scope.scope_type)?;
        if proposal.visibility_scopes.is_empty()
            || proposal.visibility_scopes.iter().any(|scope| {
                scope.tenant_id != proposal.owner_scope.tenant_id
                    || scope.namespace != proposal.namespace
            })
        {
            return Err(KnowledgeError::InvalidInput(
                "visibility scopes must be non-empty and match proposal tenant/namespace".into(),
            ));
        }
        Ok(())
    }

    fn send_submit(&self, proposal: MemoryProposal) -> Result<MemoryEntry, KnowledgeError> {
        let (response, result) = mpsc::channel();
        self.writer
            .sender
            .send(WriterCommand::Submit { proposal, response })
            .map_err(|_| KnowledgeError::Unavailable("memory writer stopped".into()))?;
        result
            .recv()
            .map_err(|_| KnowledgeError::Unavailable("memory writer response lost".into()))?
    }

    fn send_supersede(
        &self,
        memory_entry_id: &str,
        expected_version: u64,
        reason: &str,
    ) -> Result<MemoryEntry, KnowledgeError> {
        let (response, result) = mpsc::channel();
        self.writer
            .sender
            .send(WriterCommand::Supersede {
                memory_entry_id: memory_entry_id.to_owned(),
                expected_version,
                reason: reason.to_owned(),
                response,
            })
            .map_err(|_| KnowledgeError::Unavailable("memory writer stopped".into()))?;
        result
            .recv()
            .map_err(|_| KnowledgeError::Unavailable("memory writer response lost".into()))?
    }
}

impl MemoryCommandPort for GenericMemoryStore {
    fn submit(&self, proposal: MemoryProposal) -> Result<MemoryEntry, KnowledgeError> {
        self.validate_proposal(&proposal)?;
        self.send_submit(proposal)
    }

    fn supersede(
        &self,
        memory_entry_id: &str,
        expected_version: u64,
        reason: &str,
    ) -> Result<MemoryEntry, KnowledgeError> {
        if memory_entry_id.trim().is_empty() || reason.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(
                "memory entry id and supersede reason are required".into(),
            ));
        }
        self.send_supersede(memory_entry_id, expected_version, reason)
    }

    fn pending_outbox(&self, limit: usize) -> Result<Vec<KnowledgeOutboxEvent>, KnowledgeError> {
        if limit == 0 {
            return Err(KnowledgeError::InvalidInput(
                "outbox limit must be positive".into(),
            ));
        }
        let connection = self.read_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT event_id, aggregate_id, event_type, sequence, payload_json
                 FROM memory_outbox_events
                 WHERE published_at IS NULL
                 ORDER BY sequence, event_id
                 LIMIT ?1",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([limit as i64], |row| {
                let payload: String = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                    payload,
                ))
            })
            .map_err(storage_error)?;
        let mut events = Vec::new();
        for row in rows {
            let (event_id, aggregate_id, event_type, sequence, payload) =
                row.map_err(storage_error)?;
            events.push(KnowledgeOutboxEvent {
                event_id,
                aggregate_id,
                event_type,
                sequence,
                payload: serde_json::from_str(&payload)?,
            });
        }
        Ok(events)
    }

    fn acknowledge_outbox(&self, event_id: &str) -> Result<bool, KnowledgeError> {
        let (response, result) = mpsc::channel();
        self.writer
            .sender
            .send(WriterCommand::AcknowledgeOutbox {
                event_id: event_id.to_owned(),
                response,
            })
            .map_err(|_| KnowledgeError::Unavailable("memory writer stopped".into()))?;
        result
            .recv()
            .map_err(|_| KnowledgeError::Unavailable("memory writer response lost".into()))?
    }
}

impl MemoryQueryPort for GenericMemoryStore {
    fn query(&self, query: &MemoryQuery) -> Result<MemoryQueryResult, KnowledgeError> {
        if query.limit == 0 || query.authorized_scopes.is_empty() {
            return Err(KnowledgeError::InvalidInput(
                "memory query requires scopes and a positive limit".into(),
            ));
        }
        if query
            .authorized_scopes
            .iter()
            .any(|scope| scope.tenant_id != query.tenant_id)
        {
            return Err(KnowledgeError::Unauthorized(
                "query scope tenant does not match requested tenant".into(),
            ));
        }

        let connection = self.read_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT memory_entry_id
                 FROM memory_entries
                 WHERE tenant_id = ?1 AND status = 'active'
                 ORDER BY updated_at DESC, memory_entry_id",
            )
            .map_err(storage_error)?;
        let ids = statement
            .query_map([query.tenant_id.as_str()], |row| row.get::<_, String>(0))
            .map_err(storage_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        let scanned = ids.len();
        let normalized_terms = query
            .terms
            .iter()
            .map(|term| term.trim().to_lowercase())
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        let mut entries = Vec::new();
        for id in ids {
            let entry = load_entry(&connection, &id)?.ok_or_else(|| {
                KnowledgeError::Storage(format!("memory entry disappeared during query: {id}"))
            })?;
            if !query.namespaces.is_empty() && !query.namespaces.contains(&entry.namespace) {
                continue;
            }
            if !query.memory_types.is_empty() && !query.memory_types.contains(&entry.memory_type) {
                continue;
            }
            if !entry
                .visibility_scopes
                .iter()
                .any(|scope| query.authorized_scopes.contains(scope))
            {
                continue;
            }
            let summary = entry.summary.to_lowercase();
            if !normalized_terms.is_empty()
                && !normalized_terms.iter().any(|term| summary.contains(term))
            {
                continue;
            }
            entries.push(entry);
            if entries.len() > query.limit {
                break;
            }
        }
        let truncated = entries.len() > query.limit;
        entries.truncate(query.limit);
        Ok(MemoryQueryResult {
            entries,
            scanned,
            truncated,
        })
    }
}

fn writer_loop(path: &Path, receiver: mpsc::Receiver<WriterCommand>) {
    let mut connection = Connection::open(path)
        .map_err(storage_error)
        .and_then(|connection| {
            configure_writer(&connection)?;
            Ok(connection)
        });
    while let Ok(command) = receiver.recv() {
        match command {
            WriterCommand::Submit { proposal, response } => {
                let result = match connection.as_mut() {
                    Ok(connection) => write_submit(connection, &proposal),
                    Err(error) => Err(KnowledgeError::Storage(error.to_string())),
                };
                let _ = response.send(result);
            }
            WriterCommand::Supersede {
                memory_entry_id,
                expected_version,
                reason,
                response,
            } => {
                let result = match connection.as_mut() {
                    Ok(connection) => {
                        write_supersede(connection, &memory_entry_id, expected_version, &reason)
                    }
                    Err(error) => Err(KnowledgeError::Storage(error.to_string())),
                };
                let _ = response.send(result);
            }
            WriterCommand::AcknowledgeOutbox { event_id, response } => {
                let result = match connection.as_mut() {
                    Ok(connection) => connection
                        .execute(
                            "UPDATE memory_outbox_events
                             SET published_at = unixepoch('subsec') * 1000
                             WHERE event_id = ?1 AND published_at IS NULL",
                            [event_id],
                        )
                        .map(|changed| changed > 0)
                        .map_err(storage_error),
                    Err(error) => Err(KnowledgeError::Storage(error.to_string())),
                };
                let _ = response.send(result);
            }
            WriterCommand::Shutdown => break,
        }
    }
}

fn configure_writer(connection: &Connection) -> Result<(), KnowledgeError> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = FULL;",
        )
        .map_err(storage_error)
}

fn initialize_schema(connection: &Connection) -> Result<(), KnowledgeError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_entries (
                 memory_entry_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 memory_type TEXT NOT NULL,
                 owner_scope_type TEXT NOT NULL,
                 owner_scope_key TEXT NOT NULL,
                 summary TEXT NOT NULL,
                 content_provider TEXT,
                 content_resource_id TEXT,
                 content_version TEXT,
                 content_hash TEXT,
                 provenance_json TEXT NOT NULL,
                 trust TEXT NOT NULL,
                 retention TEXT NOT NULL,
                 status TEXT NOT NULL,
                 version INTEGER NOT NULL,
                 idempotency_key TEXT NOT NULL UNIQUE,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_entries_scope
                 ON memory_entries(tenant_id, namespace, memory_type, status);

             CREATE TABLE IF NOT EXISTS memory_scopes (
                 memory_entry_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 tenant_id TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 scope_type TEXT NOT NULL,
                 scope_key TEXT NOT NULL,
                 PRIMARY KEY(memory_entry_id, role, tenant_id, namespace, scope_type, scope_key),
                 FOREIGN KEY(memory_entry_id) REFERENCES memory_entries(memory_entry_id)
                     ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_memory_scopes_lookup
                 ON memory_scopes(tenant_id, namespace, scope_type, scope_key, role);

             CREATE TABLE IF NOT EXISTS memory_sources (
                 memory_entry_id TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 namespace TEXT NOT NULL,
                 resource_type TEXT NOT NULL,
                 resource_id TEXT NOT NULL,
                 version TEXT,
                 content_hash TEXT,
                 PRIMARY KEY(memory_entry_id, position),
                 FOREIGN KEY(memory_entry_id) REFERENCES memory_entries(memory_entry_id)
                     ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_memory_sources_identity
                 ON memory_sources(namespace, resource_type, resource_id, version, content_hash);

             CREATE TABLE IF NOT EXISTS memory_tombstones (
                 memory_entry_id TEXT NOT NULL,
                 version INTEGER NOT NULL,
                 reason TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY(memory_entry_id, version),
                 FOREIGN KEY(memory_entry_id) REFERENCES memory_entries(memory_entry_id)
                     ON DELETE CASCADE
             );

             CREATE TABLE IF NOT EXISTS memory_outbox_events (
                 event_id TEXT PRIMARY KEY,
                 aggregate_id TEXT NOT NULL,
                 event_type TEXT NOT NULL,
                 sequence INTEGER NOT NULL UNIQUE,
                 payload_json TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 published_at INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_memory_outbox_pending
                 ON memory_outbox_events(published_at, sequence);",
        )
        .map_err(storage_error)
}

fn write_submit(
    connection: &mut Connection,
    proposal: &MemoryProposal,
) -> Result<MemoryEntry, KnowledgeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    let existing_id = transaction
        .query_row(
            "SELECT memory_entry_id FROM memory_entries WHERE idempotency_key = ?1",
            [&proposal.idempotency_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_error)?;
    if let Some(existing_id) = existing_id {
        let entry = load_entry(&transaction, &existing_id)?.ok_or_else(|| {
            KnowledgeError::Storage(format!("idempotent memory entry missing: {existing_id}"))
        })?;
        transaction.commit().map_err(storage_error)?;
        return Ok(entry);
    }

    let memory_entry_id = deterministic_memory_id(proposal);
    let now = now_ms();
    let (provider, resource_id, content_version, content_hash) = proposal
        .content_ref
        .as_ref()
        .map_or((None, None, None, None), |reference| {
            (
                Some(reference.provider.as_str()),
                Some(reference.resource_id.as_str()),
                reference.version.as_deref(),
                Some(reference.content_hash.as_str()),
            )
        });
    transaction
        .execute(
            "INSERT INTO memory_entries(
                 memory_entry_id, tenant_id, namespace, memory_type,
                 owner_scope_type, owner_scope_key, summary,
                 content_provider, content_resource_id, content_version, content_hash,
                 provenance_json, trust, retention, status, version,
                 idempotency_key, created_at, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                 ?12, ?13, ?14, 'active', 1, ?15, ?16, ?16
             )",
            params![
                memory_entry_id,
                proposal.owner_scope.tenant_id.as_str(),
                proposal.namespace.as_str(),
                proposal.memory_type.as_str(),
                proposal.owner_scope.scope_type.as_str(),
                proposal.owner_scope.scope_key,
                proposal.summary,
                provider,
                resource_id,
                content_version,
                content_hash,
                serde_json::to_string(&proposal.provenance)?,
                enum_json(&proposal.trust)?,
                enum_json(&proposal.retention)?,
                proposal.idempotency_key,
                now,
            ],
        )
        .map_err(storage_error)?;
    insert_scope(
        &transaction,
        &memory_entry_id,
        "owner",
        &proposal.owner_scope,
    )?;
    for scope in &proposal.visibility_scopes {
        insert_scope(&transaction, &memory_entry_id, "visibility", scope)?;
    }
    for (position, source) in proposal.source_refs.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO memory_sources(
                     memory_entry_id, position, namespace, resource_type,
                     resource_id, version, content_hash
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    memory_entry_id,
                    position as i64,
                    source.namespace.as_str(),
                    source.resource_type.as_str(),
                    source.resource_id,
                    source.version,
                    source.content_hash,
                ],
            )
            .map_err(storage_error)?;
    }
    let entry = MemoryEntry {
        memory_entry_id: memory_entry_id.clone(),
        namespace: proposal.namespace.clone(),
        memory_type: proposal.memory_type.clone(),
        owner_scope: proposal.owner_scope.clone(),
        visibility_scopes: proposal.visibility_scopes.clone(),
        summary: proposal.summary.clone(),
        content_ref: proposal.content_ref.clone(),
        source_refs: proposal.source_refs.clone(),
        provenance: proposal.provenance.clone(),
        trust: proposal.trust,
        retention: proposal.retention,
        status: MemoryStatus::Active,
        version: 1,
    };
    insert_outbox(&transaction, &entry, "memory_committed", None)?;
    transaction.commit().map_err(storage_error)?;
    Ok(entry)
}

fn write_supersede(
    connection: &mut Connection,
    memory_entry_id: &str,
    expected_version: u64,
    reason: &str,
) -> Result<MemoryEntry, KnowledgeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    let mut entry = load_entry(&transaction, memory_entry_id)?.ok_or_else(|| {
        KnowledgeError::InvalidInput(format!("unknown memory: {memory_entry_id}"))
    })?;
    if entry.version != expected_version {
        return Err(KnowledgeError::StaleRevision {
            record_id: memory_entry_id.to_owned(),
            expected: expected_version,
            actual: entry.version,
        });
    }
    let next_version = entry.version + 1;
    let changed = transaction
        .execute(
            "UPDATE memory_entries
             SET status = 'superseded', version = ?1, updated_at = ?2
             WHERE memory_entry_id = ?3 AND version = ?4",
            params![next_version, now_ms(), memory_entry_id, expected_version],
        )
        .map_err(storage_error)?;
    if changed != 1 {
        return Err(KnowledgeError::StaleRevision {
            record_id: memory_entry_id.to_owned(),
            expected: expected_version,
            actual: entry.version,
        });
    }
    transaction
        .execute(
            "INSERT INTO memory_tombstones(memory_entry_id, version, reason, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![memory_entry_id, next_version, reason, now_ms()],
        )
        .map_err(storage_error)?;
    entry.status = MemoryStatus::Superseded;
    entry.version = next_version;
    insert_outbox(&transaction, &entry, "memory_superseded", Some(reason))?;
    transaction.commit().map_err(storage_error)?;
    Ok(entry)
}

fn insert_scope(
    connection: &Connection,
    memory_entry_id: &str,
    role: &str,
    scope: &ScopeRef,
) -> Result<(), KnowledgeError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO memory_scopes(
                 memory_entry_id, role, tenant_id, namespace, scope_type, scope_key
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                memory_entry_id,
                role,
                scope.tenant_id.as_str(),
                scope.namespace.as_str(),
                scope.scope_type.as_str(),
                scope.scope_key,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn insert_outbox(
    connection: &Connection,
    entry: &MemoryEntry,
    event_type: &str,
    reason: Option<&str>,
) -> Result<(), KnowledgeError> {
    let sequence: u64 = connection
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM memory_outbox_events",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    let event_id = format!("memory-outbox-{}-{}", entry.memory_entry_id, entry.version);
    let payload = serde_json::json!({
        "entry": entry,
        "reason": reason,
    });
    connection
        .execute(
            "INSERT INTO memory_outbox_events(
                 event_id, aggregate_id, event_type, sequence, payload_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event_id,
                entry.memory_entry_id,
                event_type,
                sequence,
                serde_json::to_string(&payload)?,
                now_ms(),
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn load_entry(
    connection: &Connection,
    memory_entry_id: &str,
) -> Result<Option<MemoryEntry>, KnowledgeError> {
    let row = connection
        .query_row(
            "SELECT tenant_id, namespace, memory_type, owner_scope_type, owner_scope_key,
                    summary, content_provider, content_resource_id, content_version, content_hash,
                    provenance_json, trust, retention, status, version
             FROM memory_entries WHERE memory_entry_id = ?1",
            [memory_entry_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, u64>(14)?,
                ))
            },
        )
        .optional()
        .map_err(storage_error)?;
    let Some((
        tenant,
        namespace,
        memory_type,
        owner_scope_type,
        owner_scope_key,
        summary,
        content_provider,
        content_resource_id,
        content_version,
        content_hash,
        provenance,
        trust,
        retention,
        status,
        version,
    )) = row
    else {
        return Ok(None);
    };
    let tenant_id = TenantId::from(tenant);
    let namespace = NamespaceId::from(namespace);
    let owner_scope = ScopeRef::new(
        tenant_id.clone(),
        namespace.clone(),
        ScopeTypeId::from(owner_scope_type),
        owner_scope_key,
    )?;
    let content_ref = match (content_provider, content_resource_id, content_hash) {
        (Some(provider), Some(resource_id), Some(content_hash)) => Some(ContentRef::new(
            ContentProviderId::from(provider),
            resource_id,
            content_version,
            content_hash,
        )?),
        _ => None,
    };
    Ok(Some(MemoryEntry {
        memory_entry_id: memory_entry_id.to_owned(),
        namespace,
        memory_type: MemoryTypeId::from(memory_type),
        owner_scope,
        visibility_scopes: load_visibility_scopes(connection, memory_entry_id)?,
        summary,
        content_ref,
        source_refs: load_sources(connection, memory_entry_id)?,
        provenance: serde_json::from_str::<Provenance>(&provenance)?,
        trust: enum_from_json(&trust)?,
        retention: enum_from_json(&retention)?,
        status: match status.as_str() {
            "active" => MemoryStatus::Active,
            "superseded" => MemoryStatus::Superseded,
            "tombstoned" => MemoryStatus::Tombstoned,
            other => {
                return Err(KnowledgeError::Storage(format!(
                    "unknown memory status: {other}"
                )))
            }
        },
        version,
    }))
}

fn load_visibility_scopes(
    connection: &Connection,
    memory_entry_id: &str,
) -> Result<Vec<ScopeRef>, KnowledgeError> {
    let mut statement = connection
        .prepare(
            "SELECT tenant_id, namespace, scope_type, scope_key
             FROM memory_scopes
             WHERE memory_entry_id = ?1 AND role = 'visibility'
             ORDER BY tenant_id, namespace, scope_type, scope_key",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([memory_entry_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(storage_error)?;
    let mut scopes = Vec::new();
    for row in rows {
        let (tenant, namespace, scope_type, scope_key) = row.map_err(storage_error)?;
        scopes.push(ScopeRef::new(
            TenantId::from(tenant),
            NamespaceId::from(namespace),
            ScopeTypeId::from(scope_type),
            scope_key,
        )?);
    }
    Ok(scopes)
}

fn load_sources(
    connection: &Connection,
    memory_entry_id: &str,
) -> Result<Vec<SourceRef>, KnowledgeError> {
    let mut statement = connection
        .prepare(
            "SELECT namespace, resource_type, resource_id, version, content_hash
             FROM memory_sources WHERE memory_entry_id = ?1 ORDER BY position",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([memory_entry_id], |row| {
            Ok(SourceRef::new(
                NamespaceId::from(row.get::<_, String>(0)?),
                ResourceTypeId::from(row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn deterministic_memory_id(proposal: &MemoryProposal) -> String {
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}",
        proposal.owner_scope.tenant_id,
        proposal.namespace,
        proposal.memory_type,
        proposal.owner_scope.stable_key(),
        proposal.idempotency_key
    );
    format!("memory-{}", &sha256_hex(identity.as_bytes())[..24])
}

fn enum_json<T: serde::Serialize>(value: &T) -> Result<String, KnowledgeError> {
    serde_json::to_string(value).map_err(KnowledgeError::from)
}

fn enum_from_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, KnowledgeError> {
    serde_json::from_str(value).map_err(KnowledgeError::from)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn storage_error(error: impl std::fmt::Display) -> KnowledgeError {
    KnowledgeError::Storage(error.to_string())
}
