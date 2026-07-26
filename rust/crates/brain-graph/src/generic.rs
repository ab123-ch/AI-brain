use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use knowledge_core::{
    ContentProviderId, ContentRef, EdgeId, EvidenceLinkId, EvidenceLocator, GraphAccess, GraphEdge,
    GraphEvidenceLink, GraphMutationBatch, GraphNode, GraphQueryKind, GraphQueryPort,
    GraphQueryRequest, GraphQueryResult, GraphSubjectRef, KnowledgeError, KnowledgeSchemaRegistry,
    NamespaceId, NodeId, NodeTypeId, ProjectionAdapterId, ProjectionCheckpoint, Provenance,
    RecordLifecycle, RelationTypeId, ResourceTypeId, ScopeRef, ScopeTypeId, SourceRef, TenantId,
    TrustLevel,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

const WRITER_QUEUE_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionApplyOutcome {
    Applied,
    AlreadyApplied,
}

#[derive(Clone)]
pub struct GenericGraphStore {
    path: Arc<PathBuf>,
    schemas: Arc<KnowledgeSchemaRegistry>,
    writer: Arc<GraphWriter>,
}

struct GraphWriter {
    sender: mpsc::SyncSender<WriterCommand>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

enum WriterCommand {
    Apply {
        batch: Box<GraphMutationBatch>,
        response: mpsc::Sender<Result<ProjectionApplyOutcome, KnowledgeError>>,
    },
    Shutdown,
}

impl Drop for GraphWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(WriterCommand::Shutdown);
        if let Ok(join) = self.join.get_mut() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }
}

impl GenericGraphStore {
    pub fn open(
        path: impl AsRef<Path>,
        schemas: Arc<KnowledgeSchemaRegistry>,
    ) -> Result<Self, KnowledgeError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(storage_error)?;
        }
        let mut connection = Connection::open(&path).map_err(storage_error)?;
        configure_writer(&connection)?;
        initialize_schema(&connection)?;
        persist_schemas(&mut connection, &schemas)?;
        drop(connection);

        let (sender, receiver) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let writer_path = path.clone();
        let join = thread::Builder::new()
            .name("graph-projection-writer".into())
            .spawn(move || writer_loop(&writer_path, receiver))
            .map_err(storage_error)?;
        Ok(Self {
            path: Arc::new(path),
            schemas,
            writer: Arc::new(GraphWriter {
                sender,
                join: Mutex::new(Some(join)),
            }),
        })
    }

    pub fn apply_batch(
        &self,
        batch: GraphMutationBatch,
    ) -> Result<ProjectionApplyOutcome, KnowledgeError> {
        self.validate_batch(&batch)?;
        let (response, result) = mpsc::channel();
        self.writer
            .sender
            .send(WriterCommand::Apply {
                batch: Box::new(batch),
                response,
            })
            .map_err(|_| KnowledgeError::Unavailable("graph writer stopped".into()))?;
        result
            .recv()
            .map_err(|_| KnowledgeError::Unavailable("graph writer response lost".into()))?
    }

    pub fn checkpoint(
        &self,
        projection_key: &str,
    ) -> Result<Option<ProjectionCheckpoint>, KnowledgeError> {
        let connection = self.read_connection()?;
        load_checkpoint(&connection, projection_key)
    }

    fn validate_batch(&self, batch: &GraphMutationBatch) -> Result<(), KnowledgeError> {
        if batch.projection_key.trim().is_empty()
            || batch.source_event_id.trim().is_empty()
            || batch.source_hash.trim().is_empty()
            || batch.sequence == 0
            || batch.adapter_version == 0
        {
            return Err(KnowledgeError::InvalidInput(
                "projection identity, source hash, positive sequence, and adapter version are required"
                    .into(),
            ));
        }
        for node in &batch.nodes {
            if self.schemas.node_type(&node.node_type).is_none() {
                return Err(KnowledgeError::InvalidInput(format!(
                    "unregistered node type: {}",
                    node.node_type
                )));
            }
        }
        for edge in &batch.edges {
            if self.schemas.relation_type(&edge.relation_type).is_none() {
                return Err(KnowledgeError::InvalidInput(format!(
                    "unregistered relation type: {}",
                    edge.relation_type
                )));
            }
        }
        for evidence in &batch.evidence_links {
            if evidence.visibility_scopes.is_empty()
                || evidence
                    .visibility_scopes
                    .iter()
                    .any(|scope| scope.tenant_id != evidence.tenant_id)
            {
                return Err(KnowledgeError::InvalidInput(
                    "evidence scopes must be non-empty and use its tenant".into(),
                ));
            }
        }
        Ok(())
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
}

