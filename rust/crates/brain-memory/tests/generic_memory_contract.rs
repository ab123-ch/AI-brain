use std::sync::{Arc, Barrier};

use brain_memory::generic::GenericMemoryStore;
use knowledge_core::{
    KnowledgeError, KnowledgeSchemaBundle, KnowledgeSchemaRegistry, MemoryCommandPort,
    MemoryProposal, MemoryQuery, MemoryQueryPort, MemoryTypeId, MemoryTypeSchema, NamespaceId,
    Provenance, ResourceTypeId, RetentionClass, ScopeRef, ScopeTypeId, SourceRef, TenantId,
    TrustLevel,
};

fn fixture() -> (
    tempfile::TempDir,
    Arc<KnowledgeSchemaRegistry>,
    GenericMemoryStore,
) {
    let directory = tempfile::tempdir().unwrap();
    let schemas = Arc::new(KnowledgeSchemaRegistry::new());
    schemas
        .register(
            KnowledgeSchemaBundle::new("support.knowledge", NamespaceId::from("support"), 1)
                .with_memory_type(MemoryTypeSchema::new(
                    MemoryTypeId::from("support.case_note"),
                    [ScopeTypeId::from("support.case")],
                )),
        )
        .unwrap();
    let store =
        GenericMemoryStore::open(directory.path().join("memory.db"), schemas.clone()).unwrap();
    (directory, schemas, store)
}

fn case_scope(case_id: &str) -> ScopeRef {
    ScopeRef::new(
        TenantId::from("tenant-1"),
        NamespaceId::from("support"),
        ScopeTypeId::from("support.case"),
        case_id,
    )
    .unwrap()
}

fn proposal(id: &str, case_id: &str, summary: &str) -> MemoryProposal {
    MemoryProposal::new(
        id,
        NamespaceId::from("support"),
        MemoryTypeId::from("support.case_note"),
        case_scope(case_id),
        summary,
        Provenance::new("test", "support-adapter", "policy-v1"),
        TrustLevel::UserConfirmed,
        RetentionClass::LongTerm,
        format!("idem-{id}"),
    )
    .with_source(SourceRef::new(
        NamespaceId::from("support"),
        ResourceTypeId::from("support.case_event"),
        format!("event-{id}"),
        Some("1".into()),
        Some(format!("hash-{id}")),
    ))
}

#[test]
fn proposals_are_idempotent_scope_authorized_and_emit_one_outbox_event() {
    let (_directory, _schemas, store) = fixture();
    let first = store
        .submit(proposal(
            "proposal-1",
            "case-a",
            "refund condition evidence",
        ))
        .unwrap();
    let repeated = store
        .submit(proposal(
            "proposal-1",
            "case-a",
            "refund condition evidence",
        ))
        .unwrap();
    assert_eq!(first.memory_entry_id, repeated.memory_entry_id);
    assert_eq!(first.version, 1);

    let visible = store
        .query(&MemoryQuery::new(
            TenantId::from("tenant-1"),
            vec![case_scope("case-a")],
            vec!["refund".into()],
            10,
        ))
        .unwrap();
    assert_eq!(visible.entries.len(), 1);
    assert_eq!(visible.entries[0].source_refs.len(), 1);

    let hidden = store
        .query(&MemoryQuery::new(
            TenantId::from("tenant-1"),
            vec![case_scope("case-b")],
            vec!["refund".into()],
            10,
        ))
        .unwrap();
    assert!(hidden.entries.is_empty());
    assert_eq!(store.pending_outbox(10).unwrap().len(), 1);
}

#[test]
fn stale_revision_is_rejected_and_tombstone_is_atomic_with_outbox() {
    let (_directory, _schemas, store) = fixture();
    let entry = store
        .submit(proposal("proposal-2", "case-a", "superseded note"))
        .unwrap();
    let superseded = store
        .supersede(&entry.memory_entry_id, entry.version, "corrected")
        .unwrap();
    assert_eq!(superseded.version, 2);

    assert!(matches!(
        store.supersede(&entry.memory_entry_id, entry.version, "stale retry"),
        Err(KnowledgeError::StaleRevision { .. })
    ));
    assert_eq!(store.pending_outbox(10).unwrap().len(), 2);
    assert!(store
        .query(&MemoryQuery::new(
            TenantId::from("tenant-1"),
            vec![case_scope("case-a")],
            vec!["superseded".into()],
            10,
        ))
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn different_owner_scopes_commit_without_a_process_wide_memory_mutex() {
    let (_directory, _schemas, store) = fixture();
    let store = Arc::new(store);
    let barrier = Arc::new(Barrier::new(9));
    let mut threads = Vec::new();
    for index in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            store.submit(proposal(
                &format!("parallel-{index}"),
                &format!("case-{index}"),
                "parallel note",
            ))
        }));
    }
    barrier.wait();
    for thread in threads {
        thread.join().unwrap().unwrap();
    }
    assert_eq!(store.pending_outbox(20).unwrap().len(), 8);
}
