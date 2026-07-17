use std::sync::Arc;

use brain_llm::{ChatMessage, ChatRequest, LlmProvider};
use brain_memory::novel::{ConsistencyReport, NovelProject, NovelRecallPack};
use serde_json::json;

use crate::ports::ContextDocument;
use crate::{parse_novel_response, NovelBrainError, NovelOutcome, NovelTaskState, Result};

const NOVEL_WRITING_WORKFLOW: &str =
    include_str!("../../brain-main/skills/novel-writing-workflow/SKILL.md");

pub(crate) struct GenerationResult {
    pub outcome: NovelOutcome,
    pub user_message: ChatMessage,
    pub assistant_message: ChatMessage,
}

pub(crate) struct NovelModelRuntime {
    llm: Arc<dyn LlmProvider>,
}

impl NovelModelRuntime {
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self { llm }
    }

    pub async fn generate(
        &self,
        task: &NovelTaskState,
        project: &NovelProject,
        recall: &NovelRecallPack,
        consistency: &ConsistencyReport,
        documents: &[ContextDocument],
        history: &[ChatMessage],
        revision_instruction: Option<&str>,
    ) -> Result<GenerationResult> {
        let next_version = task.draft_version + 1;
        let user_payload = build_user_payload(
            task,
            project,
            recall,
            consistency,
            documents,
            revision_instruction,
        )?;
        let user_message = ChatMessage::user(user_payload);
        let mut messages = Vec::with_capacity(history.len() + 2);
        messages.push(ChatMessage::system(system_prompt()));
        messages.extend(history.iter().cloned());
        messages.push(user_message.clone());

        let response = self
            .llm
            .complete(ChatRequest {
                model: Some(self.llm.model().to_string()),
                messages,
                max_tokens: None,
                temperature: None,
                tools: None,
                tool_choice: None,
            })
            .await
            .map_err(|error| NovelBrainError::Model(error.to_string()))?;
        let raw = response.text();
        if raw.trim().is_empty() {
            return Err(NovelBrainError::InvalidModelOutput(
                "模型没有返回正文或澄清请求".into(),
            ));
        }
        let fallback_evidence_refs = evidence_refs(task, recall, documents);
        let outcome = parse_novel_response(
            &raw,
            &task.request.task_id,
            &task.request.project_id,
            next_version,
            task.request.expected_revision,
            &fallback_evidence_refs,
        )?;
        Ok(GenerationResult {
            outcome,
            user_message,
            assistant_message: ChatMessage::assistant(raw),
        })
    }
}

fn system_prompt() -> String {
    format!(
        "你是 AI Brain 中常驻的小说脑。你只与主脑协作，不直接向用户发布内容。\n\
         你必须严格服从主脑提供的任务合同、Canon、ConsistencyReport 和授权材料全文。\n\
         你不能访问文件系统、记忆路径或图谱路径；所有数据都已由服务端放入当前环境。\n\
         先完成创作，再执行六项自检。只有六项均 pass 且 issues 为空时才能返回 draft_ready。\n\
         若关键条件不足，返回 needs_clarification，由主脑转问用户。不要自行发布或声称已经保存。\n\
         \n\
         只返回一个 JSON 对象，不要 Markdown 围栏。草稿格式：\n\
         {{\"outcome\":\"draft_ready\",\"content\":\"...\",\"self_review\":{{\"verdict\":\"pass\",\"checks\":{{\"outline_alignment\":\"pass\",\"canon_consistency\":\"pass\",\"character_consistency\":\"pass\",\"timeline_consistency\":\"pass\",\"plot_and_foreshadowing\":\"pass\",\"style_and_repetition\":\"pass\"}},\"issues\":[],\"unverified_assumptions\":[],\"summary\":\"...\"}},\"proposed_delta\":{{...NovelMemoryDelta...}},\"evidence_refs\":[\"...\"]}}。\n\
         澄清格式：{{\"outcome\":\"needs_clarification\",\"questions\":[\"...\"],\"reason\":\"...\"}}。\n\
         project_id、branch_id、expected_revision 和 source_ref 必须与任务环境完全一致。\n\n\
         <novel-writing-workflow>\n{NOVEL_WRITING_WORKFLOW}\n</novel-writing-workflow>"
    )
}

fn build_user_payload(
    task: &NovelTaskState,
    project: &NovelProject,
    recall: &NovelRecallPack,
    consistency: &ConsistencyReport,
    documents: &[ContextDocument],
    revision_instruction: Option<&str>,
) -> Result<String> {
    let context = documents
        .iter()
        .map(|document| {
            json!({
                "role": document.reference.role,
                "canonical_path": document.reference.canonical_path,
                "sha256": document.reference.sha256,
                "description": document.reference.description,
                "content": document.content,
            })
        })
        .collect::<Vec<_>>();
    let payload = json!({
        "task_contract": task.request,
        "project": {
            "project_id": project.project_id,
            "title": project.title,
            "active_branch": project.active_branch,
            "canon_revision": project.canon_revision,
            "current_volume": project.current_volume,
            "current_chapter": project.current_chapter,
        },
        "canon_recall": recall,
        "consistency_report": consistency,
        "authorized_context": context,
        "previous_draft": task.draft,
        "revision_instruction": revision_instruction,
        "server_constraints": {
            "next_draft_version": task.draft_version + 1,
            "project_id": task.request.project_id,
            "branch_id": project.active_branch,
            "expected_revision": task.request.expected_revision,
            "source_ref": task.request.output_path,
        }
    });
    serde_json::to_string_pretty(&payload).map_err(Into::into)
}

fn evidence_refs(
    task: &NovelTaskState,
    recall: &NovelRecallPack,
    documents: &[ContextDocument],
) -> Vec<String> {
    let mut refs = vec![
        format!("task:{}:requirements", task.request.task_id),
        format!(
            "memory:novel:{}:revision:{}",
            recall.project_id, recall.revision
        ),
    ];
    refs.extend(documents.iter().map(|document| {
        document
            .reference
            .canonical_path
            .to_string_lossy()
            .into_owned()
    }));
    refs.sort();
    refs.dedup();
    refs
}
