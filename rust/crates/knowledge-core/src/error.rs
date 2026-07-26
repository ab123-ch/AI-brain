use crate::{MemoryTypeId, NamespaceId, ScopeTypeId};

pub type Result<T> = std::result::Result<T, KnowledgeError>;

#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    #[error("invalid knowledge input: {0}")]
    InvalidInput(String),

    #[error("schema conflict for {schema_id}@{version}: {reason}")]
    SchemaConflict {
        schema_id: String,
        version: u32,
        reason: String,
    },

    #[error("incompatible schema upgrade for {schema_id}: {from_version} -> {to_version}")]
    IncompatibleSchemaUpgrade {
        schema_id: String,
        from_version: u32,
        to_version: u32,
    },

    #[error("unknown namespace: {0}")]
    UnknownNamespace(NamespaceId),

    #[error("unknown memory type: {0}")]
    UnknownMemoryType(MemoryTypeId),

    #[error("scope type {scope_type} is not allowed for memory type {memory_type}")]
    ScopeNotAllowed {
        memory_type: MemoryTypeId,
        scope_type: ScopeTypeId,
    },

    #[error("registration already exists: {0}")]
    DuplicateRegistration(String),

    #[error("content resolver is not registered: {0}")]
    UnknownResolver(String),

    #[error("projection adapter is not registered for {namespace}/{source_type}")]
    UnknownProjectionAdapter {
        namespace: String,
        source_type: String,
    },

    #[error("knowledge access denied: {0}")]
    Unauthorized(String),

    #[error("content hash mismatch: expected={expected}, actual={actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("stale revision for {record_id}: expected={expected}, actual={actual}")]
    StaleRevision {
        record_id: String,
        expected: u64,
        actual: u64,
    },

    #[error("knowledge source unavailable: {0}")]
    Unavailable(String),

    #[error("context budget exceeded: {0}")]
    BudgetExceeded(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
