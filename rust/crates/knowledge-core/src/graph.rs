use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    dedupe_scopes, require_non_empty, sha256_hex, validate_scope_set, ContentRef, EdgeId,
    EvidenceLinkId, NamespaceId, NodeId, NodeTypeId, ProjectionAdapterId, Provenance,
    RelationTypeId, Result, ScopeRef, SourceRef, TenantId, TrustLevel,
};

pub type PropertyMap = BTreeMap<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordLifecycle {
    Active,
    Superseded,
    Tombstoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub node_id: NodeId,
    pub tenant_id: TenantId,
    pub namespace: NamespaceId,
    pub node_type: NodeTypeId,
    pub entity_key: String,
    pub properties: PropertyMap,
    pub visibility_scopes: Vec<ScopeRef>,
    pub lifecycle: RecordLifecycle,
}

impl GraphNode {
    pub fn new(
        tenant_id: TenantId,
        namespace: NamespaceId,
        node_type: NodeTypeId,
        entity_key: impl Into<String>,
        visibility_scopes: impl IntoIterator<Item = ScopeRef>,
    ) -> Result<Self> {
        let entity_key = entity_key.into();
        require_non_empty("entity_key", &entity_key)?;
        let visibility_scopes = dedupe_scopes(visibility_scopes.into_iter().collect());
        validate_scope_set(&tenant_id, &visibility_scopes)?;
        let node_id = NodeId::from(stable_id(
            "node",
            &[
                tenant_id.as_str(),
                namespace.as_str(),
                node_type.as_str(),
                &entity_key,
            ],
        ));
        Ok(Self {
            node_id,
            tenant_id,
            namespace,
            node_type,
            entity_key,
            properties: PropertyMap::new(),
            visibility_scopes,
            lifecycle: RecordLifecycle::Active,
        })
    }

