use std::sync::Arc;

use brain_graph::generic::GenericGraphStore;
use brain_memory::generic::GenericMemoryStore;
use knowledge_core::{
    GraphAccess, GraphQueryPort, KnowledgeSchemaRegistry, MemoryQuery, MemoryQueryPort,
    NamespaceId, ScopeRef, ScopeTypeId, TenantId,
};
use novel_application::{LegacyNovelImporter, NovelDomainStore, NovelProjectionWorker};
use novel_domain::{
    CanonStatus, NovelFact, NovelFactKind, NovelLifecycleActor, NovelProject, NovelTaskEvent,
    NovelTaskRequest, NovelTaskState, NovelTaskType, PublicationPolicy,
};
use novel_knowledge_adapter::{novel_schema_bundle, NOVEL_NAMESPACE};

fn legacy_fixture(root: &std::path::Path) -> (NovelProject, novel_domain::NovelTaskCheckpoint) {
    let mut project = NovelProject::new("project-1", "Project One");
    project.canon_revision = 7;
    project.facts.push(NovelFact {
        fact_id: "character-lin".into(),
        kind: NovelFactKind::Character,
        subject_key: "character:lin".into(),
        title: "Lin".into(),
        summary: "Lin guards the old harbor".into(),
        data: serde_json::json!({"role": "guardian"}),
        status: CanonStatus::Confirmed,
        branch_id: "main".into(),
        valid_from_chapter: Some(1),
        valid_to_chapter: None,
        source_refs: vec!["chapters/0001.md".into()],
        confidence: 0.95,
        revision: 7,
        created_at: 10,
        updated_at: 20,
    });

    let project_dir = root.join("novel/projects");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("project-1.json"),
        serde_json::to_vec_pretty(&project).unwrap(),
    )
    .unwrap();

    let request = NovelTaskRequest {
        task_id: "task-1".into(),
        project_id: "project-1".into(),
        task_type: NovelTaskType::Body,
        task_brief: "Write chapter one".into(),
        target_chapter: Some(1),
        expected_revision: 7,
        output_path: "chapters/0001.md".into(),
        context_refs: Vec::new(),
        must_happen: Vec::new(),
        must_not_change: Vec::new(),
        acceptance_criteria: vec!["complete chapter".into()],
        allow_web_research: false,
        publication_policy: PublicationPolicy::RequireUserAcceptance,
        parent_task_id: None,
        source_conversation_id: None,
        source_generation_id: None,
    };
    let mut state = NovelTaskState::new(request).unwrap();
    state.begin_drafting().unwrap();
    let checkpoint = state.checkpoint().unwrap();
    let lifecycle = root.join("novel/lifecycle");
    std::fs::create_dir_all(lifecycle.join("checkpoints")).unwrap();
    std::fs::create_dir_all(lifecycle.join("events")).unwrap();
    std::fs::write(
        lifecycle.join("checkpoints/task-1.json"),
        serde_json::to_vec_pretty(&checkpoint).unwrap(),
    )
    .unwrap();
    let event = NovelTaskEvent {
        event_id: "event-1".into(),
        task_id: "task-1".into(),
        project_id: "project-1".into(),
        actor: NovelLifecycleActor::Main,
        phase: checkpoint.phase,
        summary: "started".into(),
        details: serde_json::json!({"legacy": true}),
        created_at: 30,
    };
    std::fs::write(
        lifecycle.join("events/task-1.jsonl"),
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();
    (project, checkpoint)
}

#[test]
fn legacy_import_preserves_identity_and_is_restart_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    let (project, checkpoint) = legacy_fixture(&legacy);
    let store = NovelDomainStore::open(dir.path().join("novel.db")).unwrap();
    let importer = LegacyNovelImporter::new(&legacy);

    let first = importer.import_into(&store).unwrap();
    assert_eq!(first.projects_imported, 1);
    assert_eq!(first.checkpoints_imported, 1);
    assert_eq!(first.events_imported, 1);
    assert_eq!(store.load_project("project-1").unwrap(), project);
    assert_eq!(store.load_checkpoint("task-1").unwrap(), Some(checkpoint));
    assert_eq!(store.load_task_events("task-1").unwrap().len(), 1);
    assert_eq!(store.pending_outbox(10).unwrap().len(), 1);

    drop(store);
    let reopened = NovelDomainStore::open(dir.path().join("novel.db")).unwrap();
    let second = importer.import_into(&reopened).unwrap();
    assert_eq!(second.projects_imported, 0);
    assert_eq!(second.checkpoints_imported, 0);
    assert_eq!(second.events_imported, 0);
    assert_eq!(reopened.load_project("project-1").unwrap(), project);
    assert_eq!(reopened.load_task_events("task-1").unwrap().len(), 1);
    assert_eq!(reopened.pending_outbox(10).unwrap().len(), 1);
}

#[test]
fn imported_canon_rebuilds_long_term_memory_and_evidence_linked_graph() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    legacy_fixture(&legacy);
    let store = Arc::new(NovelDomainStore::open(dir.path().join("novel.db")).unwrap());
    LegacyNovelImporter::new(&legacy)
        .import_into(&store)
        .unwrap();

    let schemas = Arc::new(KnowledgeSchemaRegistry::new());
    schemas.register(novel_schema_bundle()).unwrap();
    let memory = Arc::new(
        GenericMemoryStore::open(dir.path().join("memory.db"), Arc::clone(&schemas)).unwrap(),
    );
    let graph = Arc::new(
        GenericGraphStore::open(dir.path().join("graph.db"), Arc::clone(&schemas)).unwrap(),
    );
    let worker = NovelProjectionWorker::new(Arc::clone(&store), memory.clone(), graph.clone());
    let first = worker.drain(32).unwrap();
    assert_eq!(first.completed_events, 1);
    assert_eq!(store.pending_outbox(10).unwrap().len(), 0);
    let retry = worker.drain(32).unwrap();
    assert_eq!(retry.completed_events, 0);

    let scope = ScopeRef::new(
        TenantId::from("local"),
        NamespaceId::from(NOVEL_NAMESPACE),
        ScopeTypeId::from("novel_project"),
        "project-1",
    )
    .unwrap();
    let memories = memory
        .query(&MemoryQuery::new(
            TenantId::from("local"),
            vec![scope.clone()],
            vec!["Lin".into()],
            10,
        ))
        .unwrap();
    assert_eq!(memories.entries.len(), 1);
    assert_eq!(memories.entries[0].summary, "Lin guards the old harbor");

    let graph_result = graph
        .query(&knowledge_core::GraphQueryRequest::recall(
            GraphAccess::new(
                TenantId::from("local"),
                NamespaceId::from(NOVEL_NAMESPACE),
                vec![scope],
            ),
            vec!["Lin".into()],
            10,
        ))
        .unwrap();
    assert!(graph_result
        .nodes
        .iter()
        .any(|node| node.properties.get("name") == Some(&serde_json::json!("Lin"))));
    assert!(!graph_result.evidence_links.is_empty());
}
