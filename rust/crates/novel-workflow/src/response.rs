use novel_domain::{
    ClarificationRequest, NovelDraftEnvelope, NovelMemoryDelta, NovelOutcome, NovelSelfReview,
};
use serde::Deserialize;

use crate::{NovelWorkflowError, Result};

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
                let payload = value.get("draft").cloned().unwrap_or(value);
                build_draft(
                    serde_json::from_value(payload)?,
                    task_id,
                    project_id,
                    draft_version,
                    canon_revision,
                    fallback_evidence_refs,
                )
            }
            other => Err(NovelWorkflowError::Invalid(format!(
                "unknown Novel outcome: {other}"
            ))),
        };
    }

    let content = tagged_section(raw, "[NOVEL_CONTENT]", "[/NOVEL_CONTENT]")
        .ok_or_else(|| NovelWorkflowError::Invalid("missing JSON or NOVEL_CONTENT".into()))?;
    let review = tagged_section(raw, "[NOVEL_SELF_REVIEW]", "[/NOVEL_SELF_REVIEW]")
        .ok_or_else(|| NovelWorkflowError::Invalid("missing NOVEL_SELF_REVIEW".into()))?;
    let delta = tagged_section(raw, "[NOVEL_MEMORY_DELTA]", "[/NOVEL_MEMORY_DELTA]")
        .ok_or_else(|| NovelWorkflowError::Invalid("missing NOVEL_MEMORY_DELTA".into()))?;
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
    novel_domain::validate_self_review(&payload.self_review)
        .map_err(|error| NovelWorkflowError::Invalid(error.to_string()))?;
    if payload.proposed_delta.project_id != project_id
        || payload.proposed_delta.expected_revision != canon_revision
    {
        return Err(NovelWorkflowError::Invalid(
            "NovelMemoryDelta project or revision differs from the frozen task".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_domain_shaped_nested_draft_payload() {
        let raw = serde_json::json!({
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

        let outcome = parse_novel_response(&raw, "task-1", "project-1", 1, 5, &[]).unwrap();

        let NovelOutcome::DraftReady(draft) = outcome else {
            panic!("应解析为候选稿");
        };
        assert_eq!(draft.content, "第20章正文");
    }
}
