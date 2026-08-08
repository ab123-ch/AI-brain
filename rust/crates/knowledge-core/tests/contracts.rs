use std::sync::Arc;

use knowledge_core::{
    AccessContext, ContentProviderId, ContentRef, ContentResolver, ContentResolverRegistry,
    ContextBlockInput, ContextBlockKind, ContextBudget, ContextBuilder, ContextRequest,
    GraphMutationBatch, GraphQueryPort, GraphQueryRequest, GraphQueryResult, KnowledgeError,
    KnowledgeProjectionAdapter, KnowledgeSchemaBundle, KnowledgeSchemaRegistry,
    KnowledgeSourceEvent, MemoryEntry, MemoryQuery, MemoryQueryPort, MemoryQueryResult,
    MemoryTypeId, MemoryTypeSchema, NamespaceId, NodeTypeId, NodeTypeSchema, ProjectionAdapterId,
    ProjectionAdapterRegistry, Provenance, RelationTypeId, RelationTypeSchema, ResolvedContent,
    ResourceTypeId, ScopeRef, ScopeTypeId, SourceRef, TenantId, TrustLevel,
};

fn support_bundle() -> KnowledgeSchemaBundle {
    KnowledgeSchemaBundle::new("support.knowledge", NamespaceId::from("support"), 1)
        .with_memory_type(MemoryTypeSchema::new(
            MemoryTypeId::from("support.case_note"),
            [ScopeTypeId::from("support.case")],
        ))
        .with_node_type(NodeTypeSchema::new(NodeTypeId::from("support.case")))
        .with_relation_type(RelationTypeSchema::new(
            RelationTypeId::from("support.references"),
            [NodeTypeId::from("support.case")],
            [NodeTypeId::from("support.case")],
        ))
}

struct StaticResolver;

impl ContentResolver for StaticResolver {
    fn provider_id(&self) -> ContentProviderId {
        ContentProviderId::from("test.content")
    }

    fn resolve(
        &self,
        content_ref: &ContentRef,
        _access: &AccessContext,
    ) -> Result<ResolvedContent, KnowledgeError> {
        ResolvedContent::verified(content_ref.clone(), "resolved support evidence")
    }
}

struct SupportAdapter;

impl KnowledgeProjectionAdapter for SupportAdapter {
    fn adapter_id(&self) -> ProjectionAdapterId {
        ProjectionAdapterId::from("support.adapter")
    }

    fn namespace(&self) -> NamespaceId {
        NamespaceId::from("support")
    }

    fn source_type(&self) -> ResourceTypeId {
        ResourceTypeId::from("support.case_event")
    }

    fn version(&self) -> u32 {
        1
    }

    fn project(&self, event: &KnowledgeSourceEvent) -> Result<GraphMutationBatch, KnowledgeError> {
        Ok(GraphMutationBatch::empty(
            "support-stream",
            event.sequence,
            event.event_id.clone(),
            self.adapter_id(),
            self.version(),
            event.source_ref.content_hash.clone().unwrap_or_default(),
        ))
    }
}

#[test]
fn a_new_domain_registers_schema_resolver_and_projection_without_core_changes() {
    let schemas = KnowledgeSchemaRegistry::new();
    let registered = schemas.register(support_bundle()).unwrap();
    assert_eq!(registered.namespace, NamespaceId::from("support"));
    assert!(schemas
        .memory_type(&MemoryTypeId::from("support.case_note"))
        .is_some());

    let resolvers = ContentResolverRegistry::new();
    resolvers.register(Arc::new(StaticResolver)).unwrap();
    assert!(resolvers
        .get(&ContentProviderId::from("test.content"))
        .is_some());

    let adapters = ProjectionAdapterRegistry::new();
    adapters.register(Arc::new(SupportAdapter)).unwrap();
    assert!(adapters
        .get(
            &NamespaceId::from("support"),
            &ResourceTypeId::from("support.case_event"),
        )
        .is_some());
}

