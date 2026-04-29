//! 守护线程 — L2 总结归档到 L1、合并 merged_summary、淘汰低 importance 记忆
//!
//! 职责：
//! - 检查触发条件（8h+文件增量）
//! - LLM topic 分类未归档总结
//! - 合并到已有 topic 或创建新 topic
//! - 调用 ImportanceManager 衰减淘汰

use std::path::PathBuf;

use chrono::Utc;

use crate::analyzer::AnalysisLlm;
use crate::archive::{ArchiveReference, ArchiveStore, ArchiveTopicIndex};
use crate::error::Result;
use crate::importance::ImportanceManager;
use crate::storage::Storage;
use crate::summary::SessionSummaryStore;

/// 守护线程配置
#[derive(Debug, Clone)]
pub struct GuardianConfig {
    /// 检查间隔（秒），默认 28800 (8h)
    pub check_interval_secs: u64,
    /// 最小未归档总结数才触发，默认 10
    pub min_new_summaries: usize,
}

impl Default for GuardianConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 28800,
            min_new_summaries: 10,
        }
    }
}

/// 守护线程运行报告
#[derive(Debug, Clone)]
pub struct GuardianReport {
    /// 归档的总结数
    pub archived_count: usize,
    /// 新建的 topic 数
    pub new_topics: usize,
    /// 合并的 topic 数
    pub merged_topics: usize,
    /// 衰减条目数
    pub decayed_count: usize,
    /// 淘汰条目数
    pub pruned_count: usize,
}

/// 守护线程引擎
pub struct GuardianEngine {
    base_dir: PathBuf,
    config: GuardianConfig,
    llm: Box<dyn AnalysisLlm>,
}

impl GuardianEngine {
    pub fn new(base_dir: PathBuf, config: GuardianConfig, llm: Box<dyn AnalysisLlm>) -> Self {
        Self {
            base_dir,
            config,
            llm,
        }
    }

    /// 检查触发条件
    pub fn should_run(&self) -> Result<bool> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let summary_store = SessionSummaryStore::new(storage);
        let unarchived = summary_store.find_unarchived()?;
        Ok(unarchived.len() >= self.config.min_new_summaries)
    }

    /// 执行守护任务
    pub async fn run(&self) -> Result<GuardianReport> {
        let storage = Storage::new(self.base_dir.clone())?;
        let summary_store = SessionSummaryStore::new(storage.clone());
        let archive_store = ArchiveStore::new(storage.clone());

        // 1. 获取未归档总结
        let unarchived = summary_store.find_unarchived()?;
        if unarchived.is_empty() {
            return Ok(GuardianReport::default());
        }

        // 2. LLM topic 分类
        let topics = self.classify_topics(&unarchived).await;

        let mut report = GuardianReport::default();

        // 3. 归档到 topic
        for (topic_name, keywords, importance, session_ids) in &topics {
            let summaries_text: Vec<String> = unarchived
                .iter()
                .filter(|s| session_ids.contains(&s.session_id))
                .map(|s| s.fact_summary.clone())
                .collect();

            if summaries_text.is_empty() {
                continue;
            }

            match archive_store.load_topic(topic_name) {
                Ok(Some(mut existing_index)) => {
                    // 合并：LLM 生成合并 summary
                    let merged = self
                        .merge_summary(&existing_index.merged_summary, &summaries_text.join("\n"))
                        .await;

                    existing_index.merged_summary = merged;
                    for sid in session_ids {
                        existing_index.references.push(ArchiveReference {
                            session_id: sid.clone(),
                            summary: String::new(),
                            importance: *importance,
                        });
                    }
                    for kw in keywords {
                        if !existing_index
                            .trigger_keywords
                            .iter()
                            .any(|k| k.eq_ignore_ascii_case(kw))
                        {
                            existing_index.trigger_keywords.push(kw.clone());
                        }
                    }
                    existing_index.trigger_keywords.truncate(10);
                    existing_index.importance = existing_index.importance.max(*importance);
                    existing_index.updated_at = Utc::now();

                    archive_store.save_topic(&existing_index)?;
                    report.merged_topics += 1;
                }
                _ => {
                    // 新建 topic
                    let index = ArchiveTopicIndex {
                        topic: topic_name.clone(),
                        trigger_keywords: keywords.clone(),
                        merged_summary: summaries_text.join("\n"),
                        importance: *importance,
                        references: session_ids
                            .iter()
                            .map(|sid| ArchiveReference {
                                session_id: sid.clone(),
                                summary: String::new(),
                                importance: *importance,
                            })
                            .collect(),
                        updated_at: Utc::now(),
                        created_at: Utc::now(),
                    };
                    archive_store.save_topic(&index)?;
                    report.new_topics += 1;
                }
            }

            // 标记已归档
            for sid in session_ids {
                summary_store.mark_archived(sid)?;
            }
            report.archived_count += session_ids.len();
        }

        // 4. 衰减淘汰
        let decay_report = ImportanceManager::decay_all(&self.base_dir)?;
        report.decayed_count = decay_report.decayed_count;
        report.pruned_count = decay_report.pruned_count;

        Ok(report)
    }

    /// LLM 分类未归档总结为 topic
    async fn classify_topics(
        &self,
        unarchived: &[crate::summary::SessionSummary],
    ) -> Vec<(String, Vec<String>, f64, Vec<String>)> {
        let summaries_json: Vec<serde_json::Value> = unarchived
            .iter()
            .map(|s| {
                serde_json::json!({
                    "session_id": s.session_id,
                    "fact_summary": s.fact_summary,
                    "tags": s.tags,
                })
            })
            .collect();

        let prompt = format!(
            "# 任务\n\
             将以下未归档的会话总结按主题分类。\n\n\
             # 未归档总结\n\
             {}\n\n\
             # 输出格式（JSON 数组）\n\
             [\n\
               {{\n\
                 \"topic\": \"主题名称\",\n\
                 \"keywords\": [\"关键词1\", \"关键词2\"],\n\
                 \"importance\": 0.8,\n\
                 \"session_ids\": [\"sess-xxx\"]\n\
               }}\n\
             ]\n\n\
             规则：\n\
             1. 每个总结只能属于一个 topic\n\
             2. topic 名称用中文\n\
             3. keywords 提取 3-8 个\n\
             4. importance 0.0-1.0",
            serde_json::to_string(&summaries_json).unwrap_or_default()
        );

        match self.llm.complete(&prompt).await {
            Ok(response) => {
                let json_str = extract_json(&response);
                match serde_json::from_str::<Vec<serde_json::Value>>(&json_str) {
                    Ok(arr) => arr
                        .into_iter()
                        .filter_map(|v| {
                            let topic = v.get("topic")?.as_str()?.to_string();
                            let keywords = v
                                .get("keywords")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let importance =
                                v.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.7);
                            let session_ids = v
                                .get("session_ids")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();
                            Some((topic, keywords, importance, session_ids))
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                }
            }
            Err(_) => Vec::new(),
        }
    }

    /// LLM 合并已有 summary 与新总结
    async fn merge_summary(&self, existing: &str, new_summaries: &str) -> String {
        let prompt = format!(
            "# 任务\n\
             将新的会话总结合并到已有的归档总结中。\n\n\
             # 已有归档总结\n\
             {}\n\n\
             # 新的会话总结\n\
             {}\n\n\
             # 规则\n\
             1. 保留已有的核心内容\n\
             2. 整合新总结的关键信息\n\
             3. 去重，不要重复相同内容\n\
             4. 控制在 500 字以内\n\
             5. 直接输出合并后的文本（纯文本，不要 JSON）",
            existing, new_summaries
        );

        match self.llm.complete(&prompt).await {
            Ok(response) => response.trim().to_string(),
            Err(_) => format!("{}\n{}", existing, new_summaries),
        }
    }
}

