use novel_domain::{
    CanonStatus, ClarificationRequest, ExperienceCandidate, ForeshadowingUpdate,
    NovelDraftEnvelope, NovelFactKind, NovelMemoryDelta, NovelOutcome, NovelProjectProgress,
    NovelSelfReview, NovelSelfReviewChecks, NovelSelfReviewVerdict, NovelTaskType, PlotUpdate,
    ProposedFact, ReviewCheckStatus, ReviewIssue, StateChange,
};
use serde::Deserialize;

use crate::{NovelWorkflowError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NovelWriterOutputBinding {
    pub task_id: String,
    pub project_id: String,
    pub branch_id: String,
    pub task_type: NovelTaskType,
    pub source_ref: String,
    pub draft_version: u32,
    pub canon_revision: u64,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum WriterOutputPayload {
    DraftReady {
        draft: Box<DraftPayload>,
    },
    NeedsClarification {
        questions: Vec<String>,
        reason: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftPayload {
    content: String,
    self_review: SelfReviewPayload,
    proposed_delta: MemoryDeltaPayload,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelfReviewPayload {
    verdict: NovelSelfReviewVerdict,
    checks: SelfReviewChecksPayload,
    issues: Vec<ReviewIssuePayload>,
    unverified_assumptions: Vec<String>,
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelfReviewChecksPayload {
    outline_alignment: ReviewCheckStatus,
    canon_consistency: ReviewCheckStatus,
    character_consistency: ReviewCheckStatus,
    timeline_consistency: ReviewCheckStatus,
    plot_and_foreshadowing: ReviewCheckStatus,
    style_and_repetition: ReviewCheckStatus,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ReviewIssuePayload {
    Message(String),
    Detailed(ReviewIssueDetailsPayload),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewIssueDetailsPayload {
    category: String,
    message: String,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryDeltaPayload {
    project_id: String,
    branch_id: String,
    expected_revision: u64,
    task_type: NovelTaskType,
    source_ref: String,
    progress: RequiredNullable<ProgressPayload>,
    proposed_facts: Vec<ProposedFactPayload>,
    state_changes: Vec<StateChangePayload>,
    plot_updates: Vec<PlotUpdatePayload>,
    foreshadowing_updates: Vec<ForeshadowingUpdatePayload>,
    feedback: Vec<ProposedFactPayload>,
    experience_candidates: Vec<ExperienceCandidatePayload>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct RequiredNullable<T>(Option<T>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressPayload {
    current_volume: RequiredNullable<String>,
    current_chapter: RequiredNullable<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedFactPayload {
    fact_id: String,
    kind: NovelFactKind,
    subject_key: String,
    title: String,
    summary: String,
    data: serde_json::Value,
    status: CanonStatus,
    valid_from_chapter: RequiredNullable<u32>,
    valid_to_chapter: RequiredNullable<u32>,
    source_refs: Vec<String>,
    confidence: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateChangePayload {
    fact: ProposedFactPayload,
    supersedes_fact_id: RequiredNullable<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotUpdatePayload {
    fact: ProposedFactPayload,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForeshadowingUpdatePayload {
    fact: ProposedFactPayload,
    resolves_fact_id: RequiredNullable<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperienceCandidatePayload {
    fact: ProposedFactPayload,
    evidence_count: u32,
}

pub fn parse_novel_response(raw: &str, binding: &NovelWriterOutputBinding) -> Result<NovelOutcome> {
    let value = parse_json_value(raw).map_err(|error| {
        NovelWorkflowError::Invalid(format!(
            "Writer output must be one valid JSON object: {error}"
        ))
    })?;
    let payload: WriterOutputPayload = serde_json::from_value(value)?;
    match payload {
        WriterOutputPayload::NeedsClarification { questions, reason } => {
            if questions.is_empty()
                || questions.iter().any(|question| question.trim().is_empty())
                || reason.trim().is_empty()
            {
                return Err(NovelWorkflowError::Invalid(
                    "clarification questions and reason must not be blank".into(),
                ));
            }
            Ok(NovelOutcome::NeedsClarification(ClarificationRequest {
                task_id: binding.task_id.clone(),
                project_id: binding.project_id.clone(),
                questions,
                reason,
            }))
        }
        WriterOutputPayload::DraftReady { draft } => build_draft(*draft, binding),
    }
}

fn build_draft(payload: DraftPayload, binding: &NovelWriterOutputBinding) -> Result<NovelOutcome> {
    if payload.content.trim().is_empty() {
        return Err(NovelWorkflowError::Invalid(
            "Novel draft content must not be blank".into(),
        ));
    }
    let self_review = NovelSelfReview::from(payload.self_review);
    novel_domain::validate_self_review(&self_review)
        .map_err(|error| NovelWorkflowError::Invalid(error.to_string()))?;
    let proposed_delta = NovelMemoryDelta::from(payload.proposed_delta);
    if proposed_delta.project_id != binding.project_id
        || proposed_delta.branch_id != binding.branch_id
        || proposed_delta.expected_revision != binding.canon_revision
        || proposed_delta.task_type != binding.task_type
        || proposed_delta.source_ref != binding.source_ref
    {
        return Err(NovelWorkflowError::Invalid(
            "NovelMemoryDelta frozen project, branch, revision, task type, or source ref differs from the task".into(),
        ));
    }
    if payload.evidence_refs != binding.evidence_refs {
        return Err(NovelWorkflowError::Invalid(
            "Novel draft evidence_refs differ from the frozen task".into(),
        ));
    }
    Ok(NovelOutcome::DraftReady(NovelDraftEnvelope {
        task_id: binding.task_id.clone(),
        draft_version: binding.draft_version,
        project_id: binding.project_id.clone(),
        canon_revision: binding.canon_revision,
        content: payload.content,
        self_review,
        proposed_delta,
        evidence_refs: payload.evidence_refs,
    }))
}

fn parse_json_value(raw: &str) -> std::result::Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str(raw.trim())
}

impl From<SelfReviewPayload> for NovelSelfReview {
    fn from(payload: SelfReviewPayload) -> Self {
        Self {
            verdict: payload.verdict,
            checks: payload.checks.into(),
            issues: payload.issues.into_iter().map(Into::into).collect(),
            unverified_assumptions: payload.unverified_assumptions,
            summary: payload.summary,
        }
    }
}

impl From<SelfReviewChecksPayload> for NovelSelfReviewChecks {
    fn from(payload: SelfReviewChecksPayload) -> Self {
        Self {
            outline_alignment: payload.outline_alignment,
            canon_consistency: payload.canon_consistency,
            character_consistency: payload.character_consistency,
            timeline_consistency: payload.timeline_consistency,
            plot_and_foreshadowing: payload.plot_and_foreshadowing,
            style_and_repetition: payload.style_and_repetition,
        }
    }
}

impl From<ReviewIssuePayload> for ReviewIssue {
    fn from(payload: ReviewIssuePayload) -> Self {
        match payload {
            ReviewIssuePayload::Message(message) => Self::Message(message),
            ReviewIssuePayload::Detailed(details) => Self::Detailed {
                category: details.category,
                message: details.message,
                evidence_refs: details.evidence_refs,
            },
        }
    }
}

impl From<MemoryDeltaPayload> for NovelMemoryDelta {
    fn from(payload: MemoryDeltaPayload) -> Self {
        Self {
            project_id: payload.project_id,
            branch_id: payload.branch_id,
            expected_revision: payload.expected_revision,
            task_type: payload.task_type,
            source_ref: payload.source_ref,
            progress: payload.progress.0.map(Into::into),
            proposed_facts: payload.proposed_facts.into_iter().map(Into::into).collect(),
            state_changes: payload.state_changes.into_iter().map(Into::into).collect(),
            plot_updates: payload.plot_updates.into_iter().map(Into::into).collect(),
            foreshadowing_updates: payload
                .foreshadowing_updates
                .into_iter()
                .map(Into::into)
                .collect(),
            feedback: payload.feedback.into_iter().map(Into::into).collect(),
            experience_candidates: payload
                .experience_candidates
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<ProgressPayload> for NovelProjectProgress {
    fn from(payload: ProgressPayload) -> Self {
        Self {
            current_volume: payload.current_volume.0,
            current_chapter: payload.current_chapter.0,
        }
    }
}

impl From<ProposedFactPayload> for ProposedFact {
    fn from(payload: ProposedFactPayload) -> Self {
        Self {
            fact_id: payload.fact_id,
            kind: payload.kind,
            subject_key: payload.subject_key,
            title: payload.title,
            summary: payload.summary,
            data: payload.data,
            status: payload.status,
            valid_from_chapter: payload.valid_from_chapter.0,
            valid_to_chapter: payload.valid_to_chapter.0,
            source_refs: payload.source_refs,
            confidence: payload.confidence,
        }
    }
}

impl From<StateChangePayload> for StateChange {
    fn from(payload: StateChangePayload) -> Self {
        Self {
            fact: payload.fact.into(),
            supersedes_fact_id: payload.supersedes_fact_id.0,
        }
    }
}

impl From<PlotUpdatePayload> for PlotUpdate {
    fn from(payload: PlotUpdatePayload) -> Self {
        Self {
            fact: payload.fact.into(),
        }
    }
}

impl From<ForeshadowingUpdatePayload> for ForeshadowingUpdate {
    fn from(payload: ForeshadowingUpdatePayload) -> Self {
        Self {
            fact: payload.fact.into(),
            resolves_fact_id: payload.resolves_fact_id.0,
        }
    }
}

impl From<ExperienceCandidatePayload> for ExperienceCandidate {
    fn from(payload: ExperienceCandidatePayload) -> Self {
        Self {
            fact: payload.fact.into(),
            evidence_count: payload.evidence_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> NovelWriterOutputBinding {
        NovelWriterOutputBinding {
            task_id: "task-1".into(),
            project_id: "project-1".into(),
            branch_id: "main".into(),
            task_type: NovelTaskType::Body,
            source_ref: "chapter-20.md".into(),
            draft_version: 1,
            canon_revision: 5,
            evidence_refs: Vec::new(),
        }
    }

    fn parse(raw: &str) -> Result<NovelOutcome> {
        parse_novel_response(raw, &binding())
    }

    fn valid_draft_value() -> serde_json::Value {
        serde_json::json!({
            "outcome": "draft_ready",
            "draft": {
                "content": "第20章正文",
                "self_review": {
                    "verdict": "pass",
                    "checks": {
                        "outline_alignment": "pass",
                        "canon_consistency": "pass",
                        "character_consistency": "pass",
                        "timeline_consistency": "pass",
                        "plot_and_foreshadowing": "pass",
                        "style_and_repetition": "pass"
                    },
                    "issues": [],
                    "unverified_assumptions": [],
                    "summary": "六项检查通过"
                },
                "proposed_delta": {
                    "project_id": "project-1",
                    "branch_id": "main",
                    "expected_revision": 5,
                    "task_type": "body",
                    "source_ref": "chapter-20.md",
                    "progress": null,
                    "proposed_facts": [],
                    "state_changes": [],
                    "plot_updates": [],
                    "foreshadowing_updates": [],
                    "feedback": [],
                    "experience_candidates": []
                },
                "evidence_refs": []
            }
        })
    }

    #[test]
    fn parses_domain_shaped_nested_draft_payload() {
        let raw = valid_draft_value().to_string();

        let outcome = parse(&raw).unwrap();

        let NovelOutcome::DraftReady(draft) = outcome else {
            panic!("应解析为候选稿");
        };
        assert_eq!(draft.content, "第20章正文");
    }

    #[test]
    fn rejects_json_without_explicit_outcome() {
        let error = parse(r#"{"summary":"正文已生成"}"#).unwrap_err();

        assert!(
            error.to_string().contains("outcome"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_legacy_tagged_writer_output() {
        let error = parse("[NOVEL_CONTENT]正文[/NOVEL_CONTENT]").unwrap_err();

        assert!(
            error.to_string().contains("valid JSON object"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_blank_draft_content() {
        let mut value = valid_draft_value();
        value["draft"]["content"] = serde_json::json!("  \n");
        let raw = value.to_string();

        let error = parse(&raw).unwrap_err();

        assert!(error.to_string().contains("content must not be blank"));
    }

    #[test]
    fn rejects_blank_clarification_fields() {
        for raw in [
            r#"{"outcome":"needs_clarification","questions":[],"reason":"缺少信息"}"#,
            r#"{"outcome":"needs_clarification","questions":["  "],"reason":"缺少信息"}"#,
            r#"{"outcome":"needs_clarification","questions":["结局是什么？"],"reason":"  "}"#,
        ] {
            let error = parse(raw).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("clarification questions and reason must not be blank"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn rejects_draft_missing_required_contract_fields() {
        let raw = serde_json::json!({
            "outcome": "draft_ready",
            "draft": {
                "content": "正文",
                "self_review": {
                    "verdict": "pass",
                    "checks": {
                        "outline_alignment": "pass",
                        "canon_consistency": "pass",
                        "character_consistency": "pass",
                        "timeline_consistency": "pass",
                        "plot_and_foreshadowing": "pass",
                        "style_and_repetition": "pass"
                    },
                    "summary": "六项检查通过"
                },
                "proposed_delta": {
                    "project_id": "project-1",
                    "expected_revision": 5,
                    "task_type": "body",
                    "source_ref": "chapter-20.md"
                }
            }
        })
        .to_string();

        let error = parse(&raw).unwrap_err();

        assert!(error.to_string().contains("missing field"), "{error}");
    }

    #[test]
    fn rejects_unknown_contract_fields() {
        let raw = serde_json::json!({
            "outcome": "needs_clarification",
            "questions": ["需要什么结局？"],
            "reason": "结局未冻结",
            "unexpected": "must fail"
        })
        .to_string();

        let error = parse(&raw).unwrap_err();

        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn rejects_markdown_fenced_json() {
        let error = parse(
            "```json\n{\"outcome\":\"needs_clarification\",\"questions\":[\"需要什么结局？\"],\"reason\":\"结局未冻结\"}\n```",
        )
        .unwrap_err();

        assert!(error.to_string().contains("valid JSON object"), "{error}");
    }

    #[test]
    fn rejects_frozen_delta_or_evidence_mismatch() {
        for (path, replacement) in [
            (
                "/draft/proposed_delta/project_id",
                serde_json::json!("other"),
            ),
            (
                "/draft/proposed_delta/branch_id",
                serde_json::json!("other"),
            ),
            (
                "/draft/proposed_delta/expected_revision",
                serde_json::json!(6),
            ),
            (
                "/draft/proposed_delta/task_type",
                serde_json::json!("polish"),
            ),
            (
                "/draft/proposed_delta/source_ref",
                serde_json::json!("other.md"),
            ),
            ("/draft/evidence_refs", serde_json::json!(["other"])),
        ] {
            let mut value = valid_draft_value();
            *value.pointer_mut(path).expect("测试路径必须存在") = replacement;
            let error = parse(&value.to_string()).unwrap_err();
            assert!(
                error.to_string().contains("frozen") || error.to_string().contains("evidence_refs"),
                "path={path}, error={error}"
            );
        }
    }
}