impl GraphQueryPort for GenericGraphStore {
    fn query(&self, query: &GraphQueryRequest) -> Result<GraphQueryResult, KnowledgeError> {
        validate_query(query)?;
        let mut connection = self.read_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage_error)?;
        let projection_seq = transaction
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM projection_checkpoints",
                [],
                |row| row.get::<_, u64>(0),
            )
            .map_err(storage_error)?;
        let mut result = match &query.kind {
            GraphQueryKind::SourcesFor { subject } => {
                query_evidence_by_subject(&transaction, subject, &query.access, query.limit)?
            }
            GraphQueryKind::SubjectsFor { source_ref } => {
                query_evidence_by_source(&transaction, source_ref, &query.access, query.limit)?
            }
            GraphQueryKind::Recall { terms } => {
                query_recall(&transaction, terms, &query.access, query.limit)?
            }
        };
        result.projection_seq = projection_seq;
        transaction.commit().map_err(storage_error)?;
        Ok(result)
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
            WriterCommand::Apply { batch, response } => {
                let result = match connection.as_mut() {
                    Ok(connection) => write_batch(connection, &batch),
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
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(storage_error)
}

fn initialize_schema(connection: &Connection) -> Result<(), KnowledgeError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS knowledge_schemas (
                 schema_id TEXT NOT NULL,
                 version INTEGER NOT NULL,
                 namespace TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 bundle_json TEXT NOT NULL,
                 PRIMARY KEY(schema_id, version)
             );

             CREATE TABLE IF NOT EXISTS graph_nodes (
                 node_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 node_type TEXT NOT NULL,
                 entity_key TEXT NOT NULL,
                 properties_json TEXT NOT NULL,
                 lifecycle TEXT NOT NULL,
                 UNIQUE(tenant_id, namespace, node_type, entity_key)
             );
             CREATE INDEX IF NOT EXISTS idx_graph_nodes_lookup
                 ON graph_nodes(tenant_id, namespace, node_type, lifecycle);

             CREATE TABLE IF NOT EXISTS graph_edges (
                 edge_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 relation_type TEXT NOT NULL,
                 source_node_id TEXT NOT NULL,
                 target_node_id TEXT NOT NULL,
                 properties_json TEXT NOT NULL,
                 lifecycle TEXT NOT NULL,
                 FOREIGN KEY(source_node_id) REFERENCES graph_nodes(node_id),
                 FOREIGN KEY(target_node_id) REFERENCES graph_nodes(node_id)
             );
             CREATE INDEX IF NOT EXISTS idx_graph_edges_source
                 ON graph_edges(source_node_id, relation_type, lifecycle);
             CREATE INDEX IF NOT EXISTS idx_graph_edges_target
                 ON graph_edges(target_node_id, relation_type, lifecycle);

             CREATE TABLE IF NOT EXISTS graph_subject_scopes (
                 subject_kind TEXT NOT NULL,
                 subject_id TEXT NOT NULL,
                 tenant_id TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 scope_type TEXT NOT NULL,
                 scope_key TEXT NOT NULL,
                 PRIMARY KEY(subject_kind, subject_id, tenant_id, namespace, scope_type, scope_key)
             );
             CREATE INDEX IF NOT EXISTS idx_graph_subject_scopes_lookup
                 ON graph_subject_scopes(tenant_id, namespace, scope_type, scope_key);

             CREATE TABLE IF NOT EXISTS graph_evidence_links (
                 evidence_link_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 subject_kind TEXT NOT NULL,
                 subject_id TEXT NOT NULL,
                 source_namespace TEXT NOT NULL,
                 source_resource_type TEXT NOT NULL,
                 source_resource_id TEXT NOT NULL,
                 source_version TEXT,
                 source_hash TEXT,
                 content_provider TEXT,
                 content_resource_id TEXT,
                 content_version TEXT,
                 content_hash TEXT,
                 locator_json TEXT,
                 provenance_json TEXT NOT NULL,
                 trust TEXT NOT NULL,
                 observed_version TEXT,
                 lifecycle TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_graph_evidence_subject
                 ON graph_evidence_links(subject_kind, subject_id, lifecycle);
             CREATE INDEX IF NOT EXISTS idx_graph_evidence_source
                 ON graph_evidence_links(
                     source_namespace, source_resource_type, source_resource_id,
                     source_version, source_hash, lifecycle
                 );

             CREATE TABLE IF NOT EXISTS graph_evidence_scopes (
                 evidence_link_id TEXT NOT NULL,
                 tenant_id TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 scope_type TEXT NOT NULL,
                 scope_key TEXT NOT NULL,
                 PRIMARY KEY(evidence_link_id, tenant_id, namespace, scope_type, scope_key),
                 FOREIGN KEY(evidence_link_id) REFERENCES graph_evidence_links(evidence_link_id)
                     ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_graph_evidence_scopes_lookup
                 ON graph_evidence_scopes(tenant_id, namespace, scope_type, scope_key);

             CREATE TABLE IF NOT EXISTS projection_checkpoints (
                 projection_key TEXT PRIMARY KEY,
                 sequence INTEGER NOT NULL,
                 source_event_id TEXT NOT NULL,
                 source_hash TEXT NOT NULL,
                 adapter_id TEXT NOT NULL,
                 adapter_version INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS projection_events (
                 source_event_id TEXT PRIMARY KEY,
                 projection_key TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 source_hash TEXT NOT NULL,
                 UNIQUE(projection_key, sequence)
             );",
        )
        .map_err(storage_error)
}

fn persist_schemas(
    connection: &mut Connection,
    schemas: &KnowledgeSchemaRegistry,
) -> Result<(), KnowledgeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    for bundle in schemas.bundles() {
        let bundle_json = serde_json::to_string(&bundle)?;
        let content_hash = knowledge_core::sha256_hex(bundle_json.as_bytes());
        let existing = transaction
            .query_row(
                "SELECT content_hash FROM knowledge_schemas
                 WHERE schema_id = ?1 AND version = ?2",
                params![bundle.schema_id, bundle.version],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_error)?;
        if existing.as_ref().is_some_and(|hash| hash != &content_hash) {
            return Err(KnowledgeError::SchemaConflict {
                schema_id: bundle.schema_id,
                version: bundle.version,
                reason: "persisted schema hash differs".into(),
            });
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO knowledge_schemas(
                     schema_id, version, namespace, content_hash, bundle_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    bundle.schema_id,
                    bundle.version,
                    bundle.namespace.as_str(),
                    content_hash,
                    bundle_json,
                ],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn write_batch(
    connection: &mut Connection,
    batch: &GraphMutationBatch,
) -> Result<ProjectionApplyOutcome, KnowledgeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    if let Some(existing) = load_checkpoint(&transaction, &batch.projection_key)? {
        if existing.sequence == batch.sequence
            && existing.source_event_id == batch.source_event_id
            && existing.source_hash == batch.source_hash
            && existing.adapter_id == batch.adapter_id
            && existing.adapter_version == batch.adapter_version
        {
            transaction.commit().map_err(storage_error)?;
            return Ok(ProjectionApplyOutcome::AlreadyApplied);
        }
        if batch.sequence <= existing.sequence || batch.sequence != existing.sequence + 1 {
            return Err(KnowledgeError::StaleRevision {
                record_id: batch.projection_key.clone(),
                expected: existing.sequence + 1,
                actual: batch.sequence,
            });
        }
    } else if batch.sequence != 1 {
        return Err(KnowledgeError::StaleRevision {
            record_id: batch.projection_key.clone(),
            expected: 1,
            actual: batch.sequence,
        });
    }

    for node in &batch.nodes {
        upsert_node(&transaction, node)?;
    }
    for edge in &batch.edges {
        upsert_edge(&transaction, edge)?;
    }
    for evidence in &batch.evidence_links {
        upsert_evidence(&transaction, evidence)?;
    }
    for subject in &batch.tombstones {
        tombstone_subject(&transaction, subject)?;
    }
    transaction
        .execute(
            "INSERT INTO projection_events(
                 source_event_id, projection_key, sequence, source_hash
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                batch.source_event_id,
                batch.projection_key,
                batch.sequence,
                batch.source_hash,
            ],
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO projection_checkpoints(
                 projection_key, sequence, source_event_id, source_hash,
                 adapter_id, adapter_version, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(projection_key) DO UPDATE SET
                 sequence = excluded.sequence,
                 source_event_id = excluded.source_event_id,
                 source_hash = excluded.source_hash,
                 adapter_id = excluded.adapter_id,
                 adapter_version = excluded.adapter_version,
                 updated_at = excluded.updated_at",
            params![
                batch.projection_key,
                batch.sequence,
                batch.source_event_id,
                batch.source_hash,
                batch.adapter_id.as_str(),
                batch.adapter_version,
                now_ms(),
            ],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)?;
    Ok(ProjectionApplyOutcome::Applied)
}

fn upsert_node(connection: &Connection, node: &GraphNode) -> Result<(), KnowledgeError> {
    connection
        .execute(
            "INSERT INTO graph_nodes(
                 node_id, tenant_id, namespace, node_type, entity_key,
                 properties_json, lifecycle
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(node_id) DO UPDATE SET
                 properties_json = excluded.properties_json,
                 lifecycle = excluded.lifecycle",
            params![
                node.node_id.as_str(),
                node.tenant_id.as_str(),
                node.namespace.as_str(),
                node.node_type.as_str(),
                node.entity_key,
                serde_json::to_string(&node.properties)?,
                lifecycle_db(node.lifecycle),
            ],
        )
        .map_err(storage_error)?;
    replace_subject_scopes(
        connection,
        "node",
        node.node_id.as_str(),
        &node.visibility_scopes,
    )
}

fn upsert_edge(connection: &Connection, edge: &GraphEdge) -> Result<(), KnowledgeError> {
    connection
        .execute(
            "INSERT INTO graph_edges(
                 edge_id, tenant_id, namespace, relation_type,
                 source_node_id, target_node_id, properties_json, lifecycle
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(edge_id) DO UPDATE SET
                 properties_json = excluded.properties_json,
                 lifecycle = excluded.lifecycle",
            params![
                edge.edge_id.as_str(),
                edge.tenant_id.as_str(),
                edge.namespace.as_str(),
                edge.relation_type.as_str(),
                edge.source_node_id.as_str(),
                edge.target_node_id.as_str(),
                serde_json::to_string(&edge.properties)?,
                lifecycle_db(edge.lifecycle),
            ],
        )
        .map_err(storage_error)?;
    replace_subject_scopes(
        connection,
        "edge",
        edge.edge_id.as_str(),
        &edge.visibility_scopes,
    )
}

fn upsert_evidence(
    connection: &Connection,
    evidence: &GraphEvidenceLink,
) -> Result<(), KnowledgeError> {
    let (subject_kind, subject_id) = subject_parts(&evidence.subject);
    let subject_exists = match subject_kind {
        "node" => connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM graph_nodes WHERE node_id = ?1)",
                [subject_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?,
        "edge" => connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM graph_edges WHERE edge_id = ?1)",
                [subject_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?,
        _ => false,
    };
    if !subject_exists {
        return Err(KnowledgeError::InvalidInput(format!(
            "evidence subject does not exist: {subject_kind}:{subject_id}"
        )));
    }
    let (content_provider, content_resource_id, content_version, content_hash) = evidence
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
    connection
        .execute(
            "INSERT INTO graph_evidence_links(
                 evidence_link_id, tenant_id, namespace, subject_kind, subject_id,
                 source_namespace, source_resource_type, source_resource_id,
                 source_version, source_hash,
                 content_provider, content_resource_id, content_version, content_hash,
                 locator_json, provenance_json, trust, observed_version, lifecycle
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19
             )
             ON CONFLICT(evidence_link_id) DO UPDATE SET
                 locator_json = excluded.locator_json,
                 provenance_json = excluded.provenance_json,
                 trust = excluded.trust,
                 observed_version = excluded.observed_version,
                 lifecycle = excluded.lifecycle",
            params![
                evidence.evidence_link_id.as_str(),
                evidence.tenant_id.as_str(),
                evidence.namespace.as_str(),
                subject_kind,
                subject_id,
                evidence.source_ref.namespace.as_str(),
                evidence.source_ref.resource_type.as_str(),
                evidence.source_ref.resource_id,
                evidence.source_ref.version,
                evidence.source_ref.content_hash,
                content_provider,
                content_resource_id,
                content_version,
                content_hash,
                evidence
                    .locator
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                serde_json::to_string(&evidence.provenance)?,
                serde_json::to_string(&evidence.trust)?,
                evidence.observed_version,
                lifecycle_db(evidence.lifecycle),
            ],
        )
        .map_err(storage_error)?;
    connection
        .execute(
            "DELETE FROM graph_evidence_scopes WHERE evidence_link_id = ?1",
            [evidence.evidence_link_id.as_str()],
        )
        .map_err(storage_error)?;
    for scope in &evidence.visibility_scopes {
        connection
            .execute(
                "INSERT INTO graph_evidence_scopes(
                     evidence_link_id, tenant_id, namespace, scope_type, scope_key
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    evidence.evidence_link_id.as_str(),
                    scope.tenant_id.as_str(),
                    scope.namespace.as_str(),
                    scope.scope_type.as_str(),
                    scope.scope_key,
                ],
            )
            .map_err(storage_error)?;
    }
    Ok(())
}

fn replace_subject_scopes(
    connection: &Connection,
    subject_kind: &str,
    subject_id: &str,
    scopes: &[ScopeRef],
) -> Result<(), KnowledgeError> {
    connection
        .execute(
            "DELETE FROM graph_subject_scopes WHERE subject_kind = ?1 AND subject_id = ?2",
            params![subject_kind, subject_id],
        )
        .map_err(storage_error)?;
    for scope in scopes {
        connection
            .execute(
                "INSERT INTO graph_subject_scopes(
                     subject_kind, subject_id, tenant_id, namespace, scope_type, scope_key
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    subject_kind,
                    subject_id,
                    scope.tenant_id.as_str(),
                    scope.namespace.as_str(),
                    scope.scope_type.as_str(),
                    scope.scope_key,
                ],
            )
            .map_err(storage_error)?;
    }
    Ok(())
}

