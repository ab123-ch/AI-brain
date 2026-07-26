use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use brain_llm::{ChatMessage, ChatRequest, LlmProvider};
use novel_application::{NovelApplicationError, NovelResourcePort};
use novel_domain::{ContextRef, NovelArtifactReceipt};
use novel_workflow::{
    parse_novel_response, NovelContextDocument, NovelWorkflowPortError, NovelWriterExecution,
    NovelWriterInvocation, NovelWriterPort,
};
use sha2::{Digest, Sha256};
use task_engine::ActualUsage;

const NOVEL_WRITING_WORKFLOW: &str =
    include_str!("../../brain-main/skills/novel-writing-workflow/SKILL.md");

pub struct LlmNovelWriterAdapter {
    llm: Arc<dyn LlmProvider>,
    temperature: f64,
}

impl LlmNovelWriterAdapter {
    #[must_use]
    pub fn new(llm: Arc<dyn LlmProvider>, temperature: f64) -> Self {
        Self { llm, temperature }
    }
}

#[async_trait]
impl NovelWriterPort for LlmNovelWriterAdapter {
    async fn execute(
        &self,
        invocation: NovelWriterInvocation,
    ) -> Result<NovelWriterExecution, NovelWorkflowPortError> {
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
        let system_prompt = format!(
            "{}\n\
             Return exactly one JSON object matching novel.writer-output.v1. Never publish, write Canon, Memory, Graph, or files.\n\
             draft_ready requires content, a passing six-check self_review, proposed_delta, and evidence_refs.\n\
             needs_clarification requires non-empty questions and a reason.\n\n\
             <novel-writing-workflow>\n{NOVEL_WRITING_WORKFLOW}\n</novel-writing-workflow>",
            invocation.profile.system_prompt.join("\n")
        );
        let response = self
            .llm
            .complete(ChatRequest {
                model: Some(invocation.model.model.clone()),
                messages: vec![
                    ChatMessage::system(system_prompt),
                    ChatMessage::user(invocation.context_snapshot.render()),
                ],
                max_tokens: Some(max_tokens),
                temperature: Some(self.temperature),
                tools: None,
                tool_choice: None,
            })
            .await
            .map_err(|error| {
                NovelWorkflowPortError::Storage(format!("Novel Writer Provider failed: {error}"))
            })?;
        let raw_output = response.text();
        if raw_output.trim().is_empty() {
            return Err(NovelWorkflowPortError::InvalidWriterOutput(
                "Novel Writer Provider returned no text".into(),
            ));
        }
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
        let outcome = parse_novel_response(
            &raw_output,
            &invocation.request.task_id,
            &invocation.request.project_id,
            invocation.next_draft_version,
            invocation.request.expected_revision,
            &evidence_refs,
        )
        .map_err(|error| NovelWorkflowPortError::InvalidWriterOutput(error.to_string()))?;
        Ok(NovelWriterExecution {
            outcome,
            raw_output,
            usage: ActualUsage {
                input_tokens: response.usage.prompt_tokens,
                output_tokens: response.usage.completion_tokens,
            },
        })
    }
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
    use std::future::Future;
    use std::pin::Pin;

    use brain_llm::{ChatResponse, ContentBlock, FinishReason, TokenUsage};
    use knowledge_core::ContextSnapshot;
    use novel_domain::{
        ContextRole, NovelOutcome, NovelProject, NovelTaskRequest, NovelTaskType, PublicationPolicy,
    };
    use novel_workflow::{writer_profile, NovelWorkflowBudget, NovelWriterPort, ProfileModel};

    use super::*;

    struct RecordingLlm {
        request: std::sync::Mutex<Option<ChatRequest>>,
    }

    impl LlmProvider for RecordingLlm {
        fn model(&self) -> &str {
            "test-model"
        }

        fn complete(
            &self,
            request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            *self.request.lock().unwrap() = Some(request);
            Box::pin(async {
                Ok(ChatResponse {
                    content: vec![ContentBlock::text(
                        r#"{"outcome":"needs_clarification","questions":["Which ending?"],"reason":"ending is unspecified"}"#,
                    )],
                    model: "test-model".into(),
                    usage: TokenUsage {
                        prompt_tokens: 111,
                        completion_tokens: 22,
                        total_tokens: 133,
                        ..TokenUsage::default()
                    },
                    finish_reason: Some(FinishReason::EndTurn),
                })
            })
        }
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
        let llm = Arc::new(RecordingLlm {
            request: std::sync::Mutex::new(None),
        });
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
        let request = llm.request.lock().unwrap().take().unwrap();
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
