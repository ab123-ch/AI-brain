use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use brain_llm::{ChatMessage, ChatRequest, ChatResponse, FinishReason, LlmProvider};
use novel_application::{NovelApplicationError, NovelResourcePort};
use novel_domain::{ContextRef, NovelArtifactReceipt};
use novel_workflow::{
    parse_novel_response, render_writer_output_contract, NovelContextDocument,
    NovelWorkflowPortError, NovelWriterExecution, NovelWriterInvocation, NovelWriterPort,
};
use regex::Regex;
use sha2::{Digest, Sha256};
use task_engine::ActualUsage;

const NOVEL_WRITING_WORKFLOW: &str =
    include_str!("../../brain-main/skills/novel-writing-workflow/SKILL.md");
const WRITER_DIAGNOSTIC_PREVIEW_CHARS: usize = 2_048;
const PROVIDER_ERROR_CHARS: usize = 2_048;

pub struct LlmNovelWriterAdapter {
    llm: Arc<dyn LlmProvider>,
    temperature: f64,
}

struct PreparedWriterRequest {
    max_tokens: u32,
    evidence_refs: Vec<String>,
    base_messages: Vec<ChatMessage>,
}

struct InvalidWriterResponse {
    response: ChatResponse,
    raw: String,
    parse_error: String,
    usage: ActualUsage,
}

impl LlmNovelWriterAdapter {
    #[must_use]
    pub fn new(llm: Arc<dyn LlmProvider>, temperature: f64) -> Self {
        Self { llm, temperature }
    }

    async fn execute_prepared(
        &self,
        invocation: &NovelWriterInvocation,
        prepared: PreparedWriterRequest,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
        let first_response = self
            .llm
            .complete(ChatRequest {
                model: Some(invocation.model.model.clone()),
                messages: prepared.base_messages.clone(),
                max_tokens: Some(prepared.max_tokens),
                temperature: Some(self.temperature),
                tools: None,
                tool_choice: None,
            })
            .await
            .map_err(|error| {
                provider_failure(
                    &invocation.model.provider,
                    &invocation.model.model,
                    1,
                    &error,
                )
            })?;
        let first_raw = first_response.text();
        let first_usage = response_usage(&first_response);
        match parse_novel_response(
            &first_raw,
            &invocation.request.task_id,
            &invocation.request.project_id,
            invocation.next_draft_version,
            invocation.request.expected_revision,
            &prepared.evidence_refs,
        ) {
            Ok(outcome) => Ok(NovelWriterExecution {
                outcome,
                raw_output: first_raw,
                usage: first_usage,
            }),
            Err(error) => {
                self.repair_writer_output(
                    invocation,
                    prepared,
                    InvalidWriterResponse {
                        response: first_response,
                        raw: first_raw,
                        parse_error: error.to_string(),
                        usage: first_usage,
                    },
                )
                .await
            }
        }
    }

