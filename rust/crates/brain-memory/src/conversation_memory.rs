//! Web conversation memory generations and invalidation state.
//!
//! Each user turn is stored in its own L1 session file. Editing or retrying a
//! turn moves the superseded generation out of the active L1 pool and marks
//! derived memory stale until a full concentration pass rebuilds it.

use std::fs;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{MemoryError, Result};
use crate::pyramid_storage::PyramidStorage;

const MAX_INVALIDATION_RECORDS: usize = 500;
static CONVERSATION_MEMORY_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMemoryScope {
    pub conversation_id: String,
    pub generation_id: String,
}

impl ConversationMemoryScope {
    pub fn new(
        conversation_id: impl Into<String>,
        generation_id: impl Into<String>,
    ) -> Result<Self> {
        let scope = Self {
            conversation_id: conversation_id.into(),
            generation_id: generation_id.into(),
        };
        validate_identifier("conversation_id", &scope.conversation_id)?;
        validate_identifier("generation_id", &scope.generation_id)?;
        Ok(scope)
    }

    pub fn storage_session_id(&self, base_session_id: &str) -> Result<String> {
        validate_identifier("generation_id", &self.generation_id)?;
        Ok(format!("{base_session_id}--web--{}", self.generation_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMemoryInvalidation {
    pub conversation_id: String,
    pub generation_ids: Vec<String>,
    /// Legacy Web messages predate generation tracking. Their exact L1 rows
    /// cannot be isolated, so derived memory must remain suppressed.
    pub includes_legacy_unscoped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationMemoryRebuildSnapshot {
    pub revision: u64,
    pub derived_stale: bool,
    pub legacy_unscoped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InvalidationRecord {
    conversation_id: String,
    generation_ids: Vec<String>,
    includes_legacy_unscoped: bool,
    invalidated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InvalidationState {
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    derived_stale: bool,
    #[serde(default)]
    legacy_unscoped: bool,
    #[serde(default)]
    records: Vec<InvalidationRecord>,
}

pub struct ConversationMemoryStore {
    storage: PyramidStorage,
    base_session_id: String,
}

impl ConversationMemoryStore {
    pub fn new(storage: PyramidStorage, base_session_id: impl Into<String>) -> Self {
        Self {
            storage,
            base_session_id: base_session_id.into(),
        }
    }

    /// Returns the current invalidation revision. A concentration pass captures
    /// this before it starts and may only clear staleness at the same revision.
    pub fn revision(&self) -> Result<u64> {
        Ok(self.load_state()?.revision)
    }

    pub fn is_derived_stale(&self) -> Result<bool> {
        Ok(self.load_state()?.derived_stale)
    }

    /// Capture the rebuild inputs under the same lock used by invalidation.
    /// A later invalidation is detected by `mark_rebuilt`'s revision check.
    pub fn rebuild_snapshot(&self) -> Result<ConversationMemoryRebuildSnapshot> {
        let _guard = conversation_memory_lock()?;
        let state = self.load_state()?;
        Ok(ConversationMemoryRebuildSnapshot {
            revision: state.revision,
            derived_stale: state.derived_stale,
            legacy_unscoped: state.legacy_unscoped,
        })
    }

    /// Invalidate superseded generations before the edited/retried turn is
    /// regenerated. The stale marker is persisted first, so any later I/O
    /// failure remains fail-closed and prevents old derived memory injection.
    pub fn invalidate(&self, invalidation: &ConversationMemoryInvalidation) -> Result<usize> {
        validate_identifier("conversation_id", &invalidation.conversation_id)?;
        for generation_id in &invalidation.generation_ids {
            validate_identifier("generation_id", generation_id)?;
        }
        let _guard = conversation_memory_lock()?;

        let mut state = self.load_state()?;
        state.revision = state.revision.saturating_add(1);
        state.derived_stale = true;
        state.legacy_unscoped |= invalidation.includes_legacy_unscoped;
        state.records.push(InvalidationRecord {
            conversation_id: invalidation.conversation_id.clone(),
            generation_ids: invalidation.generation_ids.clone(),
            includes_legacy_unscoped: invalidation.includes_legacy_unscoped,
            invalidated_at: Utc::now(),
        });
        if state.records.len() > MAX_INVALIDATION_RECORDS {
            let remove = state.records.len() - MAX_INVALIDATION_RECORDS;
            state.records.drain(..remove);
        }
        self.save_state(&state)?;

        fs::create_dir_all(self.storage.invalidated_l1_dir())?;
        let mut moved = 0;
        for generation_id in &invalidation.generation_ids {
            let scoped_session = format!("{}--web--{generation_id}", self.base_session_id);
            let active = self.storage.l1_session_path(&scoped_session);
            let invalidated = self.storage.invalidated_l1_session_path(&scoped_session);
            if !active.exists() {
                continue;
            }
            if invalidated.exists() {
                return Err(MemoryError::Conflict(format!(
                    "memory generation already invalidated: {generation_id}"
                )));
            }
            fs::rename(active, invalidated)?;
            moved += 1;
        }
        Ok(moved)
    }

    /// Mark a successful full rebuild. A newer invalidation wins, and legacy
    /// unscoped history keeps the derived layers suppressed because it cannot
    /// be removed selectively.
    pub fn mark_rebuilt(&self, expected_revision: u64) -> Result<bool> {
        self.commit_rebuild(expected_revision, || Ok(()))
    }

    /// Commit derived projections and clear staleness under the invalidation
    /// lock. The callback is never run for a superseded rebuild revision.
    pub fn commit_rebuild(
        &self,
        expected_revision: u64,
        finalize: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        let _guard = conversation_memory_lock()?;
        let mut state = self.load_state()?;
        if state.revision != expected_revision || state.legacy_unscoped {
            return Ok(false);
        }
        finalize()?;
        if !state.derived_stale {
            return Ok(true);
        }
        state.derived_stale = false;
        self.save_state(&state)?;
        Ok(true)
    }

    fn load_state(&self) -> Result<InvalidationState> {
        Ok(self
            .storage
            .read_json_optional(&self.storage.conversation_invalidation_path())?
            .unwrap_or_default())
    }

    fn save_state(&self, state: &InvalidationState) -> Result<()> {
        let path = self.storage.conversation_invalidation_path();
        let parent = path
            .parent()
            .ok_or_else(|| MemoryError::PathNotFound(path.clone()))?;
        fs::create_dir_all(parent)?;
        let mut temp = NamedTempFile::new_in(parent)?;
        temp.write_all(&serde_json::to_vec_pretty(state)?)?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

fn conversation_memory_lock() -> Result<std::sync::MutexGuard<'static, ()>> {
    CONVERSATION_MEMORY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| MemoryError::Conflict("conversation memory lock poisoned".into()))
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(MemoryError::Conflict(format!(
            "invalid {field}: only ASCII letters, digits, '-' and '_' are allowed"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw_pool::RawPool;

    #[test]
    fn scoped_generation_is_moved_and_staleness_is_revision_safe() {
        let temp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(temp.path().to_path_buf(), "test");
        storage.ensure_dirs().unwrap();
        let scope = ConversationMemoryScope::new("chat_1", "generation_1").unwrap();
        let scoped_session = scope.storage_session_id("memory_session").unwrap();
        RawPool::new(storage.clone())
            .append_turn(&scoped_session, "user", "old branch", None)
            .unwrap();

        let store = ConversationMemoryStore::new(storage.clone(), "memory_session");
        let moved = store
            .invalidate(&ConversationMemoryInvalidation {
                conversation_id: "chat_1".into(),
                generation_ids: vec!["generation_1".into()],
                includes_legacy_unscoped: false,
            })
            .unwrap();

        assert_eq!(moved, 1);
        assert!(!storage.l1_session_path(&scoped_session).exists());
        assert!(storage
            .invalidated_l1_session_path(&scoped_session)
            .exists());
        assert!(store.is_derived_stale().unwrap());
        let revision = store.revision().unwrap();
        let snapshot = store.rebuild_snapshot().unwrap();
        assert_eq!(snapshot.revision, revision);
        assert!(snapshot.derived_stale);
        assert!(!snapshot.legacy_unscoped);
        let mut stale_finalize_ran = false;
        assert!(!store
            .commit_rebuild(revision.saturating_sub(1), || {
                stale_finalize_ran = true;
                Ok(())
            })
            .unwrap());
        assert!(!stale_finalize_ran);
        assert!(store.mark_rebuilt(revision).unwrap());
        assert!(!store.is_derived_stale().unwrap());
        assert!(!store.mark_rebuilt(revision.saturating_sub(1)).unwrap());
    }

    #[test]
    fn legacy_invalidation_remains_fail_closed_after_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(temp.path().to_path_buf(), "test");
        storage.ensure_dirs().unwrap();
        let store = ConversationMemoryStore::new(storage, "memory_session");
        store
            .invalidate(&ConversationMemoryInvalidation {
                conversation_id: "chat_legacy".into(),
                generation_ids: Vec::new(),
                includes_legacy_unscoped: true,
            })
            .unwrap();
        let revision = store.revision().unwrap();
        let snapshot = store.rebuild_snapshot().unwrap();
        assert_eq!(snapshot.revision, revision);
        assert!(snapshot.derived_stale);
        assert!(snapshot.legacy_unscoped);
        assert!(!store.mark_rebuilt(revision).unwrap());
        assert!(store.is_derived_stale().unwrap());
    }
}
