//! TaskEngine-backed Novel workflow definitions and application contracts.

mod response;
mod start;
mod writer_contract;

use std::collections::BTreeMap;

use agent_runtime::{AgentProfileSnapshot, OutputContract, ToolGrant};
use knowledge_core::ContextSnapshot;
use serde::{Deserialize, Serialize};
use task_engine::{BudgetLimits, BudgetRequest, NewTaskNode, NewTaskRun, NodeKind};

pub use response::parse_novel_response;
pub use start::*;
pub use writer_contract::render_writer_output_contract;

pub const WRITER_PROFILE_ID: &str = "novel.writer.v1";
pub const REVIEWER_PROFILE_ID: &str = "novel.reviewer.v1";
pub const CANON_EXTRACTOR_PROFILE_ID: &str = "novel.canon-extractor.v1";
pub const WORKFLOW_CONFIG_VERSION: &str = "novel-workflow-v1";

pub type Result<T> = std::result::Result<T, NovelWorkflowError>;

#[derive(Debug, thiserror::Error)]
pub enum NovelWorkflowError {
    #[error("invalid Novel workflow: {0}")]
    Invalid(String),
    #[error("Novel workflow serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileModel {
    pub provider: String,
    pub model: String,
}

impl ProfileModel {
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    fn validate(&self, role: &str) -> Result<()> {
        if self.provider.trim().is_empty() || self.model.trim().is_empty() {
            Err(NovelWorkflowError::Invalid(format!(
                "{role} provider and model are required"
            )))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelWorkflowModels {
    pub writer: ProfileModel,
    pub reviewer: ProfileModel,
    pub canon_extractor: ProfileModel,
}

impl NovelWorkflowModels {
    fn validate(&self) -> Result<()> {
        self.writer.validate("writer")?;
        self.reviewer.validate("reviewer")?;
        self.canon_extractor.validate("canon extractor")
    }
}

/// Per-node hard reservation. The TaskRun account is sized for every node in
/// the immutable workflow definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NovelWorkflowBudget {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl NovelWorkflowBudget {
    fn validate(self) -> Result<()> {
        if self.input_tokens == 0 || self.output_tokens == 0 {
            Err(NovelWorkflowError::Invalid(
                "workflow token reservations must be positive".into(),
            ))
        } else {
            Ok(())
        }
    }

    const fn reservation(self) -> BudgetRequest {
        BudgetRequest {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }

    fn total(self, node_count: usize) -> Result<BudgetLimits> {
        let count = u64::try_from(node_count)
            .map_err(|_| NovelWorkflowError::Invalid("too many workflow nodes".into()))?;
        Ok(BudgetLimits {
            input_tokens: self.input_tokens.checked_mul(count).ok_or_else(|| {
                NovelWorkflowError::Invalid("workflow input budget overflow".into())
            })?,
            output_tokens: self.output_tokens.checked_mul(count).ok_or_else(|| {
                NovelWorkflowError::Invalid("workflow output budget overflow".into())
            })?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct NovelWorkflowDefinition {
    task_run: NewTaskRun,
    profiles: BTreeMap<String, AgentProfileSnapshot>,
}

impl NovelWorkflowDefinition {
    pub fn writer_stage(
        task_id: &str,
        objective: &str,
        context_snapshot: ContextSnapshot,
        models: NovelWorkflowModels,
        budget: NovelWorkflowBudget,
    ) -> Result<Self> {
        validate_common(task_id, objective, &context_snapshot, &models, budget)?;
        let nodes = vec![model_node(
            format!("novel-writer-{task_id}"),
            NodeKind::Model,
            Vec::new(),
            &models.writer,
            WRITER_PROFILE_ID,
            budget,
        )];
        Self::build(
            task_id,
            objective,
            "novel.writer-stage",
            context_snapshot,
            models,
            budget,
            nodes,
            serde_json::json!({"kind": "compatibility_start"}),
        )
    }

    pub fn default_chapter(
        task_id: &str,
        objective: &str,
        context_snapshot: ContextSnapshot,
        models: NovelWorkflowModels,
        budget: NovelWorkflowBudget,
    ) -> Result<Self> {
        validate_common(task_id, objective, &context_snapshot, &models, budget)?;
        let writer_id = format!("novel-writer-{task_id}");
        let reviewer_id = format!("novel-reviewer-{task_id}");
        let extractor_id = format!("novel-canon-extractor-{task_id}");
        let nodes = vec![
            model_node(
                writer_id.clone(),
                NodeKind::Model,
                Vec::new(),
                &models.writer,
                WRITER_PROFILE_ID,
                budget,
            ),
            model_node(
                reviewer_id.clone(),
                NodeKind::Reviewer,
                vec![writer_id],
                &models.reviewer,
                REVIEWER_PROFILE_ID,
                budget,
            ),
            model_node(
                extractor_id,
                NodeKind::Model,
                vec![reviewer_id],
                &models.canon_extractor,
                CANON_EXTRACTOR_PROFILE_ID,
                budget,
            ),
        ];
        Self::build(
            task_id,
            objective,
            "novel.default-chapter",
            context_snapshot,
            models,
            budget,
            nodes,
            serde_json::json!({"kind": "default_chapter"}),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn competitive_outline(
        task_id: &str,
        objective: &str,
        context_snapshot: ContextSnapshot,
        models: NovelWorkflowModels,
        budget: NovelWorkflowBudget,
        candidate_count: usize,
        reviewers_per_candidate: usize,
    ) -> Result<Self> {
        validate_common(task_id, objective, &context_snapshot, &models, budget)?;
        if candidate_count < 2 || reviewers_per_candidate == 0 {
            return Err(NovelWorkflowError::Invalid(
                "competitive outline requires at least two candidates and one reviewer each".into(),
            ));
        }
        let mut nodes = Vec::with_capacity(
            candidate_count.saturating_mul(reviewers_per_candidate.saturating_add(1)),
        );
        for candidate in 1..=candidate_count {
            let writer_id = format!("novel-writer-{task_id}-candidate-{candidate}");
            nodes.push(model_node(
                writer_id.clone(),
                NodeKind::Model,
                Vec::new(),
                &models.writer,
                WRITER_PROFILE_ID,
                budget,
            ));
            for reviewer in 1..=reviewers_per_candidate {
                nodes.push(model_node(
                    format!("novel-reviewer-{task_id}-candidate-{candidate}-{reviewer}"),
                    NodeKind::Reviewer,
                    vec![writer_id.clone()],
                    &models.reviewer,
                    REVIEWER_PROFILE_ID,
                    budget,
                ));
            }
        }
        Self::build(
            task_id,
            objective,
            "novel.competitive-outline",
            context_snapshot,
            models,
            budget,
            nodes,
            serde_json::json!({
                "kind": "competitive_outline",
                "candidate_count": candidate_count,
                "reviewers_per_candidate": reviewers_per_candidate,
                "review_visibility": "sealed_candidate_only"
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        task_id: &str,
        objective: &str,
        workflow: &str,
        context_snapshot: ContextSnapshot,
        models: NovelWorkflowModels,
        budget: NovelWorkflowBudget,
        nodes: Vec<NewTaskNode>,
        workflow_policy: serde_json::Value,
    ) -> Result<Self> {
        let profiles = profile_map()?;
        let context_snapshot_id = context_snapshot.context_snapshot_id.clone();
        let context_content_hash = context_snapshot.content_hash.clone();
        let workflow_policy = serde_json::to_value(workflow_policy)?;
        let models = serde_json::to_value(models)?;
        let context_snapshot = serde_json::to_value(context_snapshot)?;
        let resolved_config = serde_json::json!({
            "workflow_policy": workflow_policy,
            "models": models,
            "profiles": profiles,
            "context_snapshot_id": context_snapshot_id,
            "context_content_hash": context_content_hash,
            "context_snapshot": context_snapshot,
            "per_node_budget": budget,
        });
        Ok(Self {
            task_run: NewTaskRun {
                task_run_id: format!("novel-task-{task_id}"),
                workflow: workflow.into(),
                objective: objective.into(),
                origin_kind: "novel_task".into(),
                origin_id: task_id.into(),
                room_id: None,
                config_version: WORKFLOW_CONFIG_VERSION.into(),
                resolved_config,
                parent_budget_account_id: None,
                budget: budget.total(nodes.len())?,
                nodes,
            },
            profiles,
        })
    }

    #[must_use]
    pub fn task_run(&self) -> &NewTaskRun {
        &self.task_run
    }

    #[must_use]
    pub fn profiles(&self) -> &BTreeMap<String, AgentProfileSnapshot> {
        &self.profiles
    }

    #[must_use]
    pub fn into_task_run(self) -> NewTaskRun {
        self.task_run
    }
}

pub fn writer_profile() -> Result<AgentProfileSnapshot> {
    profile(
        WRITER_PROFILE_ID,
        "Writer",
        "Produce one Novel candidate from only the frozen task environment. Return the declared JSON contract and never publish or mutate knowledge.",
        "novel.writer-output.v1",
    )
}

pub fn reviewer_profile() -> Result<AgentProfileSnapshot> {
    profile(
        REVIEWER_PROFILE_ID,
        "Reviewer",
        "Review only the sealed candidate artifact and authorized evidence. Bind every finding to the candidate id and content hash.",
        "novel.review-output.v1",
    )
}

pub fn canon_extractor_profile() -> Result<AgentProfileSnapshot> {
    profile(
        CANON_EXTRACTOR_PROFILE_ID,
        "CanonExtractor",
        "Extract proposed Novel entities and relations from an approved sealed artifact. Return proposals only; never commit Canon, Memory, or Graph.",
        "novel.canon-extraction.v1",
    )
}

fn profile(
    profile_id: &str,
    role: &str,
    prompt: &str,
    schema_id: &str,
) -> Result<AgentProfileSnapshot> {
    AgentProfileSnapshot::new(
        profile_id,
        1,
        role,
        vec![prompt.into()],
        ToolGrant::new(Vec::<String>::new()),
        OutputContract::Json {
            schema_id: schema_id.into(),
        },
        1,
    )
    .map_err(|error| NovelWorkflowError::Invalid(error.to_string()))
}

fn profile_map() -> Result<BTreeMap<String, AgentProfileSnapshot>> {
    let profiles = [
        writer_profile()?,
        reviewer_profile()?,
        canon_extractor_profile()?,
    ]
    .into_iter()
    .map(|profile| (profile.profile_id.clone(), profile))
    .collect::<BTreeMap<_, _>>();
    Ok(profiles)
}

fn validate_common(
    task_id: &str,
    objective: &str,
    context_snapshot: &ContextSnapshot,
    models: &NovelWorkflowModels,
    budget: NovelWorkflowBudget,
) -> Result<()> {
    if task_id.is_empty()
        || !task_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(NovelWorkflowError::Invalid(
            "task id may contain only letters, numbers, hyphens, and underscores".into(),
        ));
    }
    if objective.trim().is_empty() {
        return Err(NovelWorkflowError::Invalid(
            "workflow objective is required".into(),
        ));
    }
    context_snapshot
        .validate()
        .map_err(|error| NovelWorkflowError::Invalid(error.to_string()))?;
    models.validate()?;
    budget.validate()
}

fn model_node(
    node_id: String,
    kind: NodeKind,
    dependencies: Vec<String>,
    model: &ProfileModel,
    profile: &str,
    budget: NovelWorkflowBudget,
) -> NewTaskNode {
    NewTaskNode {
        node_id,
        kind,
        dependencies,
        provider: model.provider.clone(),
        model: model.model.clone(),
        profile: profile.into(),
        room_id: None,
        member_id: None,
        reservation: budget.reservation(),
        retryable: true,
        side_effecting: false,
    }
}