    #[must_use]
    pub fn with_property(mut self, key: impl Into<String>, value: Value) -> Self {
        self.properties.insert(key.into(), value);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub edge_id: EdgeId,
    pub tenant_id: TenantId,
    pub namespace: NamespaceId,
    pub relation_type: RelationTypeId,
    pub source_node_id: NodeId,
    pub target_node_id: NodeId,
    pub properties: PropertyMap,
    pub visibility_scopes: Vec<ScopeRef>,
    pub lifecycle: RecordLifecycle,
}

impl GraphEdge {
    pub fn new(
        tenant_id: TenantId,
        namespace: NamespaceId,
        relation_type: RelationTypeId,
        source_node_id: impl Into<NodeId>,
        target_node_id: impl Into<NodeId>,
        visibility_scopes: impl IntoIterator<Item = ScopeRef>,
    ) -> Result<Self> {
        let source_node_id = source_node_id.into();
        let target_node_id = target_node_id.into();
        let visibility_scopes = dedupe_scopes(visibility_scopes.into_iter().collect());
        validate_scope_set(&tenant_id, &visibility_scopes)?;
        let edge_id = EdgeId::from(stable_id(
            "edge",
            &[
                tenant_id.as_str(),
                namespace.as_str(),
                relation_type.as_str(),
                source_node_id.as_str(),
                target_node_id.as_str(),
            ],
        ));
        Ok(Self {
            edge_id,
            tenant_id,
            namespace,
            relation_type,
            source_node_id,
            target_node_id,
            properties: PropertyMap::new(),
            visibility_scopes,
            lifecycle: RecordLifecycle::Active,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum GraphSubjectRef {
    Node(NodeId),
    Edge(EdgeId),
}

impl GraphSubjectRef {
    #[must_use]
    pub fn stable_key(&self) -> String {
        match self {
            Self::Node(id) => format!("node:{id}"),
            Self::Edge(id) => format!("edge:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLocator {
    pub locator_type: String,
    pub version: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEvidenceLink {
    pub evidence_link_id: EvidenceLinkId,
    pub tenant_id: TenantId,
    pub namespace: NamespaceId,
    pub subject: GraphSubjectRef,
    pub source_ref: SourceRef,
    pub content_ref: Option<ContentRef>,
    pub locator: Option<EvidenceLocator>,
    pub visibility_scopes: Vec<ScopeRef>,
    pub provenance: Provenance,
    pub trust: TrustLevel,
    pub observed_version: Option<String>,
    pub lifecycle: RecordLifecycle,
}

impl GraphEvidenceLink {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        subject: GraphSubjectRef,
        source_ref: SourceRef,
        content_ref: Option<ContentRef>,
        visibility_scopes: impl IntoIterator<Item = ScopeRef>,
        provenance: Provenance,
        trust: TrustLevel,
        adapter_version: &str,
    ) -> Result<Self> {
        let visibility_scopes = dedupe_scopes(visibility_scopes.into_iter().collect());
        let Some(first_scope) = visibility_scopes.first() else {
            return Err(crate::KnowledgeError::InvalidInput(
                "evidence requires a visibility scope".into(),
            ));
        };
        validate_scope_set(&first_scope.tenant_id, &visibility_scopes)?;
        let evidence_link_id = EvidenceLinkId::from(stable_id(
            "evidence",
            &[
                &subject.stable_key(),
                &source_ref.stable_key(),
                content_ref
                    .as_ref()
                    .map_or("", |reference| reference.content_hash.as_str()),
                adapter_version,
            ],
        ));
        Ok(Self {
            evidence_link_id,
            tenant_id: first_scope.tenant_id.clone(),
            namespace: source_ref.namespace.clone(),
            subject,
            observed_version: source_ref.version.clone(),
            source_ref,
            content_ref,
            locator: None,
            visibility_scopes,
            provenance,
            trust,
            lifecycle: RecordLifecycle::Active,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphMutationBatch {
    pub projection_key: String,
    pub sequence: u64,
    pub source_event_id: String,
    pub adapter_id: ProjectionAdapterId,
    pub adapter_version: u32,
    pub source_hash: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub evidence_links: Vec<GraphEvidenceLink>,
    pub tombstones: Vec<GraphSubjectRef>,
}

impl GraphMutationBatch {
    #[must_use]
    pub fn new(
        projection_key: impl Into<String>,
        sequence: u64,
        source_event_id: impl Into<String>,
        adapter_id: ProjectionAdapterId,
        adapter_version: u32,
        source_hash: impl Into<String>,
    ) -> Self {
        Self {
            projection_key: projection_key.into(),
            sequence,
            source_event_id: source_event_id.into(),
            adapter_id,
            adapter_version,
            source_hash: source_hash.into(),
            nodes: Vec::new(),
            edges: Vec::new(),
            evidence_links: Vec::new(),
            tombstones: Vec::new(),
        }
    }

    #[must_use]
    pub fn empty(
        projection_key: impl Into<String>,
        sequence: u64,
        source_event_id: impl Into<String>,
        adapter_id: ProjectionAdapterId,
        adapter_version: u32,
        source_hash: impl Into<String>,
    ) -> Self {
        Self::new(
            projection_key,
            sequence,
            source_event_id,
            adapter_id,
            adapter_version,
            source_hash,
        )
    }

    #[must_use]
    pub fn with_node(mut self, node: GraphNode) -> Self {
        self.nodes.push(node);
        self
    }

    #[must_use]
    pub fn with_edge(mut self, edge: GraphEdge) -> Self {
        self.edges.push(edge);
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence: GraphEvidenceLink) -> Self {
        self.evidence_links.push(evidence);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionCheckpoint {
    pub projection_key: String,
    pub sequence: u64,
    pub source_event_id: String,
    pub source_hash: String,
    pub adapter_id: ProjectionAdapterId,
    pub adapter_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphAccess {
    pub tenant_id: TenantId,
    pub namespace: NamespaceId,
    pub authorized_scopes: Vec<ScopeRef>,
}

impl GraphAccess {
    #[must_use]
    pub fn new(
        tenant_id: TenantId,
        namespace: NamespaceId,
        authorized_scopes: Vec<ScopeRef>,
    ) -> Self {
        Self {
            tenant_id,
            namespace,
            authorized_scopes: dedupe_scopes(authorized_scopes),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphQueryKind {
    Recall { terms: Vec<String> },
    SourcesFor { subject: GraphSubjectRef },
    SubjectsFor { source_ref: SourceRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphQueryRequest {
    pub access: GraphAccess,
    pub kind: GraphQueryKind,
    pub limit: usize,
}

impl GraphQueryRequest {
    #[must_use]
    pub fn recall(access: GraphAccess, terms: Vec<String>, limit: usize) -> Self {
        Self {
            access,
            kind: GraphQueryKind::Recall { terms },
            limit,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphQueryResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub evidence_links: Vec<GraphEvidenceLink>,
    pub projection_seq: u64,
    pub is_stale: bool,
    pub lag: u64,
    pub truncated: bool,
    pub scanned: usize,
}

pub trait GraphQueryPort: Send + Sync {
    fn query(&self, query: &GraphQueryRequest) -> Result<GraphQueryResult>;

    fn sources_for(
        &self,
        subject: &GraphSubjectRef,
        access: &GraphAccess,
        limit: usize,
    ) -> Result<GraphQueryResult> {
        self.query(&GraphQueryRequest {
            access: access.clone(),
            kind: GraphQueryKind::SourcesFor {
                subject: subject.clone(),
            },
            limit,
        })
    }

    fn subjects_for(
        &self,
        source_ref: &SourceRef,
        access: &GraphAccess,
        limit: usize,
    ) -> Result<GraphQueryResult> {
        self.query(&GraphQueryRequest {
            access: access.clone(),
            kind: GraphQueryKind::SubjectsFor {
                source_ref: source_ref.clone(),
            },
            limit,
        })
    }
}

fn stable_id(prefix: &str, parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(part.as_bytes());
        bytes.push(0);
    }
    format!("{prefix}-{}", &sha256_hex(&bytes)[..24])
}
