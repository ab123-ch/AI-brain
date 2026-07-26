use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelKnowledgeStatus {
    Draft,
    Approved,
    Committed,
    Rejected,
}

impl NovelKnowledgeStatus {
    #[must_use]
    pub const fn is_projectable(self) -> bool {
        matches!(self, Self::Approved | Self::Committed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelEntityKind {
    Character,
    Location,
    Item,
    Event,
    Organization,
    Concept,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelKnowledgeEntity {
    pub entity_key: String,
    pub kind: NovelEntityKind,
    pub name: String,
    pub summary: String,
    #[serde(default)]
    pub properties: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NovelRelationKind {
    Contains,
    LocatedAt,
    Owns,
    ParticipatesIn,
    RelatedTo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelKnowledgeRelation {
    pub source_entity_key: String,
    pub target_entity_key: String,
    pub kind: NovelRelationKind,
    #[serde(default)]
    pub properties: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelKnowledgeEvent {
    pub status: NovelKnowledgeStatus,
    pub project_id: String,
    pub canon_revision: u64,
    #[serde(default)]
    pub entities: Vec<NovelKnowledgeEntity>,
    #[serde(default)]
    pub relations: Vec<NovelKnowledgeRelation>,
}
