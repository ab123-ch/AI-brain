use std::sync::Arc;

use brain_graph::generic::{GenericGraphStore, ProjectionApplyOutcome};
use knowledge_core::{
    GraphAccess, GraphEdge, GraphEvidenceLink, GraphMutationBatch, GraphNode, GraphQueryPort,
    GraphSubjectRef, KnowledgeSchemaBundle, KnowledgeSchemaRegistry, NamespaceId, NodeTypeId,
    NodeTypeSchema, ProjectionAdapterId, Provenance, RelationTypeId, RelationTypeSchema,
    ResourceTypeId, ScopeRef, ScopeTypeId, SourceRef, TenantId, TrustLevel,
};

fn fixture() -> (
    tempfile::TempDir,
    Arc<KnowledgeSchemaRegistry>,
    GenericGraphStore,
) {
    let directory = tempfile::tempdir().unwrap();
    let schemas = Arc::new(KnowledgeSchemaRegistry::new());
    schemas
        .register(
            KnowledgeSchemaBundle::new("support.knowledge", NamespaceId::from("support"), 1)
                .with_node_type(NodeTypeSchema::new(NodeTypeId::from("support.case")))
                .with_relation_type(RelationTypeSchema::new(
                    RelationTypeId::from("support.references"),
                    [NodeTypeId::from("support.case")],
                    [NodeTypeId::from("support.case")],
                )),
        )
        .unwrap();
    let store =
        GenericGraphStore::open(directory.path().join("graph.db"), schemas.clone()).unwrap();
    (directory, schemas, store)
}

fn scope() -> ScopeRef {
    ScopeRef::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        ScopeTypeId::from("support.case"),
        "case-a",
    )
    .unwrap()
}

fn batch(sequence: u64) -> (GraphMutationBatch, GraphNode, GraphEdge, GraphEvidenceLink) {
    let node_a = GraphNode::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        NodeTypeId::from("support.case"),
        "case-a",
        [scope()],
    )
    .unwrap()
    .with_property("title", serde_json::json!("Refund case"));
    let node_b = GraphNode::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        NodeTypeId::from("support.case"),
        "case-b",
        [scope()],
    )
    .unwrap();
    let edge = GraphEdge::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        RelationTypeId::from("support.references"),
        node_a.node_id.clone(),
        node_b.node_id.clone(),
        [scope()],
    )
    .unwrap();
    let source = SourceRef::new(
        NamespaceId::from("support"),
        ResourceTypeId::from("support.case_event"),
        "event-1",
        Some("1".into()),
        Some("source-hash".into()),
    );
    let evidence = GraphEvidenceLink::new(
        GraphSubjectRef::Edge(edge.edge_id.clone()),
        source,
        None,
        [scope()],
        Provenance::new("test", "support-adapter", "policy-v1"),
        TrustLevel::DomainConfirmed,
        "support-adapter@1",
    )
    .unwrap();
    let batch = GraphMutationBatch::new(
        "support-stream",
        sequence,
        format!("event-{sequence}"),
        ProjectionAdapterId::from("support-adapter"),
        1,
        "source-hash",
    )
    .with_node(node_a.clone())
    .with_node(node_b)
    .with_edge(edge.clone())
    .with_evidence(evidence.clone());
    (batch, node_a, edge, evidence)
}

fn access() -> GraphAccess {
    GraphAccess::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        vec![scope()],
    )
}

#[test]
fn projection_is_atomic_idempotent_and_checkpointed_with_the_batch() {
    let (_directory, _schemas, store) = fixture();
    let (valid, _node, _edge, _evidence) = batch(1);
    let invalid = valid.clone().with_edge(
        GraphEdge::new(
            TenantId::from("tenant-1"),
            NamespaceId::from("support"),
            RelationTypeId::from("support.references"),
            valid.nodes[0].node_id.clone(),
            "missing-node",
            [scope()],
        )
        .unwrap(),
    );
    assert!(store.apply_batch(invalid).is_err());
    assert!(store.checkpoint("support-stream").unwrap().is_none());
    assert!(store
        .subjects_for(&valid.evidence_links[0].source_ref, &access(), 10)
        .unwrap()
        .evidence_links
        .is_empty());

    assert_eq!(
        store.apply_batch(valid.clone()).unwrap(),
        ProjectionApplyOutcome::Applied
    );
    assert_eq!(
        store.apply_batch(valid).unwrap(),
        ProjectionApplyOutcome::AlreadyApplied
    );
    assert_eq!(
        store
            .checkpoint("support-stream")
            .unwrap()
            .unwrap()
            .sequence,
        1
    );
}

#[test]
fn evidence_is_queryable_in_both_directions_without_read_side_writes() {
    let (directory, _schemas, store) = fixture();
    let (batch, _node, edge, evidence) = batch(1);
    store.apply_batch(batch).unwrap();

    let observer = rusqlite::Connection::open(directory.path().join("graph.db")).unwrap();
    let before: i64 = observer
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();
    let sources = store
        .sources_for(&GraphSubjectRef::Edge(edge.edge_id), &access(), 10)
        .unwrap();
    let subjects = store
        .subjects_for(&evidence.source_ref, &access(), 10)
        .unwrap();
    let after: i64 = observer
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();

    assert_eq!(sources.evidence_links.len(), 1);
    assert_eq!(subjects.evidence_links.len(), 1);
    assert_eq!(sources.projection_seq, 1);
    assert_eq!(before, after, "graph queries must be side-effect free");
}

#[test]
fn evidence_scope_is_enforced_during_query() {
    let (_directory, _schemas, store) = fixture();
    let (batch, _node, edge, _evidence) = batch(1);
    store.apply_batch(batch).unwrap();
    let unauthorized_scope = ScopeRef::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        ScopeTypeId::from("support.case"),
        "case-secret",
    )
    .unwrap();
    let unauthorized = GraphAccess::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        vec![unauthorized_scope],
    );
    assert!(store
        .sources_for(&GraphSubjectRef::Edge(edge.edge_id), &unauthorized, 10)
        .unwrap()
        .evidence_links
        .is_empty());
}