fn tombstone_subject(
    connection: &Connection,
    subject: &GraphSubjectRef,
) -> Result<(), KnowledgeError> {
    match subject {
        GraphSubjectRef::Node(id) => {
            connection
                .execute(
                    "UPDATE graph_nodes SET lifecycle = 'tombstoned' WHERE node_id = ?1",
                    [id.as_str()],
                )
                .map_err(storage_error)?;
        }
        GraphSubjectRef::Edge(id) => {
            connection
                .execute(
                    "UPDATE graph_edges SET lifecycle = 'tombstoned' WHERE edge_id = ?1",
                    [id.as_str()],
                )
                .map_err(storage_error)?;
        }
    }
    Ok(())
}

fn load_checkpoint(
    connection: &Connection,
    projection_key: &str,
) -> Result<Option<ProjectionCheckpoint>, KnowledgeError> {
    connection
        .query_row(
            "SELECT sequence, source_event_id, source_hash, adapter_id, adapter_version
             FROM projection_checkpoints WHERE projection_key = ?1",
            [projection_key],
            |row| {
                Ok(ProjectionCheckpoint {
                    projection_key: projection_key.to_owned(),
                    sequence: row.get(0)?,
                    source_event_id: row.get(1)?,
                    source_hash: row.get(2)?,
                    adapter_id: ProjectionAdapterId::from(row.get::<_, String>(3)?),
                    adapter_version: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(storage_error)
}

fn query_evidence_by_subject(
    connection: &Connection,
    subject: &GraphSubjectRef,
    access: &GraphAccess,
    limit: usize,
) -> Result<GraphQueryResult, KnowledgeError> {
    let (kind, id) = subject_parts(subject);
    let candidates = load_evidence_rows(
        connection,
        "subject_kind = ?1 AND subject_id = ?2",
        &[kind, id],
    )?;
    filter_evidence(connection, candidates, access, limit)
}

fn query_evidence_by_source(
    connection: &Connection,
    source: &SourceRef,
    access: &GraphAccess,
    limit: usize,
) -> Result<GraphQueryResult, KnowledgeError> {
    let mut statement = connection
        .prepare(&format!(
            "{} WHERE source_namespace = ?1 AND source_resource_type = ?2
             AND source_resource_id = ?3
             AND source_version IS ?4 AND source_hash IS ?5
             AND lifecycle = 'active'
             ORDER BY evidence_link_id",
            evidence_select()
        ))
        .map_err(storage_error)?;
    let rows = statement
        .query_map(
            params![
                source.namespace.as_str(),
                source.resource_type.as_str(),
                source.resource_id,
                source.version,
                source.content_hash,
            ],
            evidence_row,
        )
        .map_err(storage_error)?;
    let candidates = collect_evidence_rows(rows)?;
    filter_evidence(connection, candidates, access, limit)
}

fn query_recall(
    connection: &Connection,
    terms: &[String],
    access: &GraphAccess,
    limit: usize,
) -> Result<GraphQueryResult, KnowledgeError> {
    let normalized_terms = terms
        .iter()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let mut statement = connection
        .prepare(
            "SELECT node_id, tenant_id, namespace, node_type, entity_key,
                    properties_json, lifecycle
             FROM graph_nodes
             WHERE tenant_id = ?1 AND namespace = ?2 AND lifecycle = 'active'
             ORDER BY node_id",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map(
            params![access.tenant_id.as_str(), access.namespace.as_str()],
            node_row,
        )
        .map_err(storage_error)?;
    let candidates = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)?;
    let mut nodes = Vec::new();
    let mut scanned = 0usize;
    for mut node in candidates {
        node.visibility_scopes = load_subject_scopes(connection, "node", node.node_id.as_str())?;
        if !authorized(&node.visibility_scopes, access) {
            continue;
        }
        scanned += 1;
        let haystack = format!(
            "{} {}",
            node.entity_key.to_lowercase(),
            serde_json::to_string(&node.properties)?.to_lowercase()
        );
        if !normalized_terms.is_empty()
            && !normalized_terms.iter().any(|term| haystack.contains(term))
        {
            continue;
        }
        nodes.push(node);
        if nodes.len() > limit {
            break;
        }
    }
    let truncated = nodes.len() > limit;
    nodes.truncate(limit);
    let selected = nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<HashSet<_>>();
    let mut edges = load_edges_between(connection, &selected, access)?;
    edges.truncate(limit);
    let mut evidence_links = Vec::new();
    for node in &nodes {
        let mut result = query_evidence_by_subject(
            connection,
            &GraphSubjectRef::Node(node.node_id.clone()),
            access,
            limit.saturating_sub(evidence_links.len()).max(1),
        )?;
        evidence_links.append(&mut result.evidence_links);
        if evidence_links.len() >= limit {
            evidence_links.truncate(limit);
            break;
        }
    }
    Ok(GraphQueryResult {
        nodes,
        edges,
        evidence_links,
        projection_seq: 0,
        is_stale: false,
        lag: 0,
        truncated,
        scanned,
    })
}

fn load_evidence_rows(
    connection: &Connection,
    predicate: &str,
    values: &[&str],
) -> Result<Vec<GraphEvidenceLink>, KnowledgeError> {
    let mut statement = connection
        .prepare(&format!(
            "{} WHERE {predicate} AND lifecycle = 'active' ORDER BY evidence_link_id",
            evidence_select()
        ))
        .map_err(storage_error)?;
    let rows = statement
        .query_map(params![values[0], values[1]], evidence_row)
        .map_err(storage_error)?;
    collect_evidence_rows(rows)
}

fn collect_evidence_rows(
    rows: rusqlite::MappedRows<
        '_,
        impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<GraphEvidenceLink>,
    >,
) -> Result<Vec<GraphEvidenceLink>, KnowledgeError> {
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn filter_evidence(
    connection: &Connection,
    candidates: Vec<GraphEvidenceLink>,
    access: &GraphAccess,
    limit: usize,
) -> Result<GraphQueryResult, KnowledgeError> {
    let mut evidence_links = Vec::new();
    let mut scanned = 0usize;
    for mut evidence in candidates {
        if evidence.tenant_id != access.tenant_id || evidence.namespace != access.namespace {
            continue;
        }
        evidence.visibility_scopes =
            load_evidence_scopes(connection, evidence.evidence_link_id.as_str())?;
        if !authorized(&evidence.visibility_scopes, access) {
            continue;
        }
        scanned += 1;
        evidence_links.push(evidence);
        if evidence_links.len() > limit {
            break;
        }
    }
    let truncated = evidence_links.len() > limit;
    evidence_links.truncate(limit);
    Ok(GraphQueryResult {
        evidence_links,
        truncated,
        scanned,
        ..GraphQueryResult::default()
    })
}

fn evidence_select() -> &'static str {
    "SELECT evidence_link_id, tenant_id, namespace, subject_kind, subject_id,
            source_namespace, source_resource_type, source_resource_id,
            source_version, source_hash,
            content_provider, content_resource_id, content_version, content_hash,
            locator_json, provenance_json, trust, observed_version, lifecycle
     FROM graph_evidence_links"
}

fn evidence_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEvidenceLink> {
    let subject_kind: String = row.get(3)?;
    let subject_id: String = row.get(4)?;
    let content_provider: Option<String> = row.get(10)?;
    let content_resource_id: Option<String> = row.get(11)?;
    let content_version: Option<String> = row.get(12)?;
    let content_hash: Option<String> = row.get(13)?;
    let locator_json: Option<String> = row.get(14)?;
    let provenance_json: String = row.get(15)?;
    let trust_json: String = row.get(16)?;
    let lifecycle: String = row.get(18)?;
    Ok(GraphEvidenceLink {
        evidence_link_id: EvidenceLinkId::from(row.get::<_, String>(0)?),
        tenant_id: TenantId::from(row.get::<_, String>(1)?),
        namespace: NamespaceId::from(row.get::<_, String>(2)?),
        subject: match subject_kind.as_str() {
            "node" => GraphSubjectRef::Node(NodeId::from(subject_id)),
            "edge" => GraphSubjectRef::Edge(EdgeId::from(subject_id)),
            _ => return Err(invalid_column("unknown graph subject kind")),
        },
        source_ref: SourceRef::new(
            NamespaceId::from(row.get::<_, String>(5)?),
            ResourceTypeId::from(row.get::<_, String>(6)?),
            row.get::<_, String>(7)?,
            row.get(8)?,
            row.get(9)?,
        ),
        content_ref: match (content_provider, content_resource_id, content_hash) {
            (Some(provider), Some(resource_id), Some(hash)) => Some(
                ContentRef::new(
                    ContentProviderId::from(provider),
                    resource_id,
                    content_version,
                    hash,
                )
                .map_err(|error| invalid_column(&error.to_string()))?,
            ),
            _ => None,
        },
        locator: locator_json
            .map(|value| serde_json::from_str::<EvidenceLocator>(&value))
            .transpose()
            .map_err(|error| invalid_column(&error.to_string()))?,
        visibility_scopes: Vec::new(),
        provenance: serde_json::from_str::<Provenance>(&provenance_json)
            .map_err(|error| invalid_column(&error.to_string()))?,
        trust: serde_json::from_str::<TrustLevel>(&trust_json)
            .map_err(|error| invalid_column(&error.to_string()))?,
        observed_version: row.get(17)?,
        lifecycle: lifecycle_from_db(&lifecycle)
            .map_err(|error| invalid_column(&error.to_string()))?,
    })
}

fn node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let properties: String = row.get(5)?;
    let lifecycle: String = row.get(6)?;
    Ok(GraphNode {
        node_id: NodeId::from(row.get::<_, String>(0)?),
        tenant_id: TenantId::from(row.get::<_, String>(1)?),
        namespace: NamespaceId::from(row.get::<_, String>(2)?),
        node_type: NodeTypeId::from(row.get::<_, String>(3)?),
        entity_key: row.get(4)?,
        properties: serde_json::from_str(&properties)
            .map_err(|error| invalid_column(&error.to_string()))?,
        visibility_scopes: Vec::new(),
        lifecycle: lifecycle_from_db(&lifecycle)
            .map_err(|error| invalid_column(&error.to_string()))?,
    })
}

fn edge_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEdge> {
    let properties: String = row.get(6)?;
    let lifecycle: String = row.get(7)?;
    Ok(GraphEdge {
        edge_id: EdgeId::from(row.get::<_, String>(0)?),
        tenant_id: TenantId::from(row.get::<_, String>(1)?),
        namespace: NamespaceId::from(row.get::<_, String>(2)?),
        relation_type: RelationTypeId::from(row.get::<_, String>(3)?),
        source_node_id: NodeId::from(row.get::<_, String>(4)?),
        target_node_id: NodeId::from(row.get::<_, String>(5)?),
        properties: serde_json::from_str(&properties)
            .map_err(|error| invalid_column(&error.to_string()))?,
        visibility_scopes: Vec::new(),
        lifecycle: lifecycle_from_db(&lifecycle)
            .map_err(|error| invalid_column(&error.to_string()))?,
    })
}

fn load_edges_between(
    connection: &Connection,
    selected: &HashSet<NodeId>,
    access: &GraphAccess,
) -> Result<Vec<GraphEdge>, KnowledgeError> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT edge_id, tenant_id, namespace, relation_type,
                    source_node_id, target_node_id, properties_json, lifecycle
             FROM graph_edges
             WHERE tenant_id = ?1 AND namespace = ?2 AND lifecycle = 'active'
             ORDER BY edge_id",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map(
            params![access.tenant_id.as_str(), access.namespace.as_str()],
            edge_row,
        )
        .map_err(storage_error)?;
    let mut edges = Vec::new();
    for row in rows {
        let mut edge = row.map_err(storage_error)?;
        if !selected.contains(&edge.source_node_id) || !selected.contains(&edge.target_node_id) {
            continue;
        }
        edge.visibility_scopes = load_subject_scopes(connection, "edge", edge.edge_id.as_str())?;
        if authorized(&edge.visibility_scopes, access) {
            edges.push(edge);
        }
    }
    Ok(edges)
}

