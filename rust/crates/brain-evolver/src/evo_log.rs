use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Phases in a single evolution cycle.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum EvoPhase {
    Perceive,
    Research,
    Learn,
    Synthesize,
    Register,
    Verify,
}

/// Lifecycle status of an evolution cycle.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum EvoCycleStatus {
    Running,
    Completed,
    Blocked,
    Cancelled,
}

/// Record for a single phase within an evolution cycle.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PhaseRecord {
    pub phase: EvoPhase,
    pub duration_secs: u64,
    pub summary: String,
    pub tokens_used: u64,
}

/// A complete evolution cycle log entry.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvoLogEntry {
    pub id: String,
    pub target_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub phases: Vec<PhaseRecord>,
    pub total_tokens: u64,
    pub skills_created: Vec<String>,
    pub backlog_resolved: Vec<String>,
    pub status: EvoCycleStatus,
}

/// Persistent store of evolution log entries, stored as `evolution_log.json`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvoLogStore {
    /// Directory that holds `evolution_log.json`.
    base_dir: PathBuf,
    entries: Vec<EvoLogEntry>,
}

impl EvoLogStore {
    const FILENAME: &'static str = "evolution_log.json";

    /// Create a new store, reading from `base_dir/evolution_log.json` if present.
    pub fn new(base_dir: &Path) -> Self {
        let path = base_dir.join(Self::FILENAME);
        let entries = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(json) => serde_json::from_str::<Vec<EvoLogEntry>>(&json).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        Self {
            base_dir: base_dir.to_path_buf(),
            entries,
        }
    }

    /// Add an entry and persist to disk.
    pub fn append(&mut self, entry: EvoLogEntry) {
        self.entries.push(entry);
        // Best-effort persist; callers can also call persist() explicitly.
        let _ = self.persist();
    }

    /// Return references to entries whose `started_at` starts with `date_prefix`
    /// (e.g. "2026-06-08").
    pub fn query_by_date(&self, date_prefix: &str) -> Vec<&EvoLogEntry> {
        self.entries
            .iter()
            .filter(|e| e.started_at.starts_with(date_prefix))
            .collect()
    }

    /// Return a reference to the most recent entry (by insertion order), or None.
    pub fn latest(&self) -> Option<&EvoLogEntry> {
        self.entries.last()
    }

    /// Update an existing entry by log id, filling in end-of-cycle fields.
    /// Returns Err if the log id is not found.
    pub fn update_entry(
        &mut self,
        log_id: &str,
        phases: Vec<PhaseRecord>,
        tokens: u64,
        skills: Vec<String>,
        resolved_backlog: Vec<String>,
        status: EvoCycleStatus,
    ) -> Result<(), String> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.id == log_id)
            .ok_or_else(|| format!("log entry not found: {log_id}"))?;
        entry.finished_at = Some(Utc::now().to_rfc3339());
        entry.phases = phases;
        entry.total_tokens = tokens;
        entry.skills_created = skills;
        entry.backlog_resolved = resolved_backlog;
        entry.status = status;
        self.persist()
    }

    /// Persist the store to `base_dir/evolution_log.json`.
    pub fn persist(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| format!("serialize error: {e}"))?;
        fs::create_dir_all(&self.base_dir).map_err(|e| format!("create dir error: {e}"))?;
        fs::write(self.base_dir.join(Self::FILENAME), json).map_err(|e| format!("write error: {e}"))
    }
}