#[test]
fn schema_conflicts_and_incompatible_upgrades_are_rejected() {
    let schemas = KnowledgeSchemaRegistry::new();
    schemas.register(support_bundle()).unwrap();
    schemas.register(support_bundle()).unwrap();

    let conflict = support_bundle()
        .with_node_type(NodeTypeSchema::new(NodeTypeId::from("support.attachment")));
    assert!(matches!(
        schemas.register(conflict),
        Err(KnowledgeError::SchemaConflict { .. })
    ));

    let skipped_version =
        KnowledgeSchemaBundle::new("support.knowledge", NamespaceId::from("support"), 3);
    assert!(matches!(
        schemas.register(skipped_version),
        Err(KnowledgeError::IncompatibleSchemaUpgrade { .. })
    ));
}

#[derive(Clone)]
struct OneMemory {
    entry: MemoryEntry,
}

impl MemoryQueryPort for OneMemory {
    fn query(&self, _query: &MemoryQuery) -> Result<MemoryQueryResult, KnowledgeError> {
        Ok(MemoryQueryResult {
            entries: vec![self.entry.clone()],
            scanned: 1,
            truncated: false,
        })
    }
}

struct UnavailableGraph;

impl GraphQueryPort for UnavailableGraph {
    fn query(&self, _query: &GraphQueryRequest) -> Result<GraphQueryResult, KnowledgeError> {
        Err(KnowledgeError::Unavailable("graph maintenance".into()))
    }
}

struct EmptyMemory;

impl MemoryQueryPort for EmptyMemory {
    fn query(&self, _query: &MemoryQuery) -> Result<MemoryQueryResult, KnowledgeError> {
        Ok(MemoryQueryResult {
            entries: Vec::new(),
            scanned: 0,
            truncated: false,
        })
    }
}

#[test]
fn conversation_reference_has_stable_wire_name() {
    assert_eq!(
        serde_json::to_value(ContextBlockKind::ConversationReference).unwrap(),
        serde_json::json!("conversation_reference")
    );
}

#[test]
fn required_conversation_reference_is_not_silently_truncated() {
    let tenant = TenantId::from("tenant-1");
    let member_scope = ScopeRef::new(
        tenant.clone(),
        NamespaceId::from("platform.core"),
        ScopeTypeId::from("member"),
        "member-a",
    )
    .unwrap();
    let builder = ContextBuilder::new(
        Arc::new(EmptyMemory),
        Arc::new(UnavailableGraph),
        Arc::new(ContentResolverRegistry::new()),
    );
    let request = ContextRequest::new(
        tenant,
        vec![member_scope],
        vec!["reply".into()],
        ContextBudget {
            max_total_tokens: 64,
            max_optional_tokens: 0,
            max_memory_tokens: 0,
            max_graph_tokens: 0,
            max_items: 1,
        },
    )
    .with_required_block(ContextBlockInput::new(
        "policy",
        ContextBlockKind::SystemPolicy,
        "policy",
    ))
    .with_required_block(ContextBlockInput::new(
        "reply-reference:event-1",
        ContextBlockKind::ConversationReference,
        "quoted material",
    ));

    assert!(matches!(
        builder.build(&request),
        Err(KnowledgeError::BudgetExceeded(_))
    ));
}

