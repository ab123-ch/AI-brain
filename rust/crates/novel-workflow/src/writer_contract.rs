use novel_domain::NovelTaskRequest;

use crate::Result;

/// 把版本化 Writer 合同连同本次冻结值渲染进提示词。
pub fn render_writer_output_contract(
    request: &NovelTaskRequest,
    branch_id: &str,
    evidence_refs: &[String],
) -> Result<String> {
    let task_type = serde_json::to_value(&request.task_type)?;
    let source_ref = request.output_path.to_string_lossy();
    let draft_ready = serde_json::json!({
        "outcome": "draft_ready",
        "draft": {
            "content": "<非空正文>",
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
                "summary": "<六项检查摘要>"
            },
            "proposed_delta": {
                "project_id": request.project_id.as_str(),
                "branch_id": branch_id,
                "expected_revision": request.expected_revision,
                "task_type": task_type,
                "source_ref": source_ref,
                "progress": null,
                "proposed_facts": [],
                "state_changes": [],
                "plot_updates": [],
                "foreshadowing_updates": [],
                "feedback": [],
                "experience_candidates": []
            },
            "evidence_refs": evidence_refs
        }
    });
    let needs_clarification = serde_json::json!({
        "outcome": "needs_clarification",
        "questions": ["<非空、可直接向用户提出的问题>"],
        "reason": "<为什么缺少的信息会阻止可靠写作>"
    });

    Ok(format!(
        "Writer output contract: novel.writer-output.v1\n\
         Return exactly one JSON object and no Markdown or explanatory text. The explicit `outcome` field is required.\n\
         Use exactly one of the two shapes below. No additional top-level shapes are accepted.\n\
         For `draft_ready`, preserve every frozen value shown below, keep all list fields present, and provide non-empty content.\n\
         For `needs_clarification`, provide at least one non-empty question and a non-empty reason.\n\n\
         draft_ready shape:\n{}\n\n\
         needs_clarification shape:\n{}",
        serde_json::to_string_pretty(&draft_ready)?,
        serde_json::to_string_pretty(&needs_clarification)?
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use novel_domain::{NovelTaskRequest, NovelTaskType, PublicationPolicy};

    use super::render_writer_output_contract;

    fn request() -> NovelTaskRequest {
        NovelTaskRequest {
            task_id: "task-20".into(),
            project_id: "project-frozen".into(),
            task_type: NovelTaskType::Body,
            task_brief: "写第20章".into(),
            target_chapter: Some(20),
            expected_revision: 7,
            output_path: PathBuf::from("chapters/0020.md"),
            context_refs: Vec::new(),
            must_happen: Vec::new(),
            must_not_change: Vec::new(),
            acceptance_criteria: vec!["完成正文".into()],
            allow_web_research: false,
            publication_policy: PublicationPolicy::RequireUserAcceptance,
            parent_task_id: None,
            source_conversation_id: None,
            source_generation_id: None,
        }
    }

    #[test]
    fn contract_contains_both_outcomes_full_shape_and_frozen_values() {
        let contract = render_writer_output_contract(
            &request(),
            "branch-frozen",
            &[
                "outline.md#sha256:abc123".into(),
                "canon:project-frozen@7".into(),
            ],
        )
        .unwrap();

        for required in [
            "novel.writer-output.v1",
            "draft_ready",
            "needs_clarification",
            "outline_alignment",
            "canon_consistency",
            "character_consistency",
            "timeline_consistency",
            "plot_and_foreshadowing",
            "style_and_repetition",
            "proposed_facts",
            "state_changes",
            "plot_updates",
            "foreshadowing_updates",
            "feedback",
            "experience_candidates",
            "project-frozen",
            "branch-frozen",
            "chapters/0020.md",
            "outline.md#sha256:abc123",
            "canon:project-frozen@7",
            "No additional top-level shapes are accepted",
        ] {
            assert!(contract.contains(required), "contract missing {required}");
        }
        assert!(contract.contains("\"expected_revision\": 7"));
        assert!(contract.contains("\"task_type\": \"body\""));
    }
}
