use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use crate::{
    AccessContext, ContentProviderId, ContentRef, GraphMutationBatch, KnowledgeError,
    KnowledgeSchemaBundle, MemoryTypeId, MemoryTypeSchema, NamespaceId, NodeTypeId, NodeTypeSchema,
    ProjectionAdapterId, RelationTypeId, RelationTypeSchema, ResolvedContent, ResourceTypeId,
    Result,
};

#[derive(Default)]
struct SchemaState {
    bundles: BTreeMap<String, BTreeMap<u32, KnowledgeSchemaBundle>>,
    memory_types: BTreeMap<MemoryTypeId, MemoryTypeSchema>,
    node_types: BTreeMap<NodeTypeId, NodeTypeSchema>,
    relation_types: BTreeMap<RelationTypeId, RelationTypeSchema>,
}

#[derive(Default)]
pub struct KnowledgeSchemaRegistry {
    state: RwLock<SchemaState>,
}

impl KnowledgeSchemaRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, bundle: KnowledgeSchemaBundle) -> Result<KnowledgeSchemaBundle> {
        if bundle.schema_id.trim().is_empty() || bundle.namespace.is_empty() || bundle.version == 0
        {
            return Err(KnowledgeError::InvalidInput(
                "schema id, namespace, and positive version are required".into(),
            ));
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| KnowledgeError::Storage("schema registry lock poisoned".into()))?;
        if let Some(versions) = state.bundles.get(&bundle.schema_id) {
            if let Some(existing) = versions.get(&bundle.version) {
                if existing == &bundle {
                    return Ok(existing.clone());
                }
                return Err(KnowledgeError::SchemaConflict {
                    schema_id: bundle.schema_id.clone(),
                    version: bundle.version,
                    reason: "same schema version has different content".into(),
                });
            }
            let current = *versions.keys().next_back().expect("versions are non-empty");
            if bundle.version != current + 1 {
                return Err(KnowledgeError::IncompatibleSchemaUpgrade {
                    schema_id: bundle.schema_id.clone(),
                    from_version: current,
                    to_version: bundle.version,
                });
            }
            let current_namespace = &versions[&current].namespace;
            if current_namespace != &bundle.namespace {
                return Err(KnowledgeError::SchemaConflict {
                    schema_id: bundle.schema_id.clone(),
                    version: bundle.version,
                    reason: "schema namespace cannot change during upgrade".into(),
                });
            }
        } else if bundle.version != 1 {
            return Err(KnowledgeError::IncompatibleSchemaUpgrade {
                schema_id: bundle.schema_id.clone(),
                from_version: 0,
                to_version: bundle.version,
            });
        }

        validate_type_conflicts(&state, &bundle)?;
        state.memory_types.extend(bundle.memory_types.clone());
        state.node_types.extend(bundle.node_types.clone());
        state.relation_types.extend(bundle.relation_types.clone());
        state
            .bundles
            .entry(bundle.schema_id.clone())
            .or_default()
            .insert(bundle.version, bundle.clone());
        Ok(bundle)
    }

    #[must_use]
    pub fn memory_type(&self, id: &MemoryTypeId) -> Option<MemoryTypeSchema> {
        self.state
            .read()
            .ok()
            .and_then(|state| state.memory_types.get(id).cloned())
    }

    #[must_use]
    pub fn node_type(&self, id: &NodeTypeId) -> Option<NodeTypeSchema> {
        self.state
            .read()
            .ok()
            .and_then(|state| state.node_types.get(id).cloned())
    }

    #[must_use]
    pub fn relation_type(&self, id: &RelationTypeId) -> Option<RelationTypeSchema> {
        self.state
            .read()
            .ok()
            .and_then(|state| state.relation_types.get(id).cloned())
    }

    #[must_use]
    pub fn bundles(&self) -> Vec<KnowledgeSchemaBundle> {
        self.state.read().map_or_else(
            |_| Vec::new(),
            |state| {
                state
                    .bundles
                    .values()
                    .flat_map(|versions| versions.values().cloned())
                    .collect()
            },
        )
    }

    pub fn validate_memory_scope(
        &self,
        memory_type: &MemoryTypeId,
        scope_type: &crate::ScopeTypeId,
    ) -> Result<()> {
        let schema = self
            .memory_type(memory_type)
            .ok_or_else(|| KnowledgeError::UnknownMemoryType(memory_type.clone()))?;
        if schema.allowed_scope_types.contains(scope_type) {
            Ok(())
        } else {
            Err(KnowledgeError::ScopeNotAllowed {
                memory_type: memory_type.clone(),
                scope_type: scope_type.clone(),
            })
        }
    }
}

