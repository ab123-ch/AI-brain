//! Evidence-linked Novel projection adapter for generic Knowledge Core.

use std::collections::{BTreeMap, HashMap};

use knowledge_core::{
    sha256_hex, GraphEdge, GraphEvidenceLink, GraphMutationBatch, GraphNode, GraphSubjectRef,
    KnowledgeError, KnowledgeProjectionAdapter, KnowledgeSchemaBundle, KnowledgeSourceEvent,
    MemoryTypeId, MemoryTypeSchema, NamespaceId, NodeTypeId, NodeTypeSchema, ProjectionAdapterId,
    RelationTypeId, RelationTypeSchema, ResourceTypeId, Result, ScopeTypeId, TrustLevel,
};
use novel_domain::{NovelEntityKind, NovelKnowledgeEvent, NovelKnowledgeStatus, NovelRelationKind};

pub const NOVEL_NAMESPACE: &str = "domain.novel";
pub const NOVEL_SCHEMA_ID: &str = "novel-knowledge";
pub const NOVEL_SCHEMA_VERSION: u32 = 1;

pub fn novel_schema_bundle() -> KnowledgeSchemaBundle {
    let namespace = NamespaceId::from(NOVEL_NAMESPACE);
    let mut bundle = KnowledgeSchemaBundle::new(NOVEL_SCHEMA_ID, namespace, NOVEL_SCHEMA_VERSION)
        .with_memory_type(MemoryTypeSchema::new(
            MemoryTypeId::from("novel.chapter"),
            [
                ScopeTypeId::from("novel_project"),
                ScopeTypeId::from("task"),
            ],
        ));
    for node_type in [
        "novel.project",
        "novel.character",
        "novel.location",
        "novel.item",
        "novel.event",
        "novel.organization",
        "novel.concept",
    ] {
        let mut schema = NodeTypeSchema::new(NodeTypeId::from(node_type));
        schema.indexed_fields = vec!["project_id".into(), "name".into(), "summary".into()];
        schema.display_fields = vec!["name".into(), "summary".into()];
        bundle = bundle.with_node_type(schema);
    }
    let all_entities = entity_node_types();
    for relation in [
        "novel.contains",
        "novel.located_at",
        "novel.owns",
        "novel.participates_in",
        "novel.related_to",
    ] {
        bundle = bundle.with_relation_type(RelationTypeSchema::new(
            RelationTypeId::from(relation),
            all_entities.clone(),
            all_entities.clone(),
        ));
    }
    bundle
}

pub struct NovelKnowledgeAdapter {
    source_type: ResourceTypeId,
}

impl NovelKnowledgeAdapter {
    #[must_use]
    pub fn for_source_type(source_type: ResourceTypeId) -> Self {
        Self { source_type }
    }
}

impl KnowledgeProjectionAdapter for NovelKnowledgeAdapter {
    fn adapter_id(&self) -> ProjectionAdapterId {
        ProjectionAdapterId::from(format!("novel-knowledge-{}", self.source_type.as_str()))
    }

    fn namespace(&self) -> NamespaceId {
        NamespaceId::from(NOVEL_NAMESPACE)
    }

    fn source_type(&self) -> ResourceTypeId {
        self.source_type.clone()
    }

    fn version(&self) -> u32 {
        NOVEL_SCHEMA_VERSION
    }

    fn project(&self, event: &KnowledgeSourceEvent) -> Result<GraphMutationBatch> {
        validate_source(self, event)?;
        let payload: NovelKnowledgeEvent = serde_json::from_value(event.payload.clone())?;
        validate_payload(&payload)?;
        let trust = match payload.status {
            NovelKnowledgeStatus::Committed => TrustLevel::DomainConfirmed,
            NovelKnowledgeStatus::Approved => TrustLevel::UserConfirmed,
            NovelKnowledgeStatus::Draft | NovelKnowledgeStatus::Rejected => {
                return Err(KnowledgeError::InvalidInput(
                    "only approved or committed Novel events may be projected".into(),
                ));
            }
        };
        let (nodes, edges) = project_nodes_and_edges(&payload, event)?;

        let source_hash = event
            .source_ref
            .content_hash
            .clone()
            .or_else(|| {
                event
                    .content_ref
                    .as_ref()
                    .map(|reference| reference.content_hash.clone())
            })
            .unwrap_or_else(|| sha256_hex(&serde_json::to_vec(&payload).unwrap_or_default()));
        let mut batch = GraphMutationBatch::new(
            format!(
                "novel:{}:{}",
                self.source_type.as_str(),
                event.source_ref.resource_id
            ),
            event.sequence,
            event.event_id.clone(),
            self.adapter_id(),
            self.version(),
            source_hash,
        );
        for node in nodes {
            let evidence = evidence_for(GraphSubjectRef::Node(node.node_id.clone()), event, trust)?;
            batch.nodes.push(node);
            batch.evidence_links.push(evidence);
        }
        for edge in edges {
            let evidence = evidence_for(GraphSubjectRef::Edge(edge.edge_id.clone()), event, trust)?;
            batch.edges.push(edge);
            batch.evidence_links.push(evidence);
        }
        Ok(batch)
    }
}

