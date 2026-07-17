use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use brain_memory::novel::{
    CommitReport, ConsistencyReport, NovelArtifactReceipt, NovelPublicationRecord, NovelRecallPack,
    NovelTaskCheckpoint, NovelTaskEvent, NovelTaskType,
};
use brain_memory::pyramid_memory_brain::PyramidMemoryBrain;
use brain_novel::{
    ContextDocument, ContextRef, NovelMemoryPort, NovelPortError, NovelResourcePort,
    NovelWorkspaceSnapshot,
};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

pub struct PyramidNovelMemoryAdapter {
    memory: Arc<Mutex<PyramidMemoryBrain>>,
}

impl PyramidNovelMemoryAdapter {
    #[must_use]
    pub fn new(memory: Arc<Mutex<PyramidMemoryBrain>>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl NovelMemoryPort for PyramidNovelMemoryAdapter {
    async fn load_workspace(
        &self,
        project_id: &str,
    ) -> Result<NovelWorkspaceSnapshot, NovelPortError> {
        let memory = self.memory.lock().await;
        let project = memory
            .load_novel_project(project_id)
            .map_err(memory_error)?;
        let active_checkpoint = memory
            .active_novel_task_for_project(project_id)
            .map_err(memory_error)?;
        Ok(NovelWorkspaceSnapshot {
            project,
            active_checkpoint,
        })
    }

    async fn active_checkpoints(&self) -> Result<Vec<NovelTaskCheckpoint>, NovelPortError> {
        let memory = self.memory.lock().await;
        let projects = memory.list_novel_projects().map_err(memory_error)?;
        let mut checkpoints = Vec::new();
        for project in projects {
            if let Some(checkpoint) = memory
                .active_novel_task_for_project(&project.project_id)
                .map_err(memory_error)?
            {
                checkpoints.push(checkpoint);
            }
        }
        Ok(checkpoints)
    }

    async fn load_checkpoint(
        &self,
        task_id: &str,
    ) -> Result<Option<NovelTaskCheckpoint>, NovelPortError> {
        self.memory
            .lock()
            .await
            .load_novel_task_checkpoint(task_id)
            .map_err(memory_error)
    }

    async fn recall_project(
        &self,
        project_id: &str,
        task_type: NovelTaskType,
    ) -> Result<NovelRecallPack, NovelPortError> {
        self.memory
            .lock()
            .await
            .recall_novel_project(project_id, task_type)
            .map_err(memory_error)
    }

    async fn check_consistency(
        &self,
        project_id: &str,
    ) -> Result<ConsistencyReport, NovelPortError> {
        self.memory
            .lock()
            .await
            .check_novel_consistency(project_id)
            .map_err(memory_error)
    }

    async fn append_task_event(&self, event: NovelTaskEvent) -> Result<(), NovelPortError> {
        self.memory
            .lock()
            .await
            .append_novel_task_event(&event)
            .map_err(memory_error)
    }

    async fn save_checkpoint(&self, checkpoint: NovelTaskCheckpoint) -> Result<(), NovelPortError> {
        self.memory
            .lock()
            .await
            .save_novel_task_checkpoint(&checkpoint)
            .map_err(memory_error)
    }

    async fn begin_publication(
        &self,
        record: NovelPublicationRecord,
    ) -> Result<(), NovelPortError> {
        self.memory
            .lock()
            .await
            .begin_novel_publication(&record)
            .map_err(memory_error)
    }

    async fn load_publication(
        &self,
        publication_id: &str,
    ) -> Result<NovelPublicationRecord, NovelPortError> {
        self.memory
            .lock()
            .await
            .load_novel_publication(publication_id)
            .map_err(memory_error)
    }

    async fn complete_publication(
        &self,
        publication_id: &str,
        artifact: NovelArtifactReceipt,
    ) -> Result<CommitReport, NovelPortError> {
        self.memory
            .lock()
            .await
            .complete_novel_publication(publication_id, artifact)
            .map_err(memory_error)
    }

    async fn abort_publication(
        &self,
        publication_id: &str,
        reason: &str,
    ) -> Result<(), NovelPortError> {
        self.memory
            .lock()
            .await
            .abort_novel_publication(publication_id, reason)
            .map_err(memory_error)
    }

    async fn pending_publications(&self) -> Result<Vec<NovelPublicationRecord>, NovelPortError> {
        self.memory
            .lock()
            .await
            .pending_novel_publications()
            .map_err(memory_error)
    }
}

pub struct ScopedNovelResourceAdapter {
    workspace_root: PathBuf,
}

impl ScopedNovelResourceAdapter {
    pub fn new(workspace_root: impl AsRef<Path>) -> Result<Self, NovelPortError> {
        let root = std::fs::canonicalize(workspace_root.as_ref()).map_err(resource_error)?;
        if !root.is_dir() {
            return Err(NovelPortError::ResourceDenied(format!(
                "小说项目工作区不是目录: {}",
                root.display()
            )));
        }
        Ok(Self {
            workspace_root: root,
        })
    }

    fn resolve_scoped(&self, path: &Path, require_file: bool) -> Result<PathBuf, NovelPortError> {
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(NovelPortError::ResourceDenied(format!(
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
            return Err(NovelPortError::ResourceDenied(format!(
                "路径超出小说项目工作区: {}",
                path.display()
            )));
        }
        if require_file && !resolved.is_file() {
            return Err(NovelPortError::Resource(format!(
                "小说上下文文件不存在: {}",
                resolved.display()
            )));
        }
        Ok(resolved)
    }
}

#[async_trait]
impl NovelResourcePort for ScopedNovelResourceAdapter {
    async fn resolve_artifact_path(&self, path: &Path) -> Result<PathBuf, NovelPortError> {
        self.resolve_scoped(path, false)
    }

    async fn read_context(
        &self,
        reference: &ContextRef,
    ) -> Result<ContextDocument, NovelPortError> {
        let path = self.resolve_scoped(&reference.canonical_path, true)?;
        let content = std::fs::read_to_string(&path).map_err(resource_error)?;
        let actual_hash = sha256(content.as_bytes());
        if !actual_hash.eq_ignore_ascii_case(reference.sha256.trim()) {
            return Err(NovelPortError::ContextChanged(format!(
                "{} 的内容 hash 已变化: expected={}, actual={actual_hash}",
                path.display(),
                reference.sha256
            )));
        }
        let mut normalized = reference.clone();
        normalized.canonical_path = path;
        normalized.sha256 = actual_hash;
        Ok(ContextDocument {
            reference: normalized,
            content,
        })
    }

    async fn write_artifact_atomic(
        &self,
        path: &Path,
        exact_content: &str,
    ) -> Result<NovelArtifactReceipt, NovelPortError> {
        let path = self.resolve_scoped(path, false)?;
        let parent = path.parent().ok_or_else(|| {
            NovelPortError::ResourceDenied(format!("输出路径没有父目录: {}", path.display()))
        })?;
        std::fs::create_dir_all(parent).map_err(resource_error)?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(resource_error)?;
        if !canonical_parent.starts_with(&self.workspace_root) {
            return Err(NovelPortError::ResourceDenied(format!(
                "输出目录超出小说项目工作区: {}",
                parent.display()
            )));
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                NovelPortError::ResourceDenied(format!("输出文件名无效: {}", path.display()))
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
    ) -> Result<Option<NovelArtifactReceipt>, NovelPortError> {
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
) -> Result<NovelArtifactReceipt, NovelPortError> {
    let content = std::fs::read(path).map_err(resource_error)?;
    let actual_hash = sha256(&content);
    if !actual_hash.eq_ignore_ascii_case(expected_sha256) {
        return Err(NovelPortError::ContextChanged(format!(
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

fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf, NovelPortError> {
    if path.exists() {
        return std::fs::canonicalize(path).map_err(resource_error);
    }
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            NovelPortError::ResourceDenied(format!("无法解析路径: {}", path.display()))
        })?;
        missing.push(name.to_os_string());
        if !ancestor.pop() {
            return Err(NovelPortError::ResourceDenied(format!(
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

fn memory_error(error: impl std::fmt::Display) -> NovelPortError {
    NovelPortError::Memory(error.to_string())
}

fn resource_error(error: impl std::fmt::Display) -> NovelPortError {
    NovelPortError::Resource(error.to_string())
}

#[cfg(test)]
mod tests {
    use brain_novel::{ContextRole, NovelResourcePort};

    use super::*;

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
            NovelPortError::ContextChanged(_)
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
            NovelPortError::ContextChanged(_)
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
            NovelPortError::ContextChanged(_)
        ));
        assert!(matches!(
            adapter
                .resolve_artifact_path(Path::new("../outside.md"))
                .await
                .unwrap_err(),
            NovelPortError::ResourceDenied(_)
        ));
    }
}
