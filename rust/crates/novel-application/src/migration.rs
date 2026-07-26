use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use novel_domain::{NovelProject, NovelPublicationRecord, NovelTaskCheckpoint, NovelTaskEvent};

use crate::{NovelApplicationError, NovelDomainStore, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub projects_imported: usize,
    pub checkpoints_imported: usize,
    pub events_imported: usize,
    pub publications_imported: usize,
    pub tasks_archived: usize,
}

pub struct LegacyNovelImporter {
    base_dir: PathBuf,
}

impl LegacyNovelImporter {
    #[must_use]
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    pub fn import_into(&self, store: &NovelDomainStore) -> Result<MigrationReport> {
        let mut report = MigrationReport::default();
        for path in json_files(&self.base_dir.join("novel/projects"))? {
            let project: NovelProject = read_json(&path)?;
            report.projects_imported += usize::from(store.import_project(&project)?);
        }

        let checkpoints = self.read_checkpoints()?;
        let active = newest_active_by_project(&checkpoints);
        for checkpoint in checkpoints {
            let archived = !checkpoint.phase.is_terminal()
                && active
                    .get(&checkpoint.project_id)
                    .is_some_and(|task_id| task_id != &checkpoint.task_id);
            if archived {
                report.tasks_archived += 1;
            }
            report.checkpoints_imported +=
                usize::from(store.import_checkpoint(&checkpoint, archived)?);
        }

        for path in jsonl_files(&self.base_dir.join("novel/lifecycle/events"))? {
            for (index, line) in std::fs::read_to_string(&path)?.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let event: NovelTaskEvent = serde_json::from_str(line).map_err(|error| {
                    NovelApplicationError::Migration(format!(
                        "{} line {}: {error}",
                        path.display(),
                        index + 1
                    ))
                })?;
                report.events_imported += usize::from(store.import_task_event(&event)?);
            }
        }

        for path in json_files(&self.base_dir.join("novel/lifecycle/publications"))? {
            let publication: NovelPublicationRecord = read_json(&path)?;
            report.publications_imported += usize::from(store.import_publication(&publication)?);
        }
        Ok(report)
    }

    fn read_checkpoints(&self) -> Result<Vec<NovelTaskCheckpoint>> {
        json_files(&self.base_dir.join("novel/lifecycle/checkpoints"))?
            .iter()
            .map(|path| read_json(path))
            .collect()
    }
}

fn newest_active_by_project(checkpoints: &[NovelTaskCheckpoint]) -> BTreeMap<String, String> {
    let mut active = BTreeMap::<String, (i64, String)>::new();
    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| !checkpoint.phase.is_terminal())
    {
        let candidate = (checkpoint.updated_at, checkpoint.task_id.clone());
        if active
            .get(&checkpoint.project_id)
            .is_none_or(|current| candidate > *current)
        {
            active.insert(checkpoint.project_id.clone(), candidate);
        }
    }
    active
        .into_iter()
        .map(|(project_id, (_, task_id))| (project_id, task_id))
        .collect()
}

fn json_files(path: &Path) -> Result<Vec<PathBuf>> {
    files_with_extension(path, "json")
}

fn jsonl_files(path: &Path) -> Result<Vec<PathBuf>> {
    files_with_extension(path, "jsonl")
}

fn files_with_extension(path: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut files = std::fs::read_dir(path)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some(extension))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|error| NovelApplicationError::Migration(format!("{}: {error}", path.display())))
}
