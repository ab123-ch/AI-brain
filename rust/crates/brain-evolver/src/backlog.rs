use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Source that produced a backlog entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BacklogSource {
    Eval,
    User,
    SelfDiagnosis,
    Memory,
    UserDefined,
}

/// Category of the identified issue.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BacklogCategory {
    KnowledgeGap,
    ToolMissing,
    CodeQuality,
    ReasoningWeakness,
    SkillConflict,
}

/// How severe the issue is.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Current lifecycle status of a backlog entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BacklogStatus {
    Pending,
    InProgress,
    Resolved,
    Superseded,
    Blocked,
}

/// A single evolution backlog entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BacklogEntry {
    pub id: String,
    pub source: BacklogSource,
    pub category: BacklogCategory,
    pub description: String,
    pub severity: Severity,
    pub frequency: u32,
    pub status: BacklogStatus,
    pub created_at: DateTime<Utc>,
    pub context_snapshot: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub evolution_log_id: Option<String>,
}

/// The evolution backlog persisted as `targets.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EvolutionBacklog {
    /// Directory that holds `targets.json`.
    base_dir: PathBuf,
    entries: Vec<BacklogEntry>,
}

impl EvolutionBacklog {
    const FILENAME: &'static str = "targets.json";

    /// Create a new backlog, reading from `base_dir/targets.json` if present.
    pub fn new(base_dir: &Path) -> Self {
        let path = base_dir.join(Self::FILENAME);
        let entries = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(json) => serde_json::from_str::<Vec<BacklogEntry>>(&json).unwrap_or_default(),
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

    /// Add an entry. If a semantically similar entry already exists (keyword
    /// overlap > 60%), its `frequency` is incremented and `severity` upgraded
    /// instead of inserting a duplicate.
    pub fn add_entry(&mut self, entry: BacklogEntry) -> Result<(), String> {
        let new_keywords = extract_keywords(&entry.description);

        for existing in &mut self.entries {
            if existing.source != entry.source || existing.category != entry.category {
                continue;
            }
            let existing_keywords = extract_keywords(&existing.description);
            let overlap = keyword_overlap(&new_keywords, &existing_keywords);
            if overlap > 0.6 {
                existing.frequency += 1;
                // Upgrade severity if the new entry is worse.
                if entry.severity > existing.severity {
                    existing.severity = entry.severity.clone();
                }
                return Ok(());
            }
        }

        self.entries.push(entry);
        Ok(())
    }

    /// Return references to all entries matching the given status.
    pub fn query_by_status(&self, status: BacklogStatus) -> Vec<&BacklogEntry> {
        self.entries.iter().filter(|e| e.status == status).collect()
    }

    /// Return all entries sorted by `frequency` in descending order.
    pub fn query_sorted_by_priority(&self) -> Vec<&BacklogEntry> {
        let mut refs: Vec<&BacklogEntry> = self.entries.iter().collect();
        refs.sort_by(|a, b| b.frequency.cmp(&a.frequency));
        refs
    }

    /// Mark an entry as resolved and attach the evolution log id.
    pub fn resolve_entry(&mut self, id: &str, evo_log_id: &str) -> Result<(), String> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("entry not found: {id}"))?;
        entry.status = BacklogStatus::Resolved;
        entry.resolved_at = Some(Utc::now());
        entry.evolution_log_id = Some(evo_log_id.to_string());
        Ok(())
    }

    /// Persist the backlog to `base_dir/targets.json`.
    pub fn persist(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| format!("serialize error: {e}"))?;
        fs::create_dir_all(&self.base_dir).map_err(|e| format!("create dir error: {e}"))?;
        fs::write(self.base_dir.join(Self::FILENAME), json).map_err(|e| format!("write error: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Keyword helpers (simple whitespace tokenisation, lowercased)
// ---------------------------------------------------------------------------

/// Extract a set of lowercased keywords from text.
fn extract_keywords(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

/// Compute Jaccard-like overlap ratio: |intersection| / |union|.
fn keyword_overlap(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let union_len = a.union(b).count() as f64;
    if union_len == 0.0 {
        return 0.0;
    }
    let inter_len = a.intersection(b).count() as f64;
    inter_len / union_len
}