    async fn repair_writer_output(
        &self,
        invocation: &NovelWriterInvocation,
        prepared: PreparedWriterRequest,
        invalid: InvalidWriterResponse,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
        let remaining_tokens = remaining_output_tokens(
            prepared.max_tokens,
            invalid.response.usage.completion_tokens,
        );
        let first_diagnostic = writer_output_diagnostic(
            &invocation.model.provider,
            &invocation.model.model,
            1,
            &invalid.response,
            &invalid.raw,
            &invalid.parse_error,
            invalid.usage,
        );
        if remaining_tokens == 0 {
            tracing::warn!("Novel Writer 输出格式错误且无剩余纠正预算: {first_diagnostic}");
            return Err(NovelWorkflowPortError::InvalidWriterOutput(
                first_diagnostic,
            ));
        }

        let mut repair_messages = prepared.base_messages;
        repair_messages.push(ChatMessage::assistant(invalid.raw));
        repair_messages.push(ChatMessage::user(
            "上一条 assistant 响应违反 writer-output-contract。只纠正 JSON 结构和必填字段；\
             保持正文语义与所有冻结值不变；只返回一个合同允许的 JSON 对象，不要 Markdown 或解释。",
        ));
        let second_response = self
            .llm
            .complete(ChatRequest {
                model: Some(invocation.model.model.clone()),
                messages: repair_messages,
                max_tokens: Some(remaining_tokens),
                temperature: Some(self.temperature),
                tools: None,
                tool_choice: None,
            })
            .await
            .map_err(|error| {
                NovelWorkflowPortError::Storage(format!(
                    "{}; first_invalid_response={first_diagnostic}",
                    provider_failure_message(
                        &invocation.model.provider,
                        &invocation.model.model,
                        2,
                        &error,
                    )
                ))
            })?;
        let second_raw = second_response.text();
        let total_usage = add_usage(invalid.usage, response_usage(&second_response));
        let outcome = parse_novel_response(
            &second_raw,
            &invocation.request.task_id,
            &invocation.request.project_id,
            invocation.next_draft_version,
            invocation.request.expected_revision,
            &prepared.evidence_refs,
        )
        .map_err(|error| {
            let diagnostic = writer_output_diagnostic(
                &invocation.model.provider,
                &invocation.model.model,
                2,
                &second_response,
                &second_raw,
                &format!("{error}; first_parse_error={}", invalid.parse_error),
                total_usage,
            );
            tracing::warn!("Novel Writer 二次输出仍不符合合同: {diagnostic}");
            NovelWorkflowPortError::InvalidWriterOutput(diagnostic)
        })?;
        Ok(NovelWriterExecution {
            outcome,
            raw_output: second_raw,
            usage: total_usage,
        })
    }
}

#[async_trait]
impl NovelWriterPort for LlmNovelWriterAdapter {
    async fn execute(
        &self,
        invocation: NovelWriterInvocation,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
        let prepared = prepare_writer_request(&invocation)?;
        self.execute_prepared(&invocation, prepared).await
    }
}

fn prepare_writer_request(
    invocation: &NovelWriterInvocation,
) -> Result<PreparedWriterRequest, NovelWorkflowPortError> {
    invocation.context_snapshot.validate().map_err(|error| {
        NovelWorkflowPortError::ContextChanged(format!(
            "Writer ContextSnapshot validation failed: {error}"
        ))
    })?;
    if invocation.profile.tool_grant.iter().next().is_some() {
        return Err(NovelWorkflowPortError::InvalidRequest(
            "Novel Writer profile must not grant tools".into(),
        ));
    }
    let max_tokens = u32::try_from(invocation.budget.output_tokens).map_err(|_| {
        NovelWorkflowPortError::InvalidRequest(
            "Novel Writer output reservation exceeds Provider limits".into(),
        )
    })?;
    let mut evidence_refs = invocation
        .context_documents
        .iter()
        .map(|document| {
            format!(
                "{}#sha256:{}",
                document.reference.canonical_path.display(),
                document.reference.sha256
            )
        })
        .collect::<Vec<_>>();
    evidence_refs.push(format!(
        "canon:{}@{}",
        invocation.request.project_id, invocation.request.expected_revision
    ));
    let output_contract = render_writer_output_contract(
        &invocation.request,
        &invocation.project.active_branch,
        &evidence_refs,
    )
    .map_err(|error| {
        NovelWorkflowPortError::InvalidRequest(format!(
            "Novel Writer output contract rendering failed: {error}"
        ))
    })?;
    let system_prompt = format!(
        "{}\n\
         Never publish, write Canon, Memory, Graph, or files.\n\n\
         <writer-output-contract>\n{output_contract}\n</writer-output-contract>\n\n\
         <novel-writing-workflow>\n{NOVEL_WRITING_WORKFLOW}\n</novel-writing-workflow>",
        invocation.profile.system_prompt.join("\n")
    );
    Ok(PreparedWriterRequest {
        max_tokens,
        evidence_refs,
        base_messages: vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(invocation.context_snapshot.render()),
        ],
    })
}

fn provider_failure(
    provider: &str,
    model: &str,
    attempt: u8,
    error: &brain_llm::LlmError,
) -> NovelWorkflowPortError {
    NovelWorkflowPortError::Storage(provider_failure_message(provider, model, attempt, error))
}