fn load_subject_scopes(
    connection: &Connection,
    subject_kind: &str,
    subject_id: &str,
) -> Result<Vec<ScopeRef>, KnowledgeError> {
    load_scopes(
        connection,
        "graph_subject_scopes",
        "subject_kind = ?1 AND subject_id = ?2",
        &[subject_kind, subject_id],
    )
}

fn load_evidence_scopes(
    connection: &Connection,
    evidence_link_id: &str,
) -> Result<Vec<ScopeRef>, KnowledgeError> {
    let mut statement = connection
        .prepare(
            "SELECT tenant_id, namespace, scope_type, scope_key
             FROM graph_evidence_scopes WHERE evidence_link_id = ?1
             ORDER BY tenant_id, namespace, scope_type, scope_key",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([evidence_link_id], scope_row)
        .map_err(storage_error)?;
    collect_scopes(rows)
}

fn load_scopes(
    connection: &Connection,
    table: &str,
    predicate: &str,
    values: &[&str],
) -> Result<Vec<ScopeRef>, KnowledgeError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT tenant_id, namespace, scope_type, scope_key
             FROM {table} WHERE {predicate}
             ORDER BY tenant_id, namespace, scope_type, scope_key"
        ))
        .map_err(storage_error)?;
    let rows = statement
        .query_map(params![values[0], values[1]], scope_row)
        .map_err(storage_error)?;
    collect_scopes(rows)
}

