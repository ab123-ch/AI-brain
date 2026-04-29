//! L1 经验归档层
//!
//! 职责：
//! - 按主题分类存储归档经验
//! - 每个主题一个文件夹，包含索引和引用
//! - 供主脑按 topic 渐进式检索历史经验
//!
//! 存储路径：`memory/archive/{topic}/`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Storage;

/// 归档引用（指向 L2 总结文件）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveReference {
    /// L2 总结文件的 session_id
    pub session_id: String,
    /// 本会话在 topic 下的贡献摘要
    pub summary: String,
    /// 重要度
    pub importance: f64,
}

/// 每个 topic 的归档索引文件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveTopicIndex {
    /// 主题领域（如"记忆脑开发"、"提单费用开发"）
    pub topic: String,
    /// 触发关键词（用于主脑匹配）
    pub trigger_keywords: Vec<String>,
    /// 跨会话的合并经验总结
    pub merged_summary: String,
    /// 重要度（基于引用条目的最高重要度）
    pub importance: f64,
    /// 引用列表（指向 L2 总结）
    pub references: Vec<ArchiveReference>,
    /// 上次更新
    pub updated_at: DateTime<Utc>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

/// L1 归档存储
pub struct ArchiveStore {
    storage: Storage,
}

impl ArchiveStore {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    fn topic_dir(&self, topic: &str) -> std::path::PathBuf {
        self.storage.archive_topic_dir(topic)
    }

    fn index_path(&self, topic: &str) -> std::path::PathBuf {
        self.topic_dir(topic).join("index.json")
    }

    /// 加载指定 topic 的归档索引
    pub fn load_topic(&self, topic: &str) -> Result<Option<ArchiveTopicIndex>> {
        let path = self.index_path(topic);
        if path.exists() {
            self.storage.read_json(&path).map(Some)
        } else {
            Ok(None)
        }
    }

    /// 保存/更新 topic 归档索引
    pub fn save_topic(&self, index: &ArchiveTopicIndex) -> Result<()> {
        let path = self.index_path(&index.topic);
        self.storage.write_json_atomic(&path, index)
    }

    /// 列出所有归档 topic
    pub fn list_topics(&self) -> Result<Vec<String>> {
        let archive_dir = self.storage.archive_dir();
        if !archive_dir.exists() {
            return Ok(Vec::new());
        }
        let mut topics = Vec::new();
        for entry in std::fs::read_dir(&archive_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    // 检查是否有 index.json
                    if entry.path().join("index.json").exists() {
                        topics.push(name.to_string());
                    }
                }
            }
        }
        topics.sort();
        Ok(topics)
    }

    /// 按关键词匹配归档 topic（渐进式披露入口）
    pub fn match_topics(
        &self,
        keywords: &[String],
        limit: usize,
    ) -> Result<Vec<ArchiveTopicIndex>> {
        let topics = self.list_topics()?;
        let mut matched = Vec::new();
        for topic in &topics {
            if matched.len() >= limit {
                break;
            }
            if let Ok(Some(index)) = self.load_topic(topic) {
                let haystack = format!(
                    "{} {} {}",
                    index.topic,
                    index.trigger_keywords.join(" "),
                    index.merged_summary
                )
                .to_lowercase();
                if keywords
                    .iter()
                    .any(|kw| haystack.contains(&kw.to_lowercase()))
                {
                    matched.push(index);
                }
            }
        }
        matched.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(matched)
    }

    /// 删除 topic
    pub fn delete_topic(&self, topic: &str) -> Result<()> {
        let dir = self.topic_dir(topic);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, ArchiveStore) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        (tmp, ArchiveStore::new(storage))
    }

    fn make_index(topic: &str) -> ArchiveTopicIndex {
        ArchiveTopicIndex {
            topic: topic.into(),
            trigger_keywords: vec!["记忆脑".into(), "L2".into()],
            merged_summary: "做过记忆脑开发".into(),
            importance: 0.9,
            references: vec![ArchiveReference {
                session_id: "sess-test".into(),
                summary: "讨论了架构设计".into(),
                importance: 0.8,
            }],
            updated_at: Utc::now(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn save_and_load_topic() {
        let (_tmp, store) = make_store();
        let index = make_index("记忆脑开发");
        store.save_topic(&index).unwrap();

        let loaded = store.load_topic("记忆脑开发").unwrap().unwrap();
        assert_eq!(loaded.topic, "记忆脑开发");
        assert_eq!(loaded.references.len(), 1);
    }

    #[test]
    fn list_topics() {
        let (_tmp, store) = make_store();
        store.save_topic(&make_index("topic-a")).unwrap();
        store.save_topic(&make_index("topic-b")).unwrap();

        let topics = store.list_topics().unwrap();
        assert_eq!(topics.len(), 2);
    }

    #[test]
    fn match_topics_by_keyword() {
        let (_tmp, store) = make_store();
        let mut dev_idx = make_index("记忆脑开发");
        dev_idx.trigger_keywords = vec!["记忆脑".into(), "L2".into()];
        store.save_topic(&dev_idx).unwrap();

        // Ship-Core topic with different keywords
        let mut ship_idx = make_index("Ship-Core");
        ship_idx.trigger_keywords = vec!["订舱".into(), "提单".into()];
        ship_idx.merged_summary = "Ship-Core 业务开发".into();
        store.save_topic(&ship_idx).unwrap();

        let matched = store.match_topics(&["记忆脑".into()], 10).unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].topic, "记忆脑开发");
    }
}
