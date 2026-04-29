//! L2 会话总结层
//!
//! 职责：
//! - 每会话一个总结文件，由四步分析 Step6 生成
//! - 存储事实摘要、关键词、踩坑记录摘要、决策记录
//! - 供召回时作为快速查看的入口（替代旧 L2 原文存储）
//!
//! 存储路径：`memory/summaries/sess-{timestamp}.json`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Storage;

/// 一条 L2 会话总结
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// 对应 L3 会话 ID（文件名不含扩展名）
    pub session_id: String,
    /// 会话时间范围
    pub session_start: DateTime<Utc>,
    pub session_end: DateTime<Utc>,
    /// 事实摘要（由 Step1 产出，精炼到 200 字以内）
    pub fact_summary: String,
    /// 关键词/标签
    pub tags: Vec<String>,
    /// 本会话遇到的踩坑（简要描述，每条 50 字以内）
    pub pitfalls: Vec<String>,
    /// 本会话的决策/关键结论
    pub decisions: Vec<String>,
    /// 已归档到 L1（由守护线程标记）
    pub archived: bool,
    /// 被后续记忆迭代取代（不再召回，保留审计）
    #[serde(default)]
    pub superseded: bool,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

/// L2 会话总结存储
pub struct SessionSummaryStore {
    storage: Storage,
}

impl SessionSummaryStore {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    fn summary_path(&self, session_id: &str) -> std::path::PathBuf {
        self.storage
            .summaries_dir()
            .join(format!("{}.json", session_id))
    }

    /// 保存/更新一条会话总结
    pub fn save(&self, summary: &SessionSummary) -> Result<()> {
        let path = self.summary_path(&summary.session_id);
        self.storage.write_json_atomic(&path, summary)
    }

    /// 加载指定会话的总结
    pub fn load(&self, session_id: &str) -> Result<Option<SessionSummary>> {
        let path = self.summary_path(session_id);
        if path.exists() {
            self.storage.read_json(&path).map(Some)
        } else {
            Ok(None)
        }
    }

    /// 列出所有 L2 总结文件（按时间降序）
    pub fn list_all(&self) -> Result<Vec<SessionSummary>> {
        let dir = self.storage.summaries_dir();
        let files = self.storage.list_json_files(&dir)?;
        let mut summaries = Vec::new();
        for file in &files {
            if let Ok(s) = self.storage.read_json::<SessionSummary>(file) {
                summaries.push(s);
            }
        }
        summaries.sort_by(|a, b| b.session_end.cmp(&a.session_end));
        Ok(summaries)
    }

    /// 查找未归档的总结（供守护线程使用）
    pub fn find_unarchived(&self) -> Result<Vec<SessionSummary>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|s| !s.archived)
            .collect())
    }

    /// 标记为已归档
    pub fn mark_archived(&self, session_id: &str) -> Result<()> {
        if let Some(mut summary) = self.load(session_id)? {
            summary.archived = true;
            self.save(&summary)?;
        }
        Ok(())
    }

    /// 按关键词匹配 L2 总结（召回用，过滤 superseded）
    pub fn match_keywords(&self, keywords: &[String], limit: usize) -> Result<Vec<SessionSummary>> {
        let all = self.find_recallable()?;
        let mut matched = Vec::new();
        for s in &all {
            if matched.len() >= limit {
                break;
            }
            let haystack = format!(
                "{} {} {}",
                s.fact_summary,
                s.tags.join(" "),
                s.decisions.join(" ")
            )
            .to_lowercase();
            if keywords
                .iter()
                .any(|kw| haystack.contains(&kw.to_lowercase()))
            {
                matched.push(s.clone());
            }
        }
        Ok(matched)
    }

    /// 统计总结数
    pub fn count(&self) -> Result<usize> {
        let dir = self.storage.summaries_dir();
        Ok(self.storage.list_json_files(&dir)?.len())
    }

    /// 查找可召回的总结（过滤 superseded 且未归档）
    pub fn find_recallable(&self) -> Result<Vec<SessionSummary>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|s| !s.superseded && !s.archived)
            .collect())
    }

    /// 标记为已取代
    pub fn mark_superseded(&self, session_id: &str) -> Result<()> {
        if let Some(mut summary) = self.load(session_id)? {
            summary.superseded = true;
            self.save(&summary)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, SessionSummaryStore) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        (tmp, SessionSummaryStore::new(storage))
    }

    fn make_summary(session_id: &str) -> SessionSummary {
        SessionSummary {
            session_id: session_id.into(),
            session_start: Utc::now(),
            session_end: Utc::now(),
            fact_summary: "讨论记忆脑架构设计".into(),
            tags: vec!["记忆脑".into(), "架构".into()],
            pitfalls: vec!["L2 和 L1 功能混淆".into()],
            decisions: vec!["L2 改为每会话总结".into()],
            archived: false,
            superseded: false,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn save_and_load() {
        let (_tmp, store) = make_store();
        let summary = make_summary("sess-test-1");
        store.save(&summary).unwrap();

        let loaded = store.load("sess-test-1").unwrap().unwrap();
        assert_eq!(loaded.fact_summary, "讨论记忆脑架构设计");
        assert!(loaded.tags.contains(&"记忆脑".into()));
    }

    #[test]
    fn load_nonexistent() {
        let (_tmp, store) = make_store();
        let loaded = store.load("sess-nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn list_all_returns_sorted() {
        let (_tmp, store) = make_store();
        store.save(&make_summary("sess-1")).unwrap();
        store.save(&make_summary("sess-2")).unwrap();

        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn find_unarchived() {
        let (_tmp, store) = make_store();
        let mut s = make_summary("sess-1");
        s.archived = true;
        store.save(&s).unwrap();
        store.save(&make_summary("sess-2")).unwrap();

        let unarchived = store.find_unarchived().unwrap();
        assert_eq!(unarchived.len(), 1);
        assert_eq!(unarchived[0].session_id, "sess-2");
    }

    #[test]
    fn mark_archived() {
        let (_tmp, store) = make_store();
        store.save(&make_summary("sess-1")).unwrap();
        store.mark_archived("sess-1").unwrap();

        let loaded = store.load("sess-1").unwrap().unwrap();
        assert!(loaded.archived);
    }
}