fn provider_failure_message(
    provider: &str,
    model: &str,
    attempt: u8,
    error: &brain_llm::LlmError,
) -> String {
    let error = bounded_sensitive_text(&error.to_string(), PROVIDER_ERROR_CHARS);
    format!(
        "Novel Writer Provider failed (provider={provider}, model={model}, attempt={attempt}/2): {error}"
    )
}

const fn response_usage(response: &ChatResponse) -> ActualUsage {
    ActualUsage {
        input_tokens: response.usage.prompt_tokens,
        output_tokens: response.usage.completion_tokens,
    }
}

const fn add_usage(left: ActualUsage, right: ActualUsage) -> ActualUsage {
    ActualUsage {
        input_tokens: left.input_tokens.saturating_add(right.input_tokens),
        output_tokens: left.output_tokens.saturating_add(right.output_tokens),
    }
}

fn remaining_output_tokens(reserved: u32, used: u64) -> u32 {
    u32::try_from(used).map_or(0, |used| reserved.saturating_sub(used))
}

fn writer_output_diagnostic(
    provider: &str,
    model: &str,
    attempt: u8,
    response: &ChatResponse,
    raw: &str,
    parse_error: &str,
    usage: ActualUsage,
) -> String {
    let preview = escape_control_chars(&redact_writer_output(raw))
        .chars()
        .take(WRITER_DIAGNOSTIC_PREVIEW_CHARS)
        .collect::<String>();
    format!(
        "provider={provider}, model={model}, attempt={attempt}/2, finish_reason={}, \
         actual_input_tokens={}, actual_output_tokens={}, raw_sha256={}, raw_preview={}, parse_error={}",
        finish_reason_label(response.finish_reason.as_ref()),
        usage.input_tokens,
        usage.output_tokens,
        sha256(raw.as_bytes()),
        serde_json::to_string(&preview).unwrap_or_else(|_| "\"[PREVIEW_SERIALIZATION_FAILED]\"".into()),
        bounded_parse_error(parse_error)
    )
}

fn finish_reason_label(reason: Option<&FinishReason>) -> &'static str {
    match reason {
        Some(FinishReason::EndTurn) => "end_turn",
        Some(FinishReason::ToolUse) => "tool_use",
        Some(FinishReason::MaxTokens) => "max_tokens",
        None => "unknown",
    }
}

fn redact_writer_output(raw: &str) -> String {
    static SENSITIVE_VALUE: OnceLock<Regex> = OnceLock::new();
    static SK_TOKEN: OnceLock<Regex> = OnceLock::new();
    let sensitive_value = SENSITIVE_VALUE.get_or_init(|| {
        Regex::new(
            r#"(?i)(["']?(?:authorization|api[_-]?key|token|secret)["']?\s*[:=]\s*(?:bearer\s+)?["']?)[^"',;\s}\]]+"#,
        )
        .expect("敏感字段脱敏正则必须有效")
    });
    let sk_token = SK_TOKEN.get_or_init(|| {
        Regex::new(r"\bsk-[A-Za-z0-9_-]{12,}\b").expect("sk token 脱敏正则必须有效")
    });
    let redacted = sensitive_value
        .replace_all(raw, "${1}[REDACTED]")
        .into_owned();
    sk_token.replace_all(&redacted, "[REDACTED]").into_owned()
}

fn escape_control_chars(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                write!(&mut escaped, "\\u{{{:x}}}", u32::from(character))
                    .expect("写入 String 不应失败");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn bounded_parse_error(error: &str) -> String {
    bounded_sensitive_text(error, 512)
}

fn bounded_sensitive_text(value: &str, max_chars: usize) -> String {
    escape_control_chars(&redact_writer_output(value))
        .chars()
        .take(max_chars)
        .collect()
}

pub struct ScopedNovelResourceAdapter {
    workspace_root: PathBuf,
}

impl ScopedNovelResourceAdapter {
    pub fn new(workspace_root: impl AsRef<Path>) -> novel_application::Result<Self> {
        let root = std::fs::canonicalize(workspace_root.as_ref()).map_err(resource_error)?;
        if !root.is_dir() {
            return Err(NovelApplicationError::ResourceDenied(format!(
                "小说项目工作区不是目录: {}",
                root.display()
            )));
        }
        Ok(Self {
            workspace_root: root,
        })
    }

    fn resolve_scoped(
        &self,
        path: &Path,
        require_file: bool,
    ) -> novel_application::Result<PathBuf> {
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(NovelApplicationError::ResourceDenied(format!(
                "路径不允许包含父目录跳转: {}",
                path.display()
            )));
        }
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        };
        let resolved = resolve_existing_ancestor(&candidate)?;
        if !resolved.starts_with(&self.workspace_root) {
            return Err(NovelApplicationError::ResourceDenied(format!(
                "路径超出小说项目工作区: {}",
                path.display()
            )));
        }
        if require_file && !resolved.is_file() {
            return Err(NovelApplicationError::Resource(format!(
                "小说上下文文件不存在: {}",
                resolved.display()
            )));
        }
        Ok(resolved)
    }
}

