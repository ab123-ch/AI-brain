use std::sync::Arc;

use brain_graph::generic::{GenericGraphStore, ProjectionApplyOutcome};
use knowledge_core::{
    ContentProviderId, ContentRef, GraphAccess, GraphQueryPort, GraphSubjectRef,
    KnowledgeProjectionAdapter, KnowledgeSchemaRegistry, KnowledgeSourceEvent, NamespaceId,
    Provenance, ResourceTypeId, ScopeRef, ScopeTypeId, SourceRef, TenantId, TrustLevel,
};
use novel_domain::{
    NovelEntityKind, NovelKnowledgeEntity, NovelKnowledgeEvent, NovelKnowledgeRelation,
    NovelKnowledgeStatus, NovelRelationKind,
};
use novel_knowledge_adapter::{novel_schema_bundle, NovelKnowledgeAdapter};

fn scope() -> ScopeRef {
    ScopeRef::new(
        TenantId::from("local"),
        NamespaceId::from("domain.novel"),
        ScopeTypeId::from("novel_project"),
        "project-1",
    )
    .unwrap()
}

fn source_event(status: NovelKnowledgeStatus) -> KnowledgeSourceEvent {
    let payload = NovelKnowledgeEvent {
        status,
        project_id: "project-1".into(),
        canon_revision: 3,
        entities: vec![
            NovelKnowledgeEntity {
                entity_key: "character:aria".into(),
                kind: NovelEntityKind::Character,
                name: "Aria".into(),
                summary: "The protagonist".into(),
                properties: serde_json::json!({"role": "protagonist"}),
            },
            NovelKnowledgeEntity {
                entity_key: "location:old-gate".into(),
                kind: NovelEntityKind::Location,
                name: "Old Gate".into(),
                summary: "A sealed city gate".into(),
                properties: serde_json::json!({}),
            },
        ],
        relations: vec![NovelKnowledgeRelation {
            source_entity_key: "character:aria".into(),
            target_entity_key: "location:old-gate".into(),
            kind: NovelRelationKind::LocatedAt,
            properties: serde_json::json!({"chapter": 1}),
        }],
    };
    KnowledgeSourceEvent {
        event_id: "novel-commit-3".into(),
        sequence: 3,
        source_ref: SourceRef::new(
            NamespaceId::from("domain.novel"),
            ResourceTypeId::from("novel.canon"),
            "project-1",
            Some("3".into()),
            Some("canon-hash-3".into()),
        ),
        content_ref: Some(
            ContentRef::new(
                ContentProviderId::from("artifact-store"),
                "artifact-writer-1",
                Some("1".into()),
                "artifact-hash-1",
            )
            .unwrap(),
        ),
        visibility_scopes: vec![scope()],
        provenance: Provenance::new("novel-domain", "publication-1", "novel-domain-v1"),
        payload: serde_json::to_value(payload).unwrap(),
    }
}

fn graph_fixture() -> (tempfile::TempDir, GenericGraphStore) {
    let directory = tempfile::tempdir().unwrap();
    let schemas = Arc::new(KnowledgeSchemaRegistry::new());
    schemas.register(novel_schema_bundle()).unwrap();
    let store = GenericGraphStore::open(directory.path().join("graph.db"), schemas).unwrap();
    (directory, store)
}

fn access() -> GraphAccess {
    GraphAccess::new(
        TenantId::from("local"),
        NamespaceId::from("domain.novel"),
        vec![scope()],
    )
}

#[test]
fn schema_and_adapter_are_registered_outside_generic_core() {
    let registry = KnowledgeSchemaRegistry::new();
    let bundle = registry.register(novel_schema_bundle()).unwrap();
    assert_eq!(bundle.namespace.as_str(), "domain.novel");
    assert!(bundle
        .node_types
        .keys()
        .any(|node_type| node_type.as_str() == "novel.character"));
    assert!(bundle
        .relation_types
        .keys()
        .any(|kind| kind.as_str() == "novel.located_at"));
}

#[test]
fn committed_event_projects_nodes_edges_and_evidence_for_every_subject() {
    let adapter = NovelKnowledgeAdapter::for_source_type(ResourceTypeId::from("novel.canon"));
    let batch = adapter
        .project(&source_event(NovelKnowledgeStatus::Committed))
        .unwrap();
    assert_eq!(batch.nodes.len(), 3);
    assert_eq!(batch.edges.len(), 3);
    assert_eq!(
        batch.evidence_links.len(),
        batch.nodes.len() + batch.edges.len()
    );
    assert!(batch
        .evidence_links
        .iter()
        .all(|link| link.source_ref.resource_id == "project-1"));
    assert!(batch
        .evidence_links
        .iter()
        .all(|link| link.content_ref.is_some()));
}

#[test]
fn draft_and_rejected_events_cannot_enter_generic_graph() {
    let adapter = NovelKnowledgeAdapter::for_source_type(ResourceTypeId::from("novel.canon"));
    assert!(adapter
        .project(&source_event(NovelKnowledgeStatus::Draft))
        .is_err());
    assert!(adapter
        .project(&source_event(NovelKnowledgeStatus::Rejected))
        .is_err());
}

#[test]
fn committed_projection_roundtrips_through_generic_graph_in_both_directions() {
    let (_directory, store) = graph_fixture();
    let adapter = NovelKnowledgeAdapter::for_source_type(ResourceTypeId::from("novel.canon"));
    let mut event = source_event(NovelKnowledgeStatus::Committed);
    event.sequence = 1;
    let source_ref = event.source_ref.clone();
    let batch = adapter.project(&event).unwrap();
    let character = batch
        .nodes
        .iter()
        .find(|node| node.entity_key == "character:aria")
        .unwrap()
        .clone();
    let expected_evidence = batch.evidence_links.len();

    assert_eq!(
        store.apply_batch(batch).unwrap(),
        ProjectionApplyOutcome::Applied
    );
    let sources = store
        .sources_for(
            &GraphSubjectRef::Node(character.node_id.clone()),
            &access(),
            10,
        )
        .unwrap();
    let subjects = store.subjects_for(&source_ref, &access(), 20).unwrap();

    assert_eq!(sources.evidence_links.len(), 1);
    assert_eq!(sources.evidence_links[0].source_ref, source_ref);
    assert_eq!(sources.evidence_links[0].trust, TrustLevel::DomainConfirmed);
    assert!(sources.evidence_links[0].content_ref.is_some());
    assert_eq!(subjects.evidence_links.len(), expected_evidence);
    assert!(subjects
        .evidence_links
        .iter()
        .any(|link| link.subject == GraphSubjectRef::Node(character.node_id.clone())));
    assert_eq!(subjects.projection_seq, 1);
}

#[test]
fn rejected_projection_attempts_leave_generic_graph_without_a_checkpoint_or_evidence() {
    let (_directory, store) = graph_fixture();
    let adapter = NovelKnowledgeAdapter::for_source_type(ResourceTypeId::from("novel.canon"));
    for status in [NovelKnowledgeStatus::Draft, NovelKnowledgeStatus::Rejected] {
        assert!(adapter.project(&source_event(status)).is_err());
    }

    let event = source_event(NovelKnowledgeStatus::Rejected);
    assert!(store
        .checkpoint("novel:novel.canon:project-1")
        .unwrap()
        .is_none());
    assert!(store
        .subjects_for(&event.source_ref, &access(), 20)
        .unwrap()
        .evidence_links
        .is_empty());
}
