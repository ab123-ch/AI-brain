use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Lifecycle status of an evolution target.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum TargetStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

/// A measurable checkpoint within an evolution target.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Checkpoint {
    pub desc: String,
    pub met: bool,
}

/// A user-defined evolution target.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EvoTarget {
    pub id: String,
    pub direction: String,
    pub description: String,
    pub priority: u32,
    pub status: TargetStatus,
    pub checkpoints: Vec<Checkpoint>,
    pub created_at: DateTime<Utc>,
    pub related_skills: Vec<String>,
}

/// Persistent queue of evolution targets stored as `evolution_targets.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EvoTargetQueue {
    /// Directory that holds `evolution_targets.json`.
    base_dir: PathBuf,
    targets: Vec<EvoTarget>,
}

impl EvoTargetQueue {
    const FILENAME: &'static str = "evolution_targets.json";

    /// Create a new queue, reading from `base_dir/evolution_targets.json` if present.
    pub fn new(base_dir: &Path) -> Self {
        let path = base_dir.join(Self::FILENAME);
        let targets = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(json) => serde_json::from_str::<Vec<EvoTarget>>(&json).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        Self {
            base_dir: base_dir.to_path_buf(),
            targets,
        }
    }

    /// Add a target and persist immediately.
    pub fn add_target(&mut self, target: EvoTarget) -> Result<(), String> {
        if self.targets.iter().any(|t| t.id == target.id) {
            return Err(format!("duplicate target id: {}", target.id));
        }
        self.targets.push(target);
        self.persist()
    }

    /// Return a slice of all targets.
    pub fn list_targets(&self) -> &[EvoTarget] {
        &self.targets
    }

    /// Return references to targets sorted by priority ascending (1 is highest).
    pub fn sorted_targets(&self) -> Vec<&EvoTarget> {
        let mut refs: Vec<&EvoTarget> = self.targets.iter().collect();
        refs.sort_by_key(|t| t.priority);
        refs
    }

    /// Update the status of a target by id.
    pub fn update_status(&mut self, id: &str, status: TargetStatus) -> Result<(), String> {
        let target = self
            .targets
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("target not found: {id}"))?;
        target.status = status;
        self.persist()
    }

    /// Set the `met` flag on a specific checkpoint.
    pub fn update_checkpoint(&mut self, id: &str, idx: usize, met: bool) -> Result<(), String> {
        let target = self
            .targets
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("target not found: {id}"))?;
        let cp = target
            .checkpoints
            .get_mut(idx)
            .ok_or_else(|| format!("checkpoint index out of bounds: {idx}"))?;
        cp.met = met;
        self.persist()
    }

    /// Remove a target by id.
    pub fn remove_target(&mut self, id: &str) -> Result<(), String> {
        let len_before = self.targets.len();
        self.targets.retain(|t| t.id != id);
        if self.targets.len() == len_before {
            return Err(format!("target not found: {id}"));
        }
        self.persist()
    }

    /// Persist the queue to `base_dir/evolution_targets.json`.
    pub fn persist(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.targets)
            .map_err(|e| format!("serialize error: {e}"))?;
        fs::create_dir_all(&self.base_dir).map_err(|e| format!("create dir error: {e}"))?;
        fs::write(self.base_dir.join(Self::FILENAME), json).map_err(|e| format!("write error: {e}"))
    }
}
