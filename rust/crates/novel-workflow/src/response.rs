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
    let value = parse_json_value(raw).map_err(|error| {
        NovelWorkflowError::Invalid(format!(
            "Writer output must be one valid JSON object: {error}"
        ))
    })?;
    let outcome = value
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| NovelWorkflowError::Invalid("missing or invalid Novel outcome".into()))?;
    match outcome {
        "needs_clarification" => {
            let payload: ClarificationPayload = serde_json::from_value(value)?;
            if payload.questions.is_empty()
                || payload
                    .questions
                    .iter()
                    .any(|question| question.trim().is_empty())
                || payload.reason.trim().is_empty()
            {
                return Err(NovelWorkflowError::Invalid(
                    "clarification questions and reason must not be blank".into(),
                ));
            }
            Ok(NovelOutcome::NeedsClarification(ClarificationRequest {
                task_id: task_id.into(),
                project_id: project_id.into(),
                questions: payload.questions,
                reason: payload.reason,
            }))
        }
        "draft_ready" => {
            let payload = value.get("draft").cloned().ok_or_else(|| {
                NovelWorkflowError::Invalid("draft_ready requires a draft object".into())
            })?;
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
    }
}

fn build_draft(
    mut payload: DraftPayload,
    task_id: &str,
    project_id: &str,
    draft_version: u32,
    canon_revision: u64,
    fallback_evidence_refs: &[String],
) -> Result<NovelOutcome> {
    if payload.content.trim().is_empty() {
        return Err(NovelWorkflowError::Invalid(
            "Novel draft content must not be blank".into(),
        ));
    }
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

    #[test]
    fn rejects_json_without_explicit_outcome() {
        let error = parse_novel_response(
            r#"{"summary":"正文已生成"}"#,
            "task-1",
            "project-1",
            1,
            5,
            &[],
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("missing or invalid Novel outcome"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_legacy_tagged_writer_output() {
        let error = parse_novel_response(
            "[NOVEL_CONTENT]正文[/NOVEL_CONTENT]",
            "task-1",
            "project-1",
            1,
            5,
            &[],
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("valid JSON object"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_blank_draft_content() {
        let raw = serde_json::json!({
            "outcome": "draft_ready",
            "draft": {
                "content": "  \n",
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

        let error = parse_novel_response(&raw, "task-1", "project-1", 1, 5, &[]).unwrap_err();

        assert!(error.to_string().contains("content must not be blank"));
    }

    #[test]
    fn rejects_blank_clarification_fields() {
        for raw in [
            r#"{"outcome":"needs_clarification","questions":[],"reason":"缺少信息"}"#,
            r#"{"outcome":"needs_clarification","questions":["  "],"reason":"缺少信息"}"#,
            r#"{"outcome":"needs_clarification","questions":["结局是什么？"],"reason":"  "}"#,
        ] {
            let error = parse_novel_response(raw, "task-1", "project-1", 1, 5, &[]).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("clarification questions and reason must not be blank"),
                "unexpected error: {error}"
            );
        }
    }
}