/// 从 LLM 响应中提取 JSON
fn extract_json(text: &str) -> String {
    let text = text.trim();
    if let Some(start) = text.find('[') {
        let end = text[start..]
            .find(']')
            .map(|i| start + i + 1)
            .unwrap_or(text.len());
        text[start..end].to_string()
    } else if let Some(start) = text.find('{') {
        let end = text[start..]
            .rfind('}')
            .map(|i| start + i + 1)
            .unwrap_or(text.len());
        text[start..end].to_string()
    } else {
        text.to_string()
    }
}

impl Default for GuardianReport {
    fn default() -> Self {
        Self {
            archived_count: 0,
            new_topics: 0,
            merged_topics: 0,
            decayed_count: 0,
            pruned_count: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::SessionSummary;
    use std::future::Future;
    use std::pin::Pin;
    use tempfile::TempDir;

    struct MockLlm {
        response: String,
    }

    impl AnalysisLlm for MockLlm {
        fn complete(
            &self,
            _prompt: &str,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send + '_>>
        {
            Box::pin(async move { Ok(self.response.clone()) })
        }
    }

    fn make_engine(response: &str) -> (TempDir, GuardianEngine) {
        let tmp = TempDir::new().unwrap();
        let llm = Box::new(MockLlm {
            response: response.to_string(),
        });
        let engine = GuardianEngine::new(
            tmp.path().to_path_buf(),
            GuardianConfig {
                check_interval_secs: 8,
                min_new_summaries: 2,
            },
            llm,
        );
        (tmp, engine)
    }

    #[test]
    fn should_run_false_when_few_summaries() {
        let (tmp, engine) = make_engine("");
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let summary_store = SessionSummaryStore::new(storage);

        // 只存 1 条（低于阈值 2）
        summary_store
            .save(&SessionSummary {
                session_id: "sess-1".into(),
                session_start: Utc::now(),
                session_end: Utc::now(),
                fact_summary: "测试".into(),
                tags: vec![],
                pitfalls: vec![],
                decisions: vec![],
                archived: false,
                superseded: false,
                created_at: Utc::now(),
            })
            .unwrap();

        assert!(!engine.should_run().unwrap());
    }

    #[test]
    fn should_run_true_when_enough_summaries() {
        let (tmp, engine) = make_engine("");
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        let summary_store = SessionSummaryStore::new(storage);

        for i in 0..3 {
            summary_store
                .save(&SessionSummary {
                    session_id: format!("sess-{i}"),
                    session_start: Utc::now(),
                    session_end: Utc::now(),
                    fact_summary: format!("测试{i}"),
                    tags: vec![],
                    pitfalls: vec![],
                    decisions: vec![],
                    archived: false,
                    superseded: false,
                    created_at: Utc::now(),
                })
                .unwrap();
        }

        assert!(engine.should_run().unwrap());
    }

    #[test]
    fn extract_json_from_array() {
        let result = extract_json("some text [{\"a\":1}] more");
        assert!(result.starts_with('['));
    }

    #[test]
    fn extract_json_from_object() {
        let result = extract_json("prefix {\"a\":1} suffix");
        assert!(result.starts_with('{'));
    }
}