#[async_trait]
impl NovelResourcePort for ScopedNovelResourceAdapter {
    async fn read_context(
        &self,
        reference: &ContextRef,
    ) -> novel_application::Result<NovelContextDocument> {
        let path = self.resolve_scoped(&reference.canonical_path, true)?;
        let content = std::fs::read_to_string(&path).map_err(resource_error)?;
        let actual_hash = sha256(content.as_bytes());
        if !actual_hash.eq_ignore_ascii_case(reference.sha256.trim()) {
            return Err(NovelApplicationError::ContextChanged(format!(
                "{} 的内容 hash 已变化: expected={}, actual={actual_hash}",
                path.display(),
                reference.sha256
            )));
        }
        let mut normalized = reference.clone();
        normalized.canonical_path = path;
        normalized.sha256 = actual_hash;
        Ok(NovelContextDocument {
            reference: normalized,
            content,
        })
    }

    async fn resolve_artifact_path(&self, path: &Path) -> novel_application::Result<PathBuf> {
        self.resolve_scoped(path, false)
    }

    async fn write_artifact_atomic(
        &self,
        path: &Path,
        exact_content: &str,
    ) -> novel_application::Result<NovelArtifactReceipt> {
        let path = self.resolve_scoped(path, false)?;
        let parent = path.parent().ok_or_else(|| {
            NovelApplicationError::ResourceDenied(format!("输出路径没有父目录: {}", path.display()))
        })?;
        std::fs::create_dir_all(parent).map_err(resource_error)?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(resource_error)?;
        if !canonical_parent.starts_with(&self.workspace_root) {
            return Err(NovelApplicationError::ResourceDenied(format!(
                "输出目录超出小说项目工作区: {}",
                parent.display()
            )));
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                NovelApplicationError::ResourceDenied(format!("输出文件名无效: {}", path.display()))
            })?
            .to_string();
        let path = canonical_parent.join(&file_name);
        let expected_sha256 = sha256(exact_content.as_bytes());
        if path.exists() {
            return existing_artifact_receipt(&path, &expected_sha256);
        }
        let temp = canonical_parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
        let write_result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(exact_content.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::hard_link(&temp, &path)?;
            let _ = std::fs::remove_file(&temp);
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&temp);
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                return existing_artifact_receipt(&path, &expected_sha256);
            }
            return Err(resource_error(error));
        }
        existing_artifact_receipt(&path, &expected_sha256)
    }

    async fn verify_artifact(
        &self,
        path: &Path,
        expected_sha256: &str,
    ) -> novel_application::Result<Option<NovelArtifactReceipt>> {
        let path = self.resolve_scoped(path, false)?;
        if !path.is_file() {
            return Ok(None);
        }
        existing_artifact_receipt(&path, expected_sha256).map(Some)
    }
}

fn existing_artifact_receipt(
    path: &Path,
    expected_sha256: &str,
) -> novel_application::Result<NovelArtifactReceipt> {
    let content = std::fs::read(path).map_err(resource_error)?;
    let actual_hash = sha256(&content);
    if !actual_hash.eq_ignore_ascii_case(expected_sha256) {
        return Err(NovelApplicationError::ContextChanged(format!(
            "作品文件 {} 已存在且 hash 不匹配，拒绝覆盖: expected={expected_sha256}, actual={actual_hash}",
            path.display()
        )));
    }
    Ok(NovelArtifactReceipt {
        canonical_path: path.to_string_lossy().into_owned(),
        sha256: actual_hash,
        bytes: content.len() as u64,
        written_at: std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(
                || chrono::Utc::now().timestamp_millis(),
                |duration| duration.as_millis() as i64,
            ),
    })
}