fn project_nodes_and_edges(
    payload: &NovelKnowledgeEvent,
    event: &KnowledgeSourceEvent,
) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
    let tenant_id = event
        .visibility_scopes
        .first()
        .ok_or_else(|| {
            KnowledgeError::InvalidInput("Novel projection requires visibility scope".into())
        })?
        .tenant_id
        .clone();
    let namespace = NamespaceId::from(NOVEL_NAMESPACE);
    let project_node = GraphNode::new(
        tenant_id.clone(),
        namespace.clone(),
        NodeTypeId::from("novel.project"),
        format!("project:{}", payload.project_id),
        event.visibility_scopes.clone(),
    )?
    .with_property("project_id", serde_json::json!(payload.project_id))
    .with_property("name", serde_json::json!(payload.project_id))
    .with_property("summary", serde_json::json!("Novel project"))
    .with_property("canon_revision", serde_json::json!(payload.canon_revision));

    let mut nodes = vec![project_node.clone()];
    let mut node_by_key = HashMap::new();
    for entity in &payload.entities {
        let node = GraphNode::new(
            tenant_id.clone(),
            namespace.clone(),
            node_type(entity.kind),
            &entity.entity_key,
            event.visibility_scopes.clone(),
        )?
        .with_property("project_id", serde_json::json!(payload.project_id))
        .with_property("name", serde_json::json!(entity.name))
        .with_property("summary", serde_json::json!(entity.summary))
        .with_property("properties", entity.properties.clone())
        .with_property("canon_revision", serde_json::json!(payload.canon_revision));
        node_by_key.insert(entity.entity_key.clone(), node.clone());
        nodes.push(node);
    }

    let mut edges = Vec::new();
    for entity in &payload.entities {
        let target = &node_by_key[&entity.entity_key];
        let mut edge = GraphEdge::new(
            tenant_id.clone(),
            namespace.clone(),
            RelationTypeId::from("novel.contains"),
            project_node.node_id.clone(),
            target.node_id.clone(),
            event.visibility_scopes.clone(),
        )?;
        edge.properties
            .insert("project_id".into(), serde_json::json!(payload.project_id));
        edges.push(edge);
    }
    for relation in &payload.relations {
        let source = &node_by_key[&relation.source_entity_key];
        let target = &node_by_key[&relation.target_entity_key];
        let mut edge = GraphEdge::new(
            tenant_id.clone(),
            namespace.clone(),
            relation_type(relation.kind),
            source.node_id.clone(),
            target.node_id.clone(),
            event.visibility_scopes.clone(),
        )?;
        edge.properties = value_to_properties(&relation.properties);
        edges.push(edge);
    }
    Ok((nodes, edges))
}

fn validate_source(adapter: &NovelKnowledgeAdapter, event: &KnowledgeSourceEvent) -> Result<()> {
    if event.source_ref.namespace.as_str() != NOVEL_NAMESPACE
        || event.source_ref.resource_type != adapter.source_type
    {
        return Err(KnowledgeError::InvalidInput(
            "Novel event namespace or source type does not match its registered adapter".into(),
        ));
    }
    if event.event_id.trim().is_empty() || event.source_ref.resource_id.trim().is_empty() {
        return Err(KnowledgeError::InvalidInput(
            "Novel event and source identity are required".into(),
        ));
    }
    Ok(())
}

fn validate_payload(payload: &NovelKnowledgeEvent) -> Result<()> {
    if !payload.status.is_projectable() {
        return Err(KnowledgeError::InvalidInput(
            "only approved or committed Novel events may be projected".into(),
        ));
    }
    if payload.project_id.trim().is_empty() {
        return Err(KnowledgeError::InvalidInput(
            "Novel knowledge event requires project identity".into(),
        ));
    }
    let mut keys = std::collections::HashSet::new();
    for entity in &payload.entities {
        if entity.entity_key.trim().is_empty()
            || entity.name.trim().is_empty()
            || !keys.insert(entity.entity_key.as_str())
        {
            return Err(KnowledgeError::InvalidInput(
                "Novel entities require unique non-empty keys and names".into(),
            ));
        }
    }
    if payload.relations.iter().any(|relation| {
        !keys.contains(relation.source_entity_key.as_str())
            || !keys.contains(relation.target_entity_key.as_str())
    }) {
        return Err(KnowledgeError::InvalidInput(
            "Novel relation endpoints must exist in the same committed event".into(),
        ));
    }
    Ok(())
}

fn evidence_for(
    subject: GraphSubjectRef,
    event: &KnowledgeSourceEvent,
    trust: TrustLevel,
) -> Result<GraphEvidenceLink> {
    GraphEvidenceLink::new(
        subject,
        event.source_ref.clone(),
        event.content_ref.clone(),
        event.visibility_scopes.clone(),
        event.provenance.clone(),
        trust,
        &NOVEL_SCHEMA_VERSION.to_string(),
    )
}

fn node_type(kind: NovelEntityKind) -> NodeTypeId {
    NodeTypeId::from(match kind {
        NovelEntityKind::Character => "novel.character",
        NovelEntityKind::Location => "novel.location",
        NovelEntityKind::Item => "novel.item",
        NovelEntityKind::Event => "novel.event",
        NovelEntityKind::Organization => "novel.organization",
        NovelEntityKind::Concept => "novel.concept",
    })
}

fn relation_type(kind: NovelRelationKind) -> RelationTypeId {
    RelationTypeId::from(match kind {
        NovelRelationKind::Contains => "novel.contains",
        NovelRelationKind::LocatedAt => "novel.located_at",
        NovelRelationKind::Owns => "novel.owns",
        NovelRelationKind::ParticipatesIn => "novel.participates_in",
        NovelRelationKind::RelatedTo => "novel.related_to",
    })
}

fn entity_node_types() -> Vec<NodeTypeId> {
    [
        "novel.project",
        "novel.character",
        "novel.location",
        "novel.item",
        "novel.event",
        "novel.organization",
        "novel.concept",
    ]
    .into_iter()
    .map(NodeTypeId::from)
    .collect()
}

fn value_to_properties(value: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    value.as_object().map_or_else(BTreeMap::new, |properties| {
        properties
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    })
}