#[test]
fn frozen_context_is_budgeted_traceable_and_degrades_when_graph_is_unavailable() {
    let tenant = TenantId::from("tenant-1");
    let member_scope = ScopeRef::new(
        tenant.clone(),
        NamespaceId::from("platform.core"),
        ScopeTypeId::from("member"),
        "member-a",
    )
    .unwrap();
    let memory = MemoryEntry::accepted(
        "memory-1",
        NamespaceId::from("platform.core"),
        MemoryTypeId::from("platform.note"),
        member_scope.clone(),
        "A concise remembered preference with a stable revision.",
        Provenance::new("test", "fixture", "policy-v1"),
        TrustLevel::UserConfirmed,
        7,
    )
    .with_source(SourceRef::new(
        NamespaceId::from("platform.core"),
        ResourceTypeId::from("conversation.turn"),
        "event-7",
        Some("7".into()),
        Some("source-hash".into()),
    ));

    let builder = ContextBuilder::new(
        Arc::new(OneMemory { entry: memory }),
        Arc::new(UnavailableGraph),
        Arc::new(ContentResolverRegistry::new()),
    );
    let request = ContextRequest::new(
        tenant,
        vec![member_scope],
        vec!["preference".into()],
        ContextBudget {
            max_total_tokens: 45,
            max_optional_tokens: 12,
            max_memory_tokens: 24,
            max_graph_tokens: 12,
            max_items: 4,
        },
    )
    .with_required_block(ContextBlockInput::new(
        "member-policy",
        ContextBlockKind::SystemPolicy,
        "You are member A.",
    ))
    .with_required_block(ContextBlockInput::new(
        "current-input",
        ContextBlockKind::CurrentInput,
        "What should we remember?",
    ));

    let first = builder.build(&request).unwrap();
    let second = builder.build(&request).unwrap();
    assert_eq!(first.content_hash, second.content_hash);
    assert_eq!(first.context_snapshot_id, second.context_snapshot_id);
    assert!(first.used_tokens <= first.budget.max_total_tokens);
    assert!(first.blocks.iter().any(|block| {
        block.source_revision == Some(7) && block.source_hash.as_deref() == Some("source-hash")
    }));
    assert!(first
        .degradations
        .iter()
        .any(|item| item.source == "graph" && item.incomplete));
}

#[test]
fn optional_context_keeps_newest_history_with_exact_source_metadata() {
    let tenant = TenantId::from("tenant-1");
    let member_scope = ScopeRef::new(
        tenant.clone(),
        NamespaceId::from("platform.core"),
        ScopeTypeId::from("member"),
        "member-a",
    )
    .unwrap();
    let history_source = |event_id: &str, sequence: u64, hash: &str| {
        ContextBlockInput::new(
            format!("room-event:{event_id}"),
            ContextBlockKind::ConversationUser,
            format!("history event {sequence}"),
        )
        .with_source_metadata(
            SourceRef::new(
                NamespaceId::from("platform.core"),
                ResourceTypeId::from("conversation.turn"),
                event_id,
                Some(sequence.to_string()),
                Some(hash.into()),
            ),
            Some(sequence),
            Some(hash.into()),
        )
    };
    let builder = ContextBuilder::new(
        Arc::new(EmptyMemory),
        Arc::new(UnavailableGraph),
        Arc::new(ContentResolverRegistry::new()),
    );
    let request = ContextRequest::new(
        tenant,
        vec![member_scope],
        vec!["latest".into()],
        ContextBudget {
            max_total_tokens: 64,
            max_optional_tokens: 32,
            max_memory_tokens: 16,
            max_graph_tokens: 16,
            max_items: 3,
        },
    )
    .with_required_block(ContextBlockInput::new(
        "member-policy",
        ContextBlockKind::SystemPolicy,
        "member policy",
    ))
    .with_optional_block(history_source("event-old", 10, "old-source-hash"))
    .with_optional_block(history_source("event-new", 11, "new-source-hash"))
    .with_required_block(ContextBlockInput::new(
        "current-input",
        ContextBlockKind::CurrentInput,
        "current input",
    ));

    let snapshot = builder.build(&request).unwrap();
    assert!(snapshot.truncated);
    assert!(snapshot
        .blocks
        .iter()
        .all(|block| block.block_id != "room-event:event-old"));
    let newest = snapshot
        .blocks
        .iter()
        .find(|block| block.block_id == "room-event:event-new")
        .unwrap();
    assert_eq!(newest.source_revision, Some(11));
    assert_eq!(newest.source_hash.as_deref(), Some("new-source-hash"));
    assert_eq!(newest.source_ref.as_ref().unwrap().resource_id, "event-new");
    snapshot.validate().unwrap();
}