fn resolve_existing_ancestor(path: &Path) -> novel_application::Result<PathBuf> {
    if path.exists() {
        return std::fs::canonicalize(path).map_err(resource_error);
    }
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            NovelApplicationError::ResourceDenied(format!("无法解析路径: {}", path.display()))
        })?;
        missing.push(name.to_os_string());
        if !ancestor.pop() {
            return Err(NovelApplicationError::ResourceDenied(format!(
                "无法解析路径: {}",
                path.display()
            )));
        }
    }
    let mut resolved = std::fs::canonicalize(ancestor).map_err(resource_error)?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn resource_error(error: impl std::fmt::Display) -> NovelApplicationError {
    NovelApplicationError::Resource(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;

    use brain_llm::{ChatResponse, ContentBlock, FinishReason, LlmError, MessageRole, TokenUsage};
    use knowledge_core::ContextSnapshot;
    use novel_domain::{
        ContextRole, NovelOutcome, NovelProject, NovelTaskRequest, NovelTaskType, PublicationPolicy,
    };
    use novel_workflow::{writer_profile, NovelWorkflowBudget, NovelWriterPort, ProfileModel};

    use super::*;

    struct RecordingLlm {
        requests: std::sync::Mutex<Vec<ChatRequest>>,
        responses: std::sync::Mutex<VecDeque<brain_llm::Result<ChatResponse>>>,
    }

    impl RecordingLlm {
        fn new(responses: Vec<brain_llm::Result<ChatResponse>>) -> Self {
            Self {
                requests: std::sync::Mutex::new(Vec::new()),
                responses: std::sync::Mutex::new(responses.into()),
            }
        }
    }

    impl LlmProvider for RecordingLlm {
        fn model(&self) -> &str {
            "test-model"
        }

        fn complete(
            &self,
            request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            self.requests.lock().unwrap().push(request);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("RecordingLlm 响应队列已耗尽");
            Box::pin(async move { response })
        }
    }

    fn response(
        content: impl Into<String>,
        prompt_tokens: u64,
        completion_tokens: u64,
        finish_reason: FinishReason,
    ) -> ChatResponse {
        ChatResponse {
            content: vec![ContentBlock::text(content)],
            model: "test-model".into(),
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
                ..TokenUsage::default()
            },
            finish_reason: Some(finish_reason),
        }
    }

    fn clarification_response(prompt_tokens: u64, completion_tokens: u64) -> ChatResponse {
        response(
            r#"{"outcome":"needs_clarification","questions":["Which ending?"],"reason":"ending is unspecified"}"#,
            prompt_tokens,
            completion_tokens,
            FinishReason::EndTurn,
        )
    }

    fn workflow_request() -> NovelTaskRequest {
        NovelTaskRequest {
            task_id: "task-1".into(),
            project_id: "project-1".into(),
            task_type: NovelTaskType::Body,
            task_brief: "Write chapter one".into(),
            target_chapter: Some(1),
            expected_revision: 0,
            output_path: PathBuf::from("chapters/0001.md"),
            context_refs: Vec::new(),
            must_happen: Vec::new(),
            must_not_change: Vec::new(),
            acceptance_criteria: vec!["complete chapter".into()],
            allow_web_research: false,
            publication_policy: PublicationPolicy::RequireUserAcceptance,
            parent_task_id: None,
            source_conversation_id: None,
            source_generation_id: None,
        }
    }

    #[tokio::test]
    async fn writer_uses_frozen_context_without_tools_and_preserves_provider_usage() {
        let llm = Arc::new(RecordingLlm::new(vec![Ok(clarification_response(111, 22))]));
        let adapter = LlmNovelWriterAdapter::new(llm.clone(), 0.25);
        let execution = adapter
            .execute(NovelWriterInvocation {
                request: workflow_request(),
                project: NovelProject::new("project-1", "Project"),
                context_snapshot: ContextSnapshot::from_text(
                    "novel-context-task-1",
                    "frozen task input",
                )
                .unwrap(),
                context_documents: Vec::new(),
                next_draft_version: 1,
                profile: writer_profile().unwrap(),
                model: ProfileModel::new("test-provider", "test-model"),
                budget: NovelWorkflowBudget {
                    input_tokens: 1_000,
                    output_tokens: 321,
                },
            })
            .await
            .unwrap();
        assert_eq!(execution.usage.input_tokens, 111);
        assert_eq!(execution.usage.output_tokens, 22);
        assert!(matches!(
            execution.outcome,
            NovelOutcome::NeedsClarification(_)
        ));
        let request = llm.requests.lock().unwrap().pop().unwrap();
        assert_eq!(request.model.as_deref(), Some("test-model"));
        assert_eq!(request.max_tokens, Some(321));
        assert_eq!(request.temperature, Some(0.25));
        assert!(request.tools.is_none());
        assert!(request
            .messages
            .last()
            .unwrap()
            .text_content()
            .contains("frozen task input"));
        let system_prompt = request.messages.first().unwrap().text_content();
        for required in [
            "novel.writer-output.v1",
            "draft_ready shape",
            "needs_clarification shape",
            "outline_alignment",
            "experience_candidates",
            "project-1",
            "chapters/0001.md",
            "\"expected_revision\": 0",
            "\"task_type\": \"body\"",
        ] {
            assert!(
                system_prompt.contains(required),
                "system prompt missing {required}"
            );
        }
    }

    fn writer_invocation(output_tokens: u64) -> NovelWriterInvocation {
        NovelWriterInvocation {
            request: workflow_request(),
            project: NovelProject::new("project-1", "Project"),
            context_snapshot: ContextSnapshot::from_text(
                "novel-context-task-1",
                "frozen task input",
            )
            .unwrap(),
            context_documents: Vec::new(),
            next_draft_version: 1,
            profile: writer_profile().unwrap(),
            model: ProfileModel::new("test-provider", "test-model"),
            budget: NovelWorkflowBudget {
                input_tokens: 1_000,
                output_tokens,
            },
        }
    }

    #[tokio::test]
    async fn writer_repairs_invalid_shape_once_on_same_model_and_sums_usage() {
        let first_raw = r#"{"summary":"正文已生成"}"#;
        let llm = Arc::new(RecordingLlm::new(vec![
            Ok(response(first_raw, 100, 20, FinishReason::EndTurn)),
            Ok(clarification_response(30, 7)),
        ]));
        let adapter = LlmNovelWriterAdapter::new(llm.clone(), 0.25);

        let execution = adapter
            .execute(writer_invocation(321))
            .await
            .expect("格式纠正后应成功");

        assert_eq!(execution.usage.input_tokens, 130);
        assert_eq!(execution.usage.output_tokens, 27);
        let requests = llm.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].model, requests[1].model);
        assert_eq!(requests[1].max_tokens, Some(301));
        assert!(requests[1].messages.iter().any(|message| {
            message.role == MessageRole::Assistant && message.text_content().contains(first_raw)
        }));
    }

    #[tokio::test]
    async fn writer_does_not_repair_provider_failure() {
        let provider_secret = "provider-super-secret";
        let llm = Arc::new(RecordingLlm::new(vec![Err(LlmError::ApiError {
            status: 503,
            message: format!("model_not_found: No available channel; api_key={provider_secret}"),
        })]));
        let adapter = LlmNovelWriterAdapter::new(llm.clone(), 0.25);

        let error = adapter.execute(writer_invocation(321)).await.unwrap_err();
        let message = error.to_string();

        assert_eq!(llm.requests.lock().unwrap().len(), 1);
        assert!(message.contains("provider=test-provider"), "{message}");
        assert!(message.contains("model=test-model"), "{message}");
        assert!(message.contains("model_not_found"), "{message}");
        assert!(!message.contains(provider_secret), "{message}");
    }

    #[tokio::test]
    async fn writer_stops_after_second_invalid_shape_with_redacted_bounded_diagnostic() {
        let bearer = "bearer-super-secret";
        let api_key = "api-super-secret";
        let token = "token-super-secret";
        let secret = "value-super-secret";
        let sk_token = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let second_raw = format!(
            "Authorization: Bearer {bearer}\n{{\"api_key\":\"{api_key}\",\"token\":\"{token}\",\"secret\":\"{secret}\",\"payload\":\"{} {sk_token}\"}}\0",
            "文".repeat(3_000)
        );
        let llm = Arc::new(RecordingLlm::new(vec![
            Ok(response(
                r#"{"summary":"first invalid"}"#,
                10,
                5,
                FinishReason::EndTurn,
            )),
            Ok(response(&second_raw, 20, 6, FinishReason::MaxTokens)),
        ]));
        let adapter = LlmNovelWriterAdapter::new(llm.clone(), 0.25);

        let message = adapter
            .execute(writer_invocation(321))
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(llm.requests.lock().unwrap().len(), 2);
        assert!(message.contains("attempt=2/2"), "{message}");
        assert!(message.contains("provider=test-provider"), "{message}");
        assert!(message.contains("model=test-model"), "{message}");
        assert!(message.contains("finish_reason=max_tokens"), "{message}");
        assert!(message.contains("raw_sha256="), "{message}");
        for sensitive in [bearer, api_key, token, secret, sk_token] {
            assert!(!message.contains(sensitive), "诊断泄漏敏感值: {sensitive}");
        }
        assert!(message.chars().count() < 2_600, "诊断未限长");
    }

    #[tokio::test]
    async fn scoped_resources_validate_hash_scope_and_atomic_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let context_path = dir.path().join("outline.md");
        std::fs::write(&context_path, "章纲内容").unwrap();
        let adapter = ScopedNovelResourceAdapter::new(dir.path()).unwrap();
        let reference = ContextRef {
            role: ContextRole::ChapterOutline,
            canonical_path: context_path.clone(),
            sha256: sha256("章纲内容".as_bytes()),
            description: None,
        };

        let document = adapter.read_context(&reference).await.unwrap();
        assert_eq!(document.content, "章纲内容");
        let receipt = adapter
            .write_artifact_atomic(Path::new("chapters/0001.md"), "第一章正文")
            .await
            .unwrap();
        assert_eq!(receipt.sha256, sha256("第一章正文".as_bytes()));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("chapters/0001.md")).unwrap(),
            "第一章正文"
        );
        let retry = adapter
            .write_artifact_atomic(Path::new("chapters/0001.md"), "第一章正文")
            .await
            .unwrap();
        assert_eq!(retry.sha256, receipt.sha256);
        assert!(matches!(
            adapter
                .write_artifact_atomic(Path::new("chapters/0001.md"), "不同正文")
                .await
                .unwrap_err(),
            NovelApplicationError::ContextChanged(_)
        ));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("chapters/0001.md")).unwrap(),
            "第一章正文"
        );
        assert!(adapter
            .verify_artifact(Path::new(&receipt.canonical_path), &receipt.sha256)
            .await
            .unwrap()
            .is_some());
        std::fs::write(&receipt.canonical_path, "外部修改").unwrap();
        assert!(matches!(
            adapter
                .verify_artifact(Path::new(&receipt.canonical_path), &receipt.sha256)
                .await
                .unwrap_err(),
            NovelApplicationError::ContextChanged(_)
        ));
    }

    #[tokio::test]
    async fn scoped_resources_reject_changed_context_and_escape() {
        let dir = tempfile::tempdir().unwrap();
        let context_path = dir.path().join("outline.md");
        std::fs::write(&context_path, "已变化内容").unwrap();
        let adapter = ScopedNovelResourceAdapter::new(dir.path()).unwrap();
        let reference = ContextRef {
            role: ContextRole::ChapterOutline,
            canonical_path: context_path,
            sha256: sha256("旧内容".as_bytes()),
            description: None,
        };
        assert!(matches!(
            adapter.read_context(&reference).await.unwrap_err(),
            NovelApplicationError::ContextChanged(_)
        ));
        assert!(matches!(
            adapter
                .resolve_artifact_path(Path::new("../outside.md"))
                .await
                .unwrap_err(),
            NovelApplicationError::ResourceDenied(_)
        ));
    }
}
