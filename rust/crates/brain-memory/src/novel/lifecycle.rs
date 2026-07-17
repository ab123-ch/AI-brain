use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use super::{NovelPublicationRecord, NovelPublicationStatus, NovelTaskCheckpoint, NovelTaskEvent};
use crate::error::{MemoryError, Result};

pub(crate) struct NovelLifecycleStore {
    root: PathBuf,
}

impl NovelLifecycleStore {
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: base_dir.into().join("novel").join("lifecycle"),
        }
    }

    pub fn append_event(&self, event: &NovelTaskEvent) -> Result<()> {
        validate_id("task_id", &event.task_id)?;
        validate_id("project_id", &event.project_id)?;
        let path = self.event_path(&event.task_id);
        ensure_parent(&path)?;
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(line.as_bytes())?;
        Ok(())
    }

    pub fn load_events(&self, task_id: &str) -> Result<Vec<NovelTaskEvent>> {
        validate_id("task_id", task_id)?;
        let path = self.event_path(task_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = fs::read_to_string(path)?;
        data.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    pub fn save_checkpoint(&self, checkpoint: &NovelTaskCheckpoint) -> Result<()> {
        validate_id("task_id", &checkpoint.task_id)?;
        validate_id("project_id", &checkpoint.project_id)?;
        write_json_atomic(&self.checkpoint_path(&checkpoint.task_id), checkpoint)
    }

    pub fn load_checkpoint(&self, task_id: &str) -> Result<Option<NovelTaskCheckpoint>> {
        validate_id("task_id", task_id)?;
        read_optional_json(&self.checkpoint_path(task_id))
    }

    pub fn active_checkpoint_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<NovelTaskCheckpoint>> {
        validate_id("project_id", project_id)?;
        let dir = self.root.join("checkpoints");
        if !dir.exists() {
            return Ok(None);
        }
        let mut candidates = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let checkpoint: NovelTaskCheckpoint = serde_json::from_str(&fs::read_to_string(path)?)?;
            if checkpoint.project_id == project_id && !checkpoint.phase.is_terminal() {
                candidates.push(checkpoint);
            }
        }
        candidates.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(candidates.into_iter().next())
    }

    pub fn begin_publication(&self, record: &NovelPublicationRecord) -> Result<()> {
        validate_id("publication_id", &record.publication_id)?;
        validate_id("task_id", &record.task_id)?;
        validate_id("project_id", &record.project_id)?;
        if record.status != NovelPublicationStatus::Pending {
            return Err(MemoryError::Conflict(
                "new publication record must be pending".into(),
            ));
        }
        if self.publication_path(&record.publication_id).exists() {
            return Err(MemoryError::Conflict(format!(
                "小说发布事务已存在: {}",
                record.publication_id
            )));
        }
        if self
            .pending_publications()?
            .iter()
            .any(|item| item.project_id == record.project_id)
        {
            return Err(MemoryError::Conflict(format!(
                "项目 {} 已有未完成的发布事务",
                record.project_id
            )));
        }
        write_json_atomic(&self.publication_path(&record.publication_id), record)
    }

    pub fn load_publication(&self, publication_id: &str) -> Result<NovelPublicationRecord> {
        validate_id("publication_id", publication_id)?;
        read_optional_json(&self.publication_path(publication_id))?
            .ok_or_else(|| MemoryError::NotFound(format!("小说发布事务不存在: {publication_id}")))
    }

    pub fn save_publication(&self, record: &NovelPublicationRecord) -> Result<()> {
        validate_id("publication_id", &record.publication_id)?;
        write_json_atomic(&self.publication_path(&record.publication_id), record)
    }

    pub fn pending_publications(&self) -> Result<Vec<NovelPublicationRecord>> {
        let dir = self.root.join("publications");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let record: NovelPublicationRecord = serde_json::from_str(&fs::read_to_string(path)?)?;
            if matches!(
                record.status,
                NovelPublicationStatus::Pending | NovelPublicationStatus::ArtifactSaved
            ) {
                records.push(record);
            }
        }
        records.sort_by_key(|record| record.created_at);
        Ok(records)
    }

    fn checkpoint_path(&self, task_id: &str) -> PathBuf {
        self.root
            .join("checkpoints")
            .join(format!("{task_id}.json"))
    }

    fn event_path(&self, task_id: &str) -> PathBuf {
        self.root.join("events").join(format!("{task_id}.jsonl"))
    }

    fn publication_path(&self, publication_id: &str) -> PathBuf {
        self.root
            .join("publications")
            .join(format!("{publication_id}.json"))
    }
}

