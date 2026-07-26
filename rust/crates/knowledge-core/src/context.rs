use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    sha256_hex, AccessContext, ContentRef, ContentResolverRegistry, GraphAccess, GraphQueryPort,
    GraphQueryRequest, GraphQueryResult, KnowledgeError, MemoryQuery, MemoryQueryPort,
    MemoryTypeId, NamespaceId, Result, ScopeRef, SourceRef, TenantId, TrustLevel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBlockKind {
    SystemPolicy,
    ConversationUser,
    ConversationAssistant,
    CurrentInput,
    Memory,
    GraphEvidence,
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlockInput {
    pub block_id: String,
    pub kind: ContextBlockKind,
    pub content: String,
    pub source_ref: Option<SourceRef>,
    pub content_ref: Option<ContentRef>,
    pub source_revision: Option<u64>,
    pub source_hash: Option<String>,
    pub trust: Option<TrustLevel>,
}

impl ContextBlockInput {
    #[must_use]
    pub fn new(
        block_id: impl Into<String>,
        kind: ContextBlockKind,
        content: impl Into<String>,
    ) -> Self {
        Self {
            block_id: block_id.into(),
            kind,
            content: content.into(),
            source_ref: None,
            content_ref: None,
            source_revision: None,
            source_hash: None,
            trust: None,
        }
    }

    #[must_use]
    pub fn with_source_metadata(
        mut self,
        source_ref: SourceRef,
        source_revision: Option<u64>,
        source_hash: Option<String>,
    ) -> Self {
        self.source_ref = Some(source_ref);
        self.source_revision = source_revision;
        self.source_hash = source_hash;
        self
    }

    #[must_use]
    pub fn with_content_ref(mut self, content_ref: ContentRef) -> Self {
        self.content_ref = Some(content_ref);
        self
    }

    #[must_use]
    pub const fn with_trust(mut self, trust: TrustLevel) -> Self {
        self.trust = Some(trust);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    pub max_total_tokens: usize,
    pub max_optional_tokens: usize,
    pub max_memory_tokens: usize,
    pub max_graph_tokens: usize,
    pub max_items: usize,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_total_tokens: 4_096,
            max_optional_tokens: 1_024,
            max_memory_tokens: 2_048,
            max_graph_tokens: 1_024,
            max_items: 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlock {
    pub block_id: String,
    pub kind: ContextBlockKind,
    pub content: String,
    pub content_hash: String,
    pub estimated_tokens: usize,
    pub source_ref: Option<SourceRef>,
    pub content_ref: Option<ContentRef>,
    pub source_revision: Option<u64>,
    pub source_hash: Option<String>,
    pub trust: Option<TrustLevel>,
    pub truncated: bool,
}

impl ContextBlock {
    pub fn new(block_id: impl Into<String>, content: impl Into<String>) -> Result<Self> {
        Self::from_input(ContextBlockInput::new(
            block_id,
            ContextBlockKind::Artifact,
            content,
        ))
    }

    pub fn from_input(input: ContextBlockInput) -> Result<Self> {
        crate::require_non_empty("context block id", &input.block_id)?;
        crate::require_non_empty("context block content", &input.content)?;
        let content_hash = sha256_hex(input.content.as_bytes());
        Ok(Self {
            block_id: input.block_id,
            kind: input.kind,
            estimated_tokens: estimate_tokens(&input.content),
            content: input.content,
            content_hash,
            source_ref: input.source_ref,
            content_ref: input.content_ref,
            source_revision: input.source_revision,
            source_hash: input.source_hash,
            trust: input.trust,
            truncated: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDegradation {
    pub source: String,
    pub reason: String,
    pub incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub context_snapshot_id: String,
    pub blocks: Vec<ContextBlock>,
    pub content_hash: String,
    pub budget: ContextBudget,
    pub used_tokens: usize,
    pub truncated: bool,
    pub degradations: Vec<ContextDegradation>,
}

impl ContextSnapshot {
    pub fn new(context_snapshot_id: impl Into<String>, blocks: Vec<ContextBlock>) -> Result<Self> {
        let context_snapshot_id = context_snapshot_id.into();
        crate::require_non_empty("context_snapshot_id", &context_snapshot_id)?;
        if blocks.is_empty() {
            return Err(KnowledgeError::InvalidInput(
                "context snapshot must contain at least one block".into(),
            ));
        }
        let used_tokens: usize = blocks.iter().map(|block| block.estimated_tokens).sum();
        let budget = ContextBudget {
            max_total_tokens: used_tokens.max(1),
            max_optional_tokens: used_tokens,
            max_memory_tokens: used_tokens,
            max_graph_tokens: used_tokens,
            max_items: blocks.len(),
        };
        freeze_snapshot(Some(context_snapshot_id), blocks, budget, false, Vec::new())
    }

    pub fn from_text(
        context_snapshot_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<Self> {
        Self::new(
            context_snapshot_id,
            vec![ContextBlock::new("delegated-input", content)?],
        )
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.blocks
            .iter()
            .map(|block| block.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn validate(&self) -> Result<()> {
        crate::require_non_empty("context_snapshot_id", &self.context_snapshot_id)?;
        if self.blocks.is_empty() {
            return Err(KnowledgeError::InvalidInput(
                "context snapshot must contain at least one block".into(),
            ));
        }
        if self.used_tokens > self.budget.max_total_tokens {
            return Err(KnowledgeError::BudgetExceeded(format!(
                "snapshot uses {} tokens over limit {}",
                self.used_tokens, self.budget.max_total_tokens
            )));
        }
        for block in &self.blocks {
            if sha256_hex(block.content.as_bytes()) != block.content_hash {
                return Err(KnowledgeError::InvalidInput(format!(
                    "context block hash mismatch: {}",
                    block.block_id
                )));
            }
        }
        let canonical = serde_json::to_vec(&(
            &self.blocks,
            &self.budget,
            self.truncated,
            &self.degradations,
        ))?;
        let actual_hash = sha256_hex(&canonical);
        if actual_hash != self.content_hash {
            return Err(KnowledgeError::HashMismatch {
                expected: self.content_hash.clone(),
                actual: actual_hash,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRequest {
    pub tenant_id: TenantId,
    pub authorized_scopes: Vec<ScopeRef>,
    pub terms: Vec<String>,
    pub namespaces: Vec<NamespaceId>,
    pub memory_types: Vec<MemoryTypeId>,
    pub budget: ContextBudget,
    pub required_blocks: Vec<ContextBlockInput>,
    pub optional_blocks: Vec<ContextBlockInput>,
    pub graph_required: bool,
}

impl ContextRequest {
    #[must_use]
    pub fn new(
        tenant_id: TenantId,
        authorized_scopes: Vec<ScopeRef>,
        terms: Vec<String>,
        budget: ContextBudget,
    ) -> Self {
        let mut namespaces = authorized_scopes
            .iter()
            .map(|scope| scope.namespace.clone())
            .collect::<Vec<_>>();
        namespaces.sort();
        namespaces.dedup();
        Self {
            tenant_id,
            authorized_scopes,
            terms,
            namespaces,
            memory_types: Vec::new(),
            budget,
            required_blocks: Vec::new(),
            optional_blocks: Vec::new(),
            graph_required: false,
        }
    }

    #[must_use]
    pub fn with_required_block(mut self, block: ContextBlockInput) -> Self {
        self.required_blocks.push(block);
        self
    }

    /// Adds a lower-priority block. Optional blocks are supplied oldest to
    /// newest; budget pressure retains the newest suffix while preserving its
    /// original order in the frozen snapshot.
    #[must_use]
    pub fn with_optional_block(mut self, block: ContextBlockInput) -> Self {
        self.optional_blocks.push(block);
        self
    }
}

pub struct ContextBuilder {
    memory: Arc<dyn MemoryQueryPort>,
    graph: Arc<dyn GraphQueryPort>,
    resolvers: Arc<ContentResolverRegistry>,
}

impl ContextBuilder {
    #[must_use]
    pub fn new(
        memory: Arc<dyn MemoryQueryPort>,
        graph: Arc<dyn GraphQueryPort>,
        resolvers: Arc<ContentResolverRegistry>,
    ) -> Self {
        Self {
            memory,
            graph,
            resolvers,
        }
    }

    pub fn build(&self, request: &ContextRequest) -> Result<ContextSnapshot> {
        validate_request(request)?;
        let mut assembly = ContextAssembly::from_required(request)?;
        assembly.append_optional(request)?;
        assembly.append_memory(self.memory.as_ref(), request)?;
        assembly.append_graph(self.graph.as_ref(), self.resolvers.as_ref(), request)?;
        freeze_snapshot(
            None,
            assembly.blocks,
            request.budget,
            assembly.truncated,
            assembly.degradations,
        )
    }
}

struct ContextAssembly {
    blocks: Vec<ContextBlock>,
    truncated: bool,
    degradations: Vec<ContextDegradation>,
}

impl ContextAssembly {
    fn from_required(request: &ContextRequest) -> Result<Self> {
        let blocks = request
            .required_blocks
            .iter()
            .cloned()
            .map(ContextBlock::from_input)
            .collect::<Result<Vec<_>>>()?;
        let required_tokens: usize = blocks.iter().map(|block| block.estimated_tokens).sum();
        if required_tokens > request.budget.max_total_tokens
            || blocks.len() > request.budget.max_items
        {
            return Err(KnowledgeError::BudgetExceeded(format!(
                "required context uses {required_tokens} tokens and {} items",
                blocks.len()
            )));
        }
        Ok(Self {
            blocks,
            truncated: false,
            degradations: Vec::new(),
        })
    }

    fn append_optional(&mut self, request: &ContextRequest) -> Result<()> {
        let mut optional_blocks = Vec::new();
        let mut remaining_tokens = request
            .budget
            .max_total_tokens
            .saturating_sub(self.used_tokens())
            .min(request.budget.max_optional_tokens);
        let mut remaining_items = request.budget.max_items.saturating_sub(self.blocks.len());
        for input in request.optional_blocks.iter().rev() {
            if remaining_items == 0 || remaining_tokens == 0 {
                self.truncated = true;
                break;
            }
            let mut block = ContextBlock::from_input(input.clone())?;
            if block.estimated_tokens > remaining_tokens {
                let (content, was_truncated) = truncate_to_tokens(&block.content, remaining_tokens);
                let mut truncated_input = input.clone();
                truncated_input.content = content;
                block = ContextBlock::from_input(truncated_input)?;
                block.truncated = was_truncated;
                self.truncated |= was_truncated;
            }
            remaining_tokens = remaining_tokens.saturating_sub(block.estimated_tokens);
            remaining_items -= 1;
            optional_blocks.push(block);
        }
        if optional_blocks.len() < request.optional_blocks.len() {
            self.truncated = true;
        }
        optional_blocks.reverse();
        self.blocks.extend(optional_blocks);
        Ok(())
    }

    fn append_memory(
        &mut self,
        memory: &dyn MemoryQueryPort,
        request: &ContextRequest,
    ) -> Result<()> {
        let memory_result = memory.query(&MemoryQuery {
            tenant_id: request.tenant_id.clone(),
            authorized_scopes: request.authorized_scopes.clone(),
            terms: request.terms.clone(),
            namespaces: request.namespaces.clone(),
            memory_types: request.memory_types.clone(),
            limit: request.budget.max_items,
        })?;
        self.truncated |= memory_result.truncated;
        let mut used_memory = 0usize;
        for entry in memory_result.entries {
            if self.blocks.len() >= request.budget.max_items {
                self.truncated = true;
                break;
            }
            let remaining_total = self.remaining_total(request);
            let remaining_memory = request.budget.max_memory_tokens.saturating_sub(used_memory);
            let allowed = remaining_total.min(remaining_memory);
            if allowed == 0 {
                self.truncated = true;
                break;
            }
            let (content, was_truncated) = truncate_to_tokens(&entry.summary, allowed);
            let mut block = ContextBlock::from_input(ContextBlockInput::new(
                format!("memory:{}", entry.memory_entry_id),
                ContextBlockKind::Memory,
                content,
            ))?;
            block.content_ref = entry.content_ref;
            block.source_ref = entry.source_refs.first().cloned();
            block.source_revision = Some(entry.version);
            block.source_hash = block
                .source_ref
                .as_ref()
                .and_then(|source| source.content_hash.clone())
                .or_else(|| {
                    block
                        .content_ref
                        .as_ref()
                        .map(|reference| reference.content_hash.clone())
                });
            block.trust = Some(entry.trust);
            block.truncated = was_truncated;
            used_memory += block.estimated_tokens;
            self.truncated |= was_truncated;
            self.blocks.push(block);
        }
        Ok(())
    }

    fn append_graph(
        &mut self,
        graph: &dyn GraphQueryPort,
        resolvers: &ContentResolverRegistry,
        request: &ContextRequest,
    ) -> Result<()> {
        let namespace = request
            .namespaces
            .first()
            .cloned()
            .unwrap_or_else(|| NamespaceId::from("platform.core"));
        let graph_request = GraphQueryRequest::recall(
            GraphAccess::new(
                request.tenant_id.clone(),
                namespace,
                request.authorized_scopes.clone(),
            ),
            request.terms.clone(),
            request.budget.max_items,
        );
        match graph.query(&graph_request) {
            Ok(result) => {
                self.append_graph_result(result, resolvers, request)?;
            }
            Err(error) if !request.graph_required => {
                self.degradations.push(ContextDegradation {
                    source: "graph".into(),
                    reason: error.to_string(),
                    incomplete: true,
                });
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn append_graph_result(
        &mut self,
        result: GraphQueryResult,
        resolvers: &ContentResolverRegistry,
        request: &ContextRequest,
    ) -> Result<()> {
        self.truncated |= result.truncated;
        let access =
            AccessContext::new(request.tenant_id.clone(), request.authorized_scopes.clone());
        let mut used_graph = 0usize;
        for evidence in result.evidence_links {
            let Some(content_ref) = evidence.content_ref.clone() else {
                continue;
            };
            if self.blocks.len() >= request.budget.max_items {
                self.truncated = true;
                break;
            }
            let allowed = self
                .remaining_total(request)
                .min(request.budget.max_graph_tokens.saturating_sub(used_graph));
            if allowed == 0 {
                self.truncated = true;
                break;
            }
            let resolved = match resolvers.resolve(&content_ref, &access) {
                Ok(resolved) => resolved,
                Err(error) => {
                    self.degradations.push(ContextDegradation {
                        source: "graph".into(),
                        reason: error.to_string(),
                        incomplete: true,
                    });
                    continue;
                }
            };
            let (content, was_truncated) = truncate_to_tokens(&resolved.content, allowed);
            let mut block = ContextBlock::from_input(ContextBlockInput::new(
                format!("evidence:{}", evidence.evidence_link_id),
                ContextBlockKind::GraphEvidence,
                content,
            ))?;
            block.source_ref = Some(evidence.source_ref);
            block.content_ref = Some(content_ref);
            block.source_hash = Some(resolved.content_hash);
            block.trust = Some(evidence.trust);
            block.truncated = was_truncated;
            used_graph += block.estimated_tokens;
            self.truncated |= was_truncated;
            self.blocks.push(block);
        }
        if result.is_stale {
            self.degradations.push(ContextDegradation {
                source: "graph".into(),
                reason: format!("projection lag: {}", result.lag),
                incomplete: true,
            });
        }
        Ok(())
    }

    fn used_tokens(&self) -> usize {
        self.blocks.iter().map(|block| block.estimated_tokens).sum()
    }

    fn remaining_total(&self, request: &ContextRequest) -> usize {
        request
            .budget
            .max_total_tokens
            .saturating_sub(self.used_tokens())
    }
}

fn validate_request(request: &ContextRequest) -> Result<()> {
    if request.tenant_id.is_empty() || request.authorized_scopes.is_empty() {
        return Err(KnowledgeError::InvalidInput(
            "context tenant and authorized scopes are required".into(),
        ));
    }
    if request.budget.max_total_tokens == 0 || request.budget.max_items == 0 {
        return Err(KnowledgeError::InvalidInput(
            "context token and item budgets must be positive".into(),
        ));
    }
    Ok(())
}

fn freeze_snapshot(
    requested_id: Option<String>,
    blocks: Vec<ContextBlock>,
    budget: ContextBudget,
    truncated: bool,
    degradations: Vec<ContextDegradation>,
) -> Result<ContextSnapshot> {
    if blocks.is_empty() {
        return Err(KnowledgeError::InvalidInput(
            "context snapshot must contain at least one block".into(),
        ));
    }
    let used_tokens = blocks.iter().map(|block| block.estimated_tokens).sum();
    if used_tokens > budget.max_total_tokens {
        return Err(KnowledgeError::BudgetExceeded(format!(
            "snapshot uses {used_tokens} tokens over limit {}",
            budget.max_total_tokens
        )));
    }
    let canonical = serde_json::to_vec(&(&blocks, &budget, truncated, &degradations))?;
    let content_hash = sha256_hex(&canonical);
    let context_snapshot_id =
        requested_id.unwrap_or_else(|| format!("context-{}", &content_hash[..24]));
    let snapshot = ContextSnapshot {
        context_snapshot_id,
        blocks,
        content_hash,
        budget,
        used_tokens,
        truncated,
        degradations,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

fn estimate_tokens(content: &str) -> usize {
    let chars = content.chars().count();
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4)
    }
}

fn truncate_to_tokens(content: &str, max_tokens: usize) -> (String, bool) {
    if estimate_tokens(content) <= max_tokens {
        return (content.to_owned(), false);
    }
    let max_chars = max_tokens.saturating_mul(4);
    (content.chars().take(max_chars).collect(), true)
}
