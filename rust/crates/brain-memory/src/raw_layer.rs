use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use brain_core::types::{BrainContext, KnowledgeSource, MemoryEntry, MemoryLayer};

use crate::error::Result;
use crate::storage::Storage;

/// L3 原始记忆条目 — 永久保留
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEntry {
    pub id: String,
    pub content: String,
    pub raw_input: String,
    pub context: BrainContext,
    pub timestamp: DateTime<Utc>,
}

/// L3 原始记忆层
///
/// 职责：
/// - JSONL 持久化所有原始消息（永不删除）
/// - 按会话分文件存储
/// - 追加写入，不修改已有内容
pub struct RawLayer {
    storage: Storage,
}

impl RawLayer {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 追加一条原始记忆
    pub fn append(&self, session_id: &str, entry: &RawEntry) -> Result<()> {
        let path = self
            .storage
            .sessions_dir()
            .join(format!("{session_id}.jsonl"));
        self.storage.append_jsonl(&path, &entry)
    }

    /// 读取一个会话的所有原始记忆
    pub fn read_session(&self, session_id: &str) -> Result<Vec<RawEntry>> {
        let path = self
            .storage
            .sessions_dir()
            .join(format!("{session_id}.jsonl"));
        self.storage.read_jsonl(&path)
    }

    /// 列出所有会话文件
    pub fn list_sessions(&self) -> Result<Vec<String>> {
        let files = self
            .storage
            .list_jsonl_files(&self.storage.sessions_dir())?;
        Ok(files
            .iter()
            .filter_map(|f| f.file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect())
    }

    /// 统计原始记忆条目数
    pub fn count(&self) -> Result<u32> {
        let sessions = self.list_sessions()?;
        let mut total = 0u32;
        for sid in &sessions {
            total += self.read_session(sid)?.len() as u32;
        }
        Ok(total)
    }

    /// 按关键词在原始层搜索（遍历所有会话）
    ///
    /// 注意：L3 搜索较慢，只在 L0-L2 未命中时使用
    pub fn search(&self, keywords: &[String], limit: usize) -> Result<Vec<MemoryEntry>> {
        let sessions = self.list_sessions()?;
        let mut results = Vec::new();

        for sid in &sessions {
            let entries = self.read_session(sid)?;
            for entry in entries {
                let matched = keywords.iter().any(|kw| {
                    entry.content.to_lowercase().contains(&kw.to_lowercase())
                        || entry.raw_input.to_lowercase().contains(&kw.to_lowercase())
                });
                if matched {
                    results.push(MemoryEntry {
                        id: entry.id.clone(),
                        content: entry.content.clone(),
                        tags: Vec::new(),
                        layer: MemoryLayer::Raw,
                        importance: 0.3, // 原始层默认低重要性
                        source: KnowledgeSource::Memory {
                            memory_id: entry.id.clone(),
                            layer: MemoryLayer::Raw,
                        },
                        confidence: 1.0,
                        reference_count: 0,
                        created_at: entry.timestamp,
                        last_accessed: entry.timestamp,
                        consolidated: false,
                    });
                    if results.len() >= limit {
                        return Ok(results);
                    }
                }
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_layer() -> RawLayer {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        // Leak tempdir to keep it alive for the test
        // (in real code, Storage owns the path)
        std::mem::forget(tmp);
        RawLayer::new(storage)
    }

    fn test_context() -> BrainContext {
        BrainContext {
            current_date: "2026-04-04".into(),
            cwd: "/test".into(),
            git_branch: Some("main".into()),
            platform: "darwin".into(),
        }
    }

    #[test]
    fn append_and_read_session() {
        let layer = make_layer();
        let entry = RawEntry {
            id: "r1".into(),
            content: "用户查询清明节假期".into(),
            raw_input: "这个月有节假日吗？".into(),
            context: test_context(),
            timestamp: Utc::now(),
        };

        layer.append("sess-001", &entry).unwrap();
        let entries = layer.read_session("sess-001").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "r1");
        assert_eq!(entries[0].raw_input, "这个月有节假日吗？");
    }

    #[test]
    fn multiple_sessions() {
        let layer = make_layer();
        let ctx = test_context();

        for i in 0..3 {
            let entry = RawEntry {
                id: format!("r{i}"),
                content: format!("内容{i}"),
                raw_input: format!("输入{i}"),
                context: ctx.clone(),
                timestamp: Utc::now(),
            };
            layer.append("sess-a", &entry).unwrap();
        }

        let entry = RawEntry {
            id: "other".into(),
            content: "另一个会话".into(),
            raw_input: "另一个".into(),
            context: ctx,
            timestamp: Utc::now(),
        };
        layer.append("sess-b", &entry).unwrap();

        let sessions = layer.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);

        assert_eq!(layer.count().unwrap(), 4);
    }

    #[test]
    fn search_by_keyword() {
        let layer = make_layer();
        let ctx = test_context();

        let entry = RawEntry {
            id: "r1".into(),
            content: "清明节是4月5日".into(),
            raw_input: "清明节几号".into(),
            context: ctx,
            timestamp: Utc::now(),
        };
        layer.append("sess-001", &entry).unwrap();

        let results = layer.search(&["清明节".into()], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].layer, MemoryLayer::Raw);
    }
}
