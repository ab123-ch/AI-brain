use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use brain_core::types::{KnowledgeSource, MemoryEntry, MemoryLayer};

use crate::error::Result;
use crate::storage::Storage;

/// L1 事件索引条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEntry {
    pub id: String,
    pub date: String,
    pub summary: String,
    pub key_actions: Vec<String>,
    pub tags: Vec<String>,
    pub importance: f64,
    pub created_at: DateTime<Utc>,
}

/// L1 事件索引层
///
/// 职责：
/// - 按日期存储事件摘要（每日一个文件）
/// - 关键事件记录：做了什么、为什么、改了什么
/// - 持久化到 `memory/events/{date}.json`
pub struct EventIndexLayer {
    storage: Storage,
}

impl EventIndexLayer {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 存入一个事件
    pub fn store(&self, entry: &EventEntry) -> Result<()> {
        let path = self
            .storage
            .events_dir()
            .join(format!("{}.json", entry.date));
        let mut events: Vec<EventEntry> = if path.exists() {
            self.storage.read_json(&path)?
        } else {
            Vec::new()
        };
        events.push(entry.clone());
        self.storage.write_json(&path, &events)?;
        Ok(())
    }

    /// 读取某日的事件
    pub fn read_date(&self, date: &str) -> Result<Vec<EventEntry>> {
        let path = self.storage.events_dir().join(format!("{date}.json"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        self.storage.read_json(&path)
    }

    /// 按关键词搜索事件
    pub fn search(&self, keywords: &[String], limit: usize) -> Result<Vec<MemoryEntry>> {
        let files = self.storage.list_json_files(&self.storage.events_dir())?;
        let mut results = Vec::new();

        for file in &files {
            let events: Vec<EventEntry> = self.storage.read_json(file)?;
            for event in events {
                let matched = keywords.iter().any(|kw| {
                    event.summary.to_lowercase().contains(&kw.to_lowercase())
                        || event.tags.iter().any(|t| t.eq_ignore_ascii_case(kw))
                        || event
                            .key_actions
                            .iter()
                            .any(|a| a.to_lowercase().contains(&kw.to_lowercase()))
                });
                if matched {
                    results.push(MemoryEntry {
                        id: event.id.clone(),
                        content: event.summary.clone(),
                        tags: event.tags.clone(),
                        layer: MemoryLayer::EventIndex,
                        importance: event.importance,
                        source: KnowledgeSource::Memory {
                            memory_id: event.id.clone(),
                            layer: MemoryLayer::EventIndex,
                        },
                        confidence: 0.85,
                        reference_count: 0,
                        created_at: event.created_at,
                        last_accessed: event.created_at,
                        consolidated: true,
                    });
                    if results.len() >= limit {
                        return Ok(results);
                    }
                }
            }
        }
        Ok(results)
    }

    /// 统计事件数
    pub fn count(&self) -> Result<u32> {
        let files = self.storage.list_json_files(&self.storage.events_dir())?;
        let mut total = 0u32;
        for file in &files {
            let events: Vec<EventEntry> = self.storage.read_json(file)?;
            total += events.len() as u32;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_layer() -> EventIndexLayer {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        std::mem::forget(tmp);
        EventIndexLayer::new(storage)
    }

    fn make_event(id: &str, date: &str, summary: &str, tags: &[&str]) -> EventEntry {
        EventEntry {
            id: id.into(),
            date: date.into(),
            summary: summary.into(),
            key_actions: vec!["完成了开发".into()],
            tags: tags.iter().map(|t| (*t).into()).collect(),
            importance: 0.7,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn store_and_read_date() {
        let layer = make_layer();
        layer
            .store(&make_event(
                "ev1",
                "2026-04-04",
                "完成了 Phase 3 记忆脑开发",
                &["开发", "记忆"],
            ))
            .unwrap();

        let events = layer.read_date("2026-04-04").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "完成了 Phase 3 记忆脑开发");
    }

    #[test]
    fn search_across_dates() {
        let layer = make_layer();
        layer
            .store(&make_event(
                "ev1",
                "2026-04-03",
                "Phase 2 感知脑完成",
                &["开发"],
            ))
            .unwrap();
        layer
            .store(&make_event(
                "ev2",
                "2026-04-04",
                "Phase 3 记忆脑开发",
                &["开发", "记忆"],
            ))
            .unwrap();

        let results = layer.search(&["记忆".into()], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].layer, MemoryLayer::EventIndex);
    }

    #[test]
    fn read_empty_date() {
        let layer = make_layer();
        let events = layer.read_date("2099-01-01").unwrap();
        assert!(events.is_empty());
    }
}