fn validate_type_conflicts(state: &SchemaState, bundle: &KnowledgeSchemaBundle) -> Result<()> {
    for (id, schema) in &bundle.memory_types {
        if state
            .memory_types
            .get(id)
            .is_some_and(|existing| existing != schema)
        {
            return Err(KnowledgeError::SchemaConflict {
                schema_id: bundle.schema_id.clone(),
                version: bundle.version,
                reason: format!("memory type {id} is already registered differently"),
            });
        }
    }
    for (id, schema) in &bundle.node_types {
        if state
            .node_types
            .get(id)
            .is_some_and(|existing| existing != schema)
        {
            return Err(KnowledgeError::SchemaConflict {
                schema_id: bundle.schema_id.clone(),
                version: bundle.version,
                reason: format!("node type {id} is already registered differently"),
            });
        }
    }
    for (id, schema) in &bundle.relation_types {
        if state
            .relation_types
            .get(id)
            .is_some_and(|existing| existing != schema)
        {
            return Err(KnowledgeError::SchemaConflict {
                schema_id: bundle.schema_id.clone(),
                version: bundle.version,
                reason: format!("relation type {id} is already registered differently"),
            });
        }
    }
    Ok(())
}

pub trait ContentResolver: Send + Sync {
    fn provider_id(&self) -> ContentProviderId;

    fn resolve(&self, content_ref: &ContentRef, access: &AccessContext) -> Result<ResolvedContent>;
}

#[derive(Default)]
pub struct ContentResolverRegistry {
    resolvers: RwLock<HashMap<ContentProviderId, Arc<dyn ContentResolver>>>,
}

impl ContentResolverRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, resolver: Arc<dyn ContentResolver>) -> Result<()> {
        let provider = resolver.provider_id();
        let mut resolvers = self
            .resolvers
            .write()
            .map_err(|_| KnowledgeError::Storage("resolver registry lock poisoned".into()))?;
        if resolvers.contains_key(&provider) {
            return Err(KnowledgeError::DuplicateRegistration(provider.to_string()));
        }
        resolvers.insert(provider, resolver);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, provider: &ContentProviderId) -> Option<Arc<dyn ContentResolver>> {
        self.resolvers
            .read()
            .ok()
            .and_then(|resolvers| resolvers.get(provider).cloned())
    }

    pub fn resolve(
        &self,
        content_ref: &ContentRef,
        access: &AccessContext,
    ) -> Result<ResolvedContent> {
        let resolver = self
            .get(&content_ref.provider)
            .ok_or_else(|| KnowledgeError::UnknownResolver(content_ref.provider.to_string()))?;
        resolver.resolve(content_ref, access)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeSourceEvent {
    pub event_id: String,
    pub sequence: u64,
    pub source_ref: crate::SourceRef,
    pub content_ref: Option<ContentRef>,
    pub visibility_scopes: Vec<crate::ScopeRef>,
    pub provenance: crate::Provenance,
    pub payload: serde_json::Value,
}

pub trait KnowledgeProjectionAdapter: Send + Sync {
    fn adapter_id(&self) -> ProjectionAdapterId;
    fn namespace(&self) -> NamespaceId;
    fn source_type(&self) -> ResourceTypeId;
    fn version(&self) -> u32;
    fn project(&self, event: &KnowledgeSourceEvent) -> Result<GraphMutationBatch>;
}

type AdapterKey = (NamespaceId, ResourceTypeId);

#[derive(Default)]
pub struct ProjectionAdapterRegistry {
    adapters: RwLock<HashMap<AdapterKey, Arc<dyn KnowledgeProjectionAdapter>>>,
}

impl ProjectionAdapterRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, adapter: Arc<dyn KnowledgeProjectionAdapter>) -> Result<()> {
        let key = (adapter.namespace(), adapter.source_type());
        let mut adapters = self
            .adapters
            .write()
            .map_err(|_| KnowledgeError::Storage("adapter registry lock poisoned".into()))?;
        if adapters.contains_key(&key) {
            return Err(KnowledgeError::DuplicateRegistration(format!(
                "{}/{}",
                key.0, key.1
            )));
        }
        adapters.insert(key, adapter);
        Ok(())
    }

    #[must_use]
    pub fn get(
        &self,
        namespace: &NamespaceId,
        source_type: &ResourceTypeId,
    ) -> Option<Arc<dyn KnowledgeProjectionAdapter>> {
        self.adapters.read().ok().and_then(|adapters| {
            adapters
                .get(&(namespace.clone(), source_type.clone()))
                .cloned()
        })
    }
}
