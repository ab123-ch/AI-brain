use serde::{Deserialize, Serialize};

use crate::{
    ContentRef, MemoryTypeId, NamespaceId, Provenance, Result, ScopeRef, SourceRef, TenantId,
    TrustLevel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    Ephemeral,
    Task,
    LongTerm,
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Active,
    Superseded,
    Tombstoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposal {
    pub proposal_id: String,
    pub namespace: NamespaceId,
    pub memory_type: MemoryTypeId,
    pub owner_scope: ScopeRef,
    pub visibility_scopes: Vec<ScopeRef>,
    pub summary: String,
    pub content_ref: Option<ContentRef>,
    pub source_refs: Vec<SourceRef>,
    pub provenance: Provenance,
    pub trust: TrustLevel,
    pub retention: RetentionClass,
    pub idempotency_key: String,
}

impl MemoryProposal {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        proposal_id: impl Into<String>,
        namespace: NamespaceId,
        memory_type: MemoryTypeId,
        owner_scope: ScopeRef,
        summary: impl Into<String>,
        provenance: Provenance,
        trust: TrustLevel,
        retention: RetentionClass,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            proposal_id: proposal_id.into(),
            namespace,
            memory_type,
            visibility_scopes: vec![owner_scope.clone()],
            owner_scope,
            summary: summary.into(),
            content_ref: None,
            source_refs: Vec::new(),
            provenance,
            trust,
            retention,
            idempotency_key: idempotency_key.into(),
        }
    }

    #[must_use]
    pub fn with_source(mut self, source: SourceRef) -> Self {
        self.source_refs.push(source);
        self
    }

    #[must_use]
    pub fn with_visibility(mut self, scopes: impl IntoIterator<Item = ScopeRef>) -> Self {
        self.visibility_scopes = scopes.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_content_ref(mut self, content_ref: ContentRef) -> Self {
        self.content_ref = Some(content_ref);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub memory_entry_id: String,
    pub namespace: NamespaceId,
    pub memory_type: MemoryTypeId,
    pub owner_scope: ScopeRef,
    pub visibility_scopes: Vec<ScopeRef>,
    pub summary: String,
    pub content_ref: Option<ContentRef>,
    pub source_refs: Vec<SourceRef>,
    pub provenance: Provenance,
    pub trust: TrustLevel,
    pub retention: RetentionClass,
    pub status: MemoryStatus,
    pub version: u64,
}

impl MemoryEntry {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn accepted(
        memory_entry_id: impl Into<String>,
        namespace: NamespaceId,
        memory_type: MemoryTypeId,
        owner_scope: ScopeRef,
        summary: impl Into<String>,
        provenance: Provenance,
        trust: TrustLevel,
        version: u64,
    ) -> Self {
        Self {
            memory_entry_id: memory_entry_id.into(),
            namespace,
            memory_type,
            visibility_scopes: vec![owner_scope.clone()],
            owner_scope,
            summary: summary.into(),
            content_ref: None,
            source_refs: Vec::new(),
            provenance,
            trust,
            retention: RetentionClass::LongTerm,
            status: MemoryStatus::Active,
            version,
        }
    }

    #[must_use]
    pub fn with_source(mut self, source: SourceRef) -> Self {
        self.source_refs.push(source);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub tenant_id: TenantId,
    pub authorized_scopes: Vec<ScopeRef>,
    pub terms: Vec<String>,
    pub namespaces: Vec<NamespaceId>,
    pub memory_types: Vec<MemoryTypeId>,
    pub limit: usize,
}

impl MemoryQuery {
    #[must_use]
    pub fn new(
        tenant_id: TenantId,
        authorized_scopes: Vec<ScopeRef>,
        terms: Vec<String>,
        limit: usize,
    ) -> Self {
        Self {
            tenant_id,
            authorized_scopes,
            terms,
            namespaces: Vec::new(),
            memory_types: Vec::new(),
            limit,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryQueryResult {
    pub entries: Vec<MemoryEntry>,
    pub scanned: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeOutboxEvent {
    pub event_id: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub sequence: u64,
    pub payload: serde_json::Value,
}

pub trait MemoryQueryPort: Send + Sync {
    fn query(&self, query: &MemoryQuery) -> Result<MemoryQueryResult>;
}

pub trait MemoryCommandPort: Send + Sync {
    fn submit(&self, proposal: MemoryProposal) -> Result<MemoryEntry>;

    fn supersede(
        &self,
        memory_entry_id: &str,
        expected_version: u64,
        reason: &str,
    ) -> Result<MemoryEntry>;

    fn pending_outbox(&self, limit: usize) -> Result<Vec<KnowledgeOutboxEvent>>;

    fn acknowledge_outbox(&self, event_id: &str) -> Result<bool>;
}
