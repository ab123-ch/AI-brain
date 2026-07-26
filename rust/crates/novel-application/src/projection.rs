use std::sync::Arc;

use brain_graph::generic::GenericGraphStore;
use brain_memory::generic::GenericMemoryStore;
use knowledge_core::{
    ContentProviderId, ContentRef, KnowledgeProjectionAdapter, KnowledgeSourceEvent,
    MemoryCommandPort, MemoryProposal, MemoryTypeId, NamespaceId, Provenance, ResourceTypeId,
    RetentionClass, ScopeRef, ScopeTypeId, SourceRef, TenantId, TrustLevel,
};
use novel_domain::{
    CanonStatus, NovelEntityKind, NovelFactKind, NovelKnowledgeEntity, NovelKnowledgeEvent,
    NovelKnowledgeStatus, NovelProject,
};
use novel_knowledge_adapter::{NovelKnowledgeAdapter, NOVEL_NAMESPACE};
use serde::{Deserialize, Serialize};

use crate::{NovelDomainStore, Result};

const CANON_SOURCE_TYPE: &str = "novel.canon";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelProjectionEnvelope {
    pub source_event: KnowledgeSourceEvent,
    pub memory_proposals: Vec<MemoryProposal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NovelProjectionReport {
    pub completed_events: usize,
    pub memory_entries: usize,
    pub graph_batches: usize,
}

pub struct NovelProjectionWorker {
    store: Arc<NovelDomainStore>,
    memory: Arc<GenericMemoryStore>,
    graph: Arc<GenericGraphStore>,
}

impl NovelProjectionWorker {
    #[must_use]
    pub fn new(
        store: Arc<NovelDomainStore>,
        memory: Arc<GenericMemoryStore>,
        graph: Arc<GenericGraphStore>,
    ) -> Self {
        Self {
            store,
            memory,
            graph,
        }
    }

    pub fn drain(&self, limit: usize) -> Result<NovelProjectionReport> {
        let mut report = NovelProjectionReport::default();
        for record in self.store.pending_outbox(limit)? {
            if !record.memory_published {
                for proposal in &record.envelope.memory_proposals {
                    self.memory.submit(proposal.clone())?;
                    report.memory_entries += 1;
                }
                self.store.mark_memory_published(&record.event_id)?;
            }
            if !record.graph_published {
                let source_type = record
                    .envelope
                    .source_event
                    .source_ref
                    .resource_type
                    .clone();
                let adapter = NovelKnowledgeAdapter::for_source_type(source_type);
                let batch = adapter.project(&record.envelope.source_event)?;
                self.graph.apply_batch(batch)?;
                self.store.mark_graph_published(&record.event_id)?;
                report.graph_batches += 1;
            }
            report.completed_events += 1;
        }
        Ok(report)
    }
}

pub(crate) fn projection_envelope(
    project: &NovelProject,
    sequence: u64,
    source_hash: &str,
) -> Result<NovelProjectionEnvelope> {
    let namespace = NamespaceId::from(NOVEL_NAMESPACE);
    let source_type = ResourceTypeId::from(CANON_SOURCE_TYPE);
    let tenant_id = TenantId::from("local");
    let scope = ScopeRef::new(
        tenant_id,
        namespace.clone(),
        ScopeTypeId::from("novel_project"),
        &project.project_id,
    )?;
    let source_ref = SourceRef::new(
        namespace.clone(),
        source_type,
        project.project_id.clone(),
        Some(project.canon_revision.to_string()),
        Some(source_hash.to_owned()),
    );
    let content_ref = ContentRef::new(
        ContentProviderId::from("novel-domain-store"),
        project.project_id.clone(),
        Some(project.canon_revision.to_string()),
        source_hash,
    )?;
    let provenance = Provenance::new("domain", "novel-application", "novel-cutover-v1");
    let entities = project
        .facts
        .iter()
        .filter(|fact| fact.status == CanonStatus::Confirmed)
        .map(|fact| NovelKnowledgeEntity {
            entity_key: format!("{}:{}", fact.subject_key, fact.fact_id),
            kind: entity_kind(&fact.kind),
            name: fact.title.clone(),
            summary: fact.summary.clone(),
            properties: serde_json::json!({
                "fact_id": fact.fact_id,
                "subject_key": fact.subject_key,
                "branch_id": fact.branch_id,
                "revision": fact.revision,
                "data": fact.data,
            }),
        })
        .collect::<Vec<_>>();
    let event_id = format!(
        "novel-canon-{}-{}-{}",
        project.project_id,
        project.canon_revision,
        &source_hash[..source_hash.len().min(12)]
    );
    let knowledge = NovelKnowledgeEvent {
        project_id: project.project_id.clone(),
        canon_revision: project.canon_revision,
        status: NovelKnowledgeStatus::Committed,
        entities,
        relations: Vec::new(),
    };
    let source_event = KnowledgeSourceEvent {
        event_id,
        sequence,
        source_ref: source_ref.clone(),
        content_ref: Some(content_ref.clone()),
        visibility_scopes: vec![scope.clone()],
        provenance: provenance.clone(),
        payload: serde_json::to_value(knowledge)?,
    };
    let memory_proposals = project
        .facts
        .iter()
        .filter(|fact| fact.status == CanonStatus::Confirmed)
        .map(|fact| {
            MemoryProposal::new(
                format!(
                    "novel-memory-{}-{}-{}",
                    project.project_id, fact.fact_id, project.canon_revision
                ),
                namespace.clone(),
                MemoryTypeId::from("novel.chapter"),
                scope.clone(),
                fact.summary.clone(),
                provenance.clone(),
                TrustLevel::DomainConfirmed,
                RetentionClass::LongTerm,
                format!(
                    "novel-canon:{}:{}:{}",
                    project.project_id, fact.fact_id, project.canon_revision
                ),
            )
            .with_source(source_ref.clone())
            .with_content_ref(content_ref.clone())
        })
        .collect();
    Ok(NovelProjectionEnvelope {
        source_event,
        memory_proposals,
    })
}

const fn entity_kind(kind: &NovelFactKind) -> NovelEntityKind {
    match kind {
        NovelFactKind::Character | NovelFactKind::CharacterState => NovelEntityKind::Character,
        NovelFactKind::Location => NovelEntityKind::Location,
        NovelFactKind::Organization => NovelEntityKind::Organization,
        NovelFactKind::Item => NovelEntityKind::Item,
        NovelFactKind::Event | NovelFactKind::Timeline => NovelEntityKind::Event,
        NovelFactKind::WorldRule
        | NovelFactKind::PlotThread
        | NovelFactKind::Foreshadowing
        | NovelFactKind::Outline
        | NovelFactKind::ChapterPlan
        | NovelFactKind::ChapterSummary
        | NovelFactKind::Decision
        | NovelFactKind::Feedback
        | NovelFactKind::WritingExperience => NovelEntityKind::Concept,
    }
}
