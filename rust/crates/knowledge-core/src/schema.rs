use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{MemoryTypeId, NamespaceId, NodeTypeId, RelationTypeId, ScopeTypeId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTypeSchema {
    pub memory_type: MemoryTypeId,
    pub allowed_scope_types: Vec<ScopeTypeId>,
    pub metadata_schema: Value,
}

impl MemoryTypeSchema {
    #[must_use]
    pub fn new(
        memory_type: MemoryTypeId,
        allowed_scope_types: impl IntoIterator<Item = ScopeTypeId>,
    ) -> Self {
        Self {
            memory_type,
            allowed_scope_types: allowed_scope_types.into_iter().collect(),
            metadata_schema: serde_json::json!({"type": "object"}),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTypeSchema {
    pub node_type: NodeTypeId,
    pub properties_schema: Value,
    pub indexed_fields: Vec<String>,
    pub display_fields: Vec<String>,
}

impl NodeTypeSchema {
    #[must_use]
    pub fn new(node_type: NodeTypeId) -> Self {
        Self {
            node_type,
            properties_schema: serde_json::json!({"type": "object"}),
            indexed_fields: Vec::new(),
            display_fields: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationTypeSchema {
    pub relation_type: RelationTypeId,
    pub source_types: Vec<NodeTypeId>,
    pub target_types: Vec<NodeTypeId>,
    pub properties_schema: Value,
    pub allow_cross_namespace: bool,
}

impl RelationTypeSchema {
    #[must_use]
    pub fn new(
        relation_type: RelationTypeId,
        source_types: impl IntoIterator<Item = NodeTypeId>,
        target_types: impl IntoIterator<Item = NodeTypeId>,
    ) -> Self {
        Self {
            relation_type,
            source_types: source_types.into_iter().collect(),
            target_types: target_types.into_iter().collect(),
            properties_schema: serde_json::json!({"type": "object"}),
            allow_cross_namespace: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeSchemaBundle {
    pub schema_id: String,
    pub namespace: NamespaceId,
    pub version: u32,
    pub memory_types: BTreeMap<MemoryTypeId, MemoryTypeSchema>,
    pub node_types: BTreeMap<NodeTypeId, NodeTypeSchema>,
    pub relation_types: BTreeMap<RelationTypeId, RelationTypeSchema>,
}

impl KnowledgeSchemaBundle {
    #[must_use]
    pub fn new(schema_id: impl Into<String>, namespace: NamespaceId, version: u32) -> Self {
        Self {
            schema_id: schema_id.into(),
            namespace,
            version,
            memory_types: BTreeMap::new(),
            node_types: BTreeMap::new(),
            relation_types: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_memory_type(mut self, schema: MemoryTypeSchema) -> Self {
        self.memory_types.insert(schema.memory_type.clone(), schema);
        self
    }

    #[must_use]
    pub fn with_node_type(mut self, schema: NodeTypeSchema) -> Self {
        self.node_types.insert(schema.node_type.clone(), schema);
        self
    }

    #[must_use]
    pub fn with_relation_type(mut self, schema: RelationTypeSchema) -> Self {
        self.relation_types
            .insert(schema.relation_type.clone(), schema);
        self
    }
}
