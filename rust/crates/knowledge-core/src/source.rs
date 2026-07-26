use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ContentProviderId, KnowledgeError, NamespaceId, ResourceTypeId, Result, ScopeTypeId, TenantId,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScopeRef {
    pub tenant_id: TenantId,
    pub namespace: NamespaceId,
    pub scope_type: ScopeTypeId,
    pub scope_key: String,
}

impl ScopeRef {
    pub fn new(
        tenant_id: TenantId,
        namespace: NamespaceId,
        scope_type: ScopeTypeId,
        scope_key: impl Into<String>,
    ) -> Result<Self> {
        let scope_key = scope_key.into();
        if tenant_id.is_empty()
            || namespace.is_empty()
            || scope_type.is_empty()
            || scope_key.trim().is_empty()
        {
            return Err(KnowledgeError::InvalidInput(
                "scope tenant, namespace, type, and key must be non-empty".into(),
            ));
        }
        Ok(Self {
            tenant_id,
            namespace,
            scope_type,
            scope_key,
        })
    }

    #[must_use]
    pub fn stable_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.tenant_id, self.namespace, self.scope_type, self.scope_key
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentRef {
    pub provider: ContentProviderId,
    pub resource_id: String,
    pub version: Option<String>,
    pub content_hash: String,
}

impl ContentRef {
    pub fn new(
        provider: ContentProviderId,
        resource_id: impl Into<String>,
        version: Option<String>,
        content_hash: impl Into<String>,
    ) -> Result<Self> {
        let resource_id = resource_id.into();
        let content_hash = content_hash.into();
        if provider.is_empty() || resource_id.trim().is_empty() || content_hash.trim().is_empty() {
            return Err(KnowledgeError::InvalidInput(
                "content provider, resource id, and hash must be non-empty".into(),
            ));
        }
        Ok(Self {
            provider,
            resource_id,
            version,
            content_hash,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub namespace: NamespaceId,
    pub resource_type: ResourceTypeId,
    pub resource_id: String,
    pub version: Option<String>,
    pub content_hash: Option<String>,
}

impl SourceRef {
    #[must_use]
    pub fn new(
        namespace: NamespaceId,
        resource_type: ResourceTypeId,
        resource_id: impl Into<String>,
        version: Option<String>,
        content_hash: Option<String>,
    ) -> Self {
        Self {
            namespace,
            resource_type,
            resource_id: resource_id.into(),
            version,
            content_hash,
        }
    }

    #[must_use]
    pub fn stable_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.namespace,
            self.resource_type,
            self.resource_id,
            self.version.as_deref().unwrap_or_default(),
            self.content_hash.as_deref().unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub producer_kind: String,
    pub producer_id: String,
    pub policy_version: String,
}

impl Provenance {
    #[must_use]
    pub fn new(
        producer_kind: impl Into<String>,
        producer_id: impl Into<String>,
        policy_version: impl Into<String>,
    ) -> Self {
        Self {
            producer_kind: producer_kind.into(),
            producer_id: producer_id.into(),
            policy_version: policy_version.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Untrusted,
    ModelGenerated,
    UserConfirmed,
    DomainConfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessContext {
    pub tenant_id: TenantId,
    pub authorized_scopes: Vec<ScopeRef>,
}

impl AccessContext {
    #[must_use]
    pub fn new(tenant_id: TenantId, authorized_scopes: Vec<ScopeRef>) -> Self {
        Self {
            tenant_id,
            authorized_scopes: dedupe_scopes(authorized_scopes),
        }
    }

    #[must_use]
    pub fn authorizes_any(&self, required: &[ScopeRef]) -> bool {
        required.iter().any(|scope| {
            scope.tenant_id == self.tenant_id && self.authorized_scopes.contains(scope)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedContent {
    pub content_ref: ContentRef,
    pub content: String,
    pub content_hash: String,
}

impl ResolvedContent {
    pub fn verified(content_ref: ContentRef, content: impl Into<String>) -> Result<Self> {
        let content = content.into();
        let actual = sha256_hex(content.as_bytes());
        if actual != content_ref.content_hash {
            return Err(KnowledgeError::HashMismatch {
                expected: content_ref.content_hash.clone(),
                actual,
            });
        }
        Ok(Self {
            content_hash: content_ref.content_hash.clone(),
            content_ref,
            content,
        })
    }
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn require_non_empty(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(KnowledgeError::InvalidInput(format!(
            "{label} must be non-empty"
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_scope_set(tenant_id: &TenantId, scopes: &[ScopeRef]) -> Result<()> {
    if scopes.is_empty() {
        return Err(KnowledgeError::InvalidInput(
            "at least one visibility scope is required".into(),
        ));
    }
    if scopes.iter().any(|scope| &scope.tenant_id != tenant_id) {
        return Err(KnowledgeError::InvalidInput(
            "visibility scopes must use the record tenant".into(),
        ));
    }
    Ok(())
}

pub(crate) fn dedupe_scopes(scopes: Vec<ScopeRef>) -> Vec<ScopeRef> {
    scopes
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