fn validate_id(field: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(MemoryError::Conflict(format!(
            "{field} 只能包含字母、数字、-、_: {value}"
        )))
    }
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    ensure_parent(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| MemoryError::PathNotFound(path.to_path_buf()))?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(&serde_json::to_vec_pretty(value)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn read_optional_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&fs::read_to_string(path)?)?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::novel::{
        NovelLifecycleActor, NovelMemoryDelta, NovelPublicationRecord, NovelTaskPhase,
        NovelTaskType,
    };

    fn checkpoint(
        task_id: &str,
        project_id: &str,
        phase: NovelTaskPhase,
        updated_at: i64,
    ) -> NovelTaskCheckpoint {
        NovelTaskCheckpoint {
            task_id: task_id.into(),
            project_id: project_id.into(),
            phase,
            draft_version: 1,
            canon_revision: 0,
            output_path: "chapters/0001.md".into(),
            state: json!({"summary": "working"}),
            updated_at,
        }
    }

    fn publication(id: &str, project_id: &str) -> NovelPublicationRecord {
        NovelPublicationRecord::pending(
            id,
            format!("task-{id}"),
            1,
            format!("chapters/{id}.md"),
            format!("sha256-{id}"),
            NovelMemoryDelta {
                project_id: project_id.into(),
                branch_id: "main".into(),
                expected_revision: 0,
                task_type: NovelTaskType::Body,
                source_ref: format!("chapters/{id}.md"),
                progress: None,
                proposed_facts: Vec::new(),
                state_changes: Vec::new(),
                plot_updates: Vec::new(),
                foreshadowing_updates: Vec::new(),
                feedback: Vec::new(),
                experience_candidates: Vec::new(),
            },
        )
    }

    #[test]
    fn task_event_and_checkpoint_roundtrip() {
        let dir = tempdir().unwrap();
        let store = NovelLifecycleStore::new(dir.path());
        let event = NovelTaskEvent {
            event_id: "event-1".into(),
            task_id: "task-1".into(),
            project_id: "project-1".into(),
            actor: NovelLifecycleActor::Main,
            phase: NovelTaskPhase::Preparing,
            summary: "任务合同已创建".into(),
            details: json!({"expected_revision": 0}),
            created_at: 10,
        };
        let saved_checkpoint = checkpoint(
            "task-1",
            "project-1",
            NovelTaskPhase::AwaitingMainReview,
            20,
        );

        store.append_event(&event).unwrap();
        store.save_checkpoint(&saved_checkpoint).unwrap();

        let events = store.load_events("task-1").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, "event-1");
        assert_eq!(events[0].details["expected_revision"], 0);
        let loaded = store.load_checkpoint("task-1").unwrap().unwrap();
        assert_eq!(loaded.phase, NovelTaskPhase::AwaitingMainReview);
        assert_eq!(loaded.state["summary"], "working");

        let replacement = checkpoint("task-1", "project-1", NovelTaskPhase::Completed, 30);
        store.save_checkpoint(&replacement).unwrap();
        let loaded = store.load_checkpoint("task-1").unwrap().unwrap();
        assert_eq!(loaded.phase, NovelTaskPhase::Completed);
        assert_eq!(loaded.updated_at, 30);
    }

    #[test]
    fn active_checkpoint_filters_terminal_tasks_and_returns_latest() {
        let dir = tempdir().unwrap();
        let store = NovelLifecycleStore::new(dir.path());
        store
            .save_checkpoint(&checkpoint(
                "task-old",
                "project-1",
                NovelTaskPhase::Drafting,
                10,
            ))
            .unwrap();
        store
            .save_checkpoint(&checkpoint(
                "task-complete",
                "project-1",
                NovelTaskPhase::Completed,
                30,
            ))
            .unwrap();
        store
            .save_checkpoint(&checkpoint(
                "task-current",
                "project-1",
                NovelTaskPhase::AwaitingUserDecision,
                20,
            ))
            .unwrap();

        let active = store
            .active_checkpoint_for_project("project-1")
            .unwrap()
            .unwrap();
        assert_eq!(active.task_id, "task-current");
        assert_eq!(active.phase, NovelTaskPhase::AwaitingUserDecision);
    }

    #[test]
    fn project_allows_only_one_pending_publication() {
        let dir = tempdir().unwrap();
        let store = NovelLifecycleStore::new(dir.path());
        let first = publication("publication-1", "project-1");
        let second = publication("publication-2", "project-1");

        store.begin_publication(&first).unwrap();
        let error = store.begin_publication(&second).unwrap_err();
        assert!(error.to_string().contains("已有未完成"));

        let mut first = store.load_publication("publication-1").unwrap();
        first.status = NovelPublicationStatus::Aborted;
        store.save_publication(&first).unwrap();
        store.begin_publication(&second).unwrap();
        assert_eq!(store.pending_publications().unwrap().len(), 1);
    }
}
