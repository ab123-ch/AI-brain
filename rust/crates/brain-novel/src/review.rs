use brain_memory::novel::NovelMemoryDelta;
use serde::Deserialize;

use crate::{
    ClarificationRequest, MainReviewRecord, MainReviewVerdict, NovelBrainError, NovelDraftEnvelope,
    NovelOutcome, NovelSelfReview, NovelSelfReviewVerdict, Result,
};

#[derive(Debug, Deserialize)]
struct DraftPayload {
    content: String,
    self_review: NovelSelfReview,
    proposed_delta: NovelMemoryDelta,
    #[serde(default)]
    evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ClarificationPayload {
    questions: Vec<String>,
    reason: String,
}

pub fn validate_self_review(review: &NovelSelfReview) -> Result<()> {
    let issues_are_valid = review
        .issues
        .iter()
        .all(|issue| !issue.message().trim().is_empty());
    let assumptions_are_valid = review
        .unverified_assumptions
        .iter()
        .all(|item| !item.trim().is_empty());
    let pass_is_consistent = review.verdict == NovelSelfReviewVerdict::Pass
        && review.checks.all_pass()
        && review.issues.is_empty()
        && !review.summary.trim().is_empty();
    if !issues_are_valid || !assumptions_are_valid || !pass_is_consistent {
        return Err(NovelBrainError::InvalidModelOutput(
            "小说脑自检必须包含六项 pass、空 issues 和 verdict=pass".into(),
        ));
    }
    Ok(())
}

pub fn validate_main_review(review: &MainReviewRecord, draft: &NovelDraftEnvelope) -> Result<()> {
    if review.task_id != draft.task_id
        || review.draft_version != draft.draft_version
        || review.reviewed_canon_revision != draft.canon_revision
    {
        return Err(NovelBrainError::InvalidTransition(
            "主脑复审记录对应的 task、draft version 或 Canon revision 已过期".into(),
        ));
    }
    if review.summary.trim().is_empty()
        || review
            .issues
            .iter()
            .any(|issue| issue.message().trim().is_empty())
    {
        return Err(NovelBrainError::InvalidTransition(
            "主脑复审必须包含有效 summary 和 issue".into(),
        ));
    }
    match review.verdict {
        MainReviewVerdict::Pass => {
            validate_self_review(&draft.self_review)?;
            let distinct_evidence = review
                .evidence_refs
                .iter()
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .collect::<std::collections::HashSet<_>>();
            if !review.checks.all_pass() || !review.issues.is_empty() || distinct_evidence.len() < 3
            {
                return Err(NovelBrainError::InvalidTransition(
                    "Pass 复审必须全部检查通过、issues 为空且至少包含三类 evidence refs".into(),
                ));
            }
        }
        MainReviewVerdict::Revise => {
            if review.issues.is_empty() {
                return Err(NovelBrainError::InvalidTransition(
                    "Revise 复审必须包含具体问题".into(),
                ));
            }
        }
    }
    Ok(())
}

pub fn parse_novel_response(
    raw: &str,
    task_id: &str,
    project_id: &str,
    draft_version: u32,
    canon_revision: u64,
    fallback_evidence_refs: &[String],
) -> Result<NovelOutcome> {
    if let Ok(value) = parse_json_value(raw) {
        let outcome = value
            .get("outcome")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("draft_ready");
        return match outcome {
            "needs_clarification" => {
                let payload: ClarificationPayload = serde_json::from_value(value)?;
                Ok(NovelOutcome::NeedsClarification(ClarificationRequest {
                    task_id: task_id.into(),
                    project_id: project_id.into(),
                    questions: payload.questions,
                    reason: payload.reason,
                }))
            }
            "draft_ready" => {
                let payload: DraftPayload = serde_json::from_value(value)?;
                build_draft(
                    payload,
                    task_id,
                    project_id,
                    draft_version,
                    canon_revision,
                    fallback_evidence_refs,
                )
            }
            other => Err(NovelBrainError::InvalidModelOutput(format!(
                "未知 outcome: {other}"
            ))),
        };
    }

    let content = tagged_section(raw, "[NOVEL_CONTENT]", "[/NOVEL_CONTENT]")
        .ok_or_else(|| NovelBrainError::InvalidModelOutput("缺少 JSON 或 NOVEL_CONTENT".into()))?;
    let review = tagged_section(raw, "[NOVEL_SELF_REVIEW]", "[/NOVEL_SELF_REVIEW]")
        .ok_or_else(|| NovelBrainError::InvalidModelOutput("缺少 NOVEL_SELF_REVIEW".into()))?;
    let delta = tagged_section(raw, "[NOVEL_MEMORY_DELTA]", "[/NOVEL_MEMORY_DELTA]")
        .ok_or_else(|| NovelBrainError::InvalidModelOutput("缺少 NOVEL_MEMORY_DELTA".into()))?;
    build_draft(
        DraftPayload {
            content: content.trim().into(),
            self_review: serde_json::from_str(review.trim())?,
            proposed_delta: serde_json::from_str(delta.trim())?,
            evidence_refs: fallback_evidence_refs.to_vec(),
        },
        task_id,
        project_id,
        draft_version,
        canon_revision,
        fallback_evidence_refs,
    )
}

fn build_draft(
    mut payload: DraftPayload,
    task_id: &str,
    project_id: &str,
    draft_version: u32,
    canon_revision: u64,
    fallback_evidence_refs: &[String],
) -> Result<NovelOutcome> {
    validate_self_review(&payload.self_review)?;
    if payload.proposed_delta.project_id != project_id
        || payload.proposed_delta.expected_revision != canon_revision
    {
        return Err(NovelBrainError::InvalidModelOutput(
            "NovelMemoryDelta 的 project_id 或 expected_revision 与任务环境不一致".into(),
        ));
    }
    if payload.evidence_refs.is_empty() {
        payload.evidence_refs = fallback_evidence_refs.to_vec();
    }
    Ok(NovelOutcome::DraftReady(NovelDraftEnvelope {
        task_id: task_id.into(),
        draft_version,
        project_id: project_id.into(),
        canon_revision,
        content: payload.content,
        self_review: payload.self_review,
        proposed_delta: payload.proposed_delta,
        evidence_refs: payload.evidence_refs,
    }))
}

fn parse_json_value(raw: &str) -> std::result::Result<serde_json::Value, serde_json::Error> {
    let trimmed = raw.trim();
    let candidate = if let Some(fenced) = trimmed.strip_prefix("```json") {
        fenced.strip_suffix("```").unwrap_or(fenced).trim()
    } else if let Some(fenced) = trimmed.strip_prefix("```") {
        fenced.strip_suffix("```").unwrap_or(fenced).trim()
    } else {
        trimmed
    };
    serde_json::from_str(candidate)
}

fn tagged_section<'a>(value: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_index = value.find(start)? + start.len();
    let remainder = &value[start_index..];
    let end_index = remainder.find(end)?;
    Some(&remainder[..end_index])
}