fn scope_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScopeRef> {
    ScopeRef::new(
        TenantId::from(row.get::<_, String>(0)?),
        NamespaceId::from(row.get::<_, String>(1)?),
        ScopeTypeId::from(row.get::<_, String>(2)?),
        row.get::<_, String>(3)?,
    )
    .map_err(|error| invalid_column(&error.to_string()))
}

fn collect_scopes(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<ScopeRef>>,
) -> Result<Vec<ScopeRef>, KnowledgeError> {
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn authorized(scopes: &[ScopeRef], access: &GraphAccess) -> bool {
    scopes.iter().any(|scope| {
        scope.tenant_id == access.tenant_id && access.authorized_scopes.contains(scope)
    })
}

fn validate_query(query: &GraphQueryRequest) -> Result<(), KnowledgeError> {
    if query.limit == 0 || query.access.authorized_scopes.is_empty() {
        return Err(KnowledgeError::InvalidInput(
            "graph query requires scopes and a positive limit".into(),
        ));
    }
    if query.access.authorized_scopes.iter().any(|scope| {
        scope.tenant_id != query.access.tenant_id || scope.namespace != query.access.namespace
    }) {
        return Err(KnowledgeError::Unauthorized(
            "graph query scopes must match tenant and namespace".into(),
        ));
    }
    Ok(())
}

fn subject_parts(subject: &GraphSubjectRef) -> (&'static str, &str) {
    match subject {
        GraphSubjectRef::Node(id) => ("node", id.as_str()),
        GraphSubjectRef::Edge(id) => ("edge", id.as_str()),
    }
}

fn lifecycle_db(lifecycle: RecordLifecycle) -> &'static str {
    match lifecycle {
        RecordLifecycle::Active => "active",
        RecordLifecycle::Superseded => "superseded",
        RecordLifecycle::Tombstoned => "tombstoned",
    }
}

fn lifecycle_from_db(value: &str) -> Result<RecordLifecycle, KnowledgeError> {
    match value {
        "active" => Ok(RecordLifecycle::Active),
        "superseded" => Ok(RecordLifecycle::Superseded),
        "tombstoned" => Ok(RecordLifecycle::Tombstoned),
        other => Err(KnowledgeError::Storage(format!(
            "unknown graph lifecycle: {other}"
        ))),
    }
}

fn invalid_column(message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.to_owned(),
        )),
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn storage_error(error: impl std::fmt::Display) -> KnowledgeError {
    KnowledgeError::Storage(error.to_string())
}
