//! 潜意识层（印象索引）
//!
//! 职责：
//! - 三层渐进披露的最外层：潜意识记忆 → 踩坑摘要 → L2/L3 具体引用
//! - 只存"我做过这件事"+"有个坑大概是这样"，不存结论和教训
//! - 匹配到潜意识 → 通过 reference 深入 L2/L3 获取完整细节
//! - 匹配不到说明可能没有做过这种事情
//!
//! 存储路径：`memory/subconscious/`
//!
//! 示例条目：
//!   topic: "Claude Code配置"
//!   trigger_keywords: ["配置文件", "settings", "Claude Code"]
//!   impression: "了解过Claude Code配置"           ← 极简：我做过这事
//!   pitfall_hint: "配置文件有多个，改错了文件"     ← 直觉：有个坑
//!   reference_hint: "pitfall/pt-1745xxx.json"    ← 精确引用

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Storage;

/// 一条潜意识（印象）条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubconsciousEntry {
    pub id: String,
    /// 主题领域（如"Claude Code配置"、"记忆脑开发"）
    pub topic: String,
    /// 触发关键词（匹配用，渐进式披露的入口）
    pub trigger_keywords: Vec<String>,
    /// 极简印象：只说"了解过/做过/踩过坑"（如"了解过Claude Code配置"）
    pub impression: String,
    /// 踩坑摘要：直觉级，一句话说坑在哪（如"配置文件有多个，改错了"）
    #[serde(default)]
    pub pitfall_hint: String,
    /// 精确引用：指向 L2/L3 具体文件（如"pitfall/pt-1745xxx.json"）
    pub reference_hint: String,
    /// 重要度 0.0~1.0
    pub importance: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 被后续记忆迭代取代（不再召回，保留审计）
    #[serde(default)]
    pub superseded: bool,
    /// 最后被召回访问的时间（用于衰减计算）
    #[serde(default = "Utc::now")]
    pub last_accessed: DateTime<Utc>,
}

/// 潜意识层存储管理器
pub struct SubconsciousStore {
    storage: Storage,
}

impl SubconsciousStore {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    fn subconscious_dir(&self) -> std::path::PathBuf {
        self.storage.base_dir().join("memory").join("subconscious")
    }

    /// 加载所有潜意识条目
    pub fn load_all(&self) -> Result<Vec<SubconsciousEntry>> {
        let dir = self.subconscious_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let files = self.storage.list_json_files(&dir)?;
        let mut entries = Vec::new();
        for file in &files {
            if let Ok(entry) = self.storage.read_json::<SubconsciousEntry>(file) {
                entries.push(entry);
            }
        }
        // 按 importance 降序
        entries.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(entries)
    }

    /// 按关键词匹配潜意识条目（渐进式披露入口）
    ///
    /// 返回与任一 keyword 匹配的条目（trigger_keywords 中包含该 keyword）
    pub fn match_keywords(
        &self,
        keywords: &[String],
        limit: usize,
    ) -> Result<Vec<SubconsciousEntry>> {
        let all = self.load_recallable()?;
        let mut matched = Vec::new();
        for entry in &all {
            if matched.len() >= limit {
                break;
            }
            // 任一 keyword 命中任一 trigger_keyword 即匹配
            let hit = keywords.iter().any(|kw| {
                let kw_lower = kw.to_lowercase();
                entry
                    .trigger_keywords
                    .iter()
                    .any(|tk| tk.to_lowercase() == kw_lower)
                    || entry.topic.to_lowercase().contains(&kw_lower)
                    || entry.impression.to_lowercase().contains(&kw_lower)
            });
            if hit {
                matched.push(entry.clone());
            }
        }
        Ok(matched)
    }

    /// 合并新的潜意识条目
    ///
    /// 策略：按 topic 模糊匹配
    /// - 匹配到已有条目：更新 impression 和 trigger_keywords（合并去重）
    /// - 未匹配：新建条目
    pub fn merge_entries(
        &self,
        new_entries: &[NewSubconsciousEntry],
    ) -> Result<Vec<SubconsciousEntry>> {
        let mut existing = self.load_all()?;
        let mut result = Vec::new();

        for new in new_entries {
            // 尝试匹配已有条目（topic 关键词有重叠）
            let matched_idx = existing
                .iter()
                .position(|e| topics_overlap(&e.topic, &new.topic));

            if let Some(idx) = matched_idx {
                // 合并：更新 impression/pitfall_hint，合并 keywords
                let entry = &mut existing[idx];
                entry.impression = new.impression.clone();
                entry.pitfall_hint = new.pitfall_hint.clone();
                entry.reference_hint = new.reference_hint.clone();
                for kw in &new.trigger_keywords {
                    if !entry
                        .trigger_keywords
                        .iter()
                        .any(|k| k.eq_ignore_ascii_case(kw))
                    {
                        entry.trigger_keywords.push(kw.clone());
                    }
                }
                // 保留最多 10 个触发词
                entry.trigger_keywords.truncate(10);
                entry.importance = entry.importance.max(new.importance);
                entry.updated_at = Utc::now();
                let cloned = entry.clone();
                self.store(&cloned)?;
                result.push(cloned);
            } else {
                // 新建
                let entry = SubconsciousEntry {
                    id: format!("sc-{}", Utc::now().timestamp_millis()),
                    topic: new.topic.clone(),
                    trigger_keywords: new.trigger_keywords.clone(),
                    impression: new.impression.clone(),
                    pitfall_hint: new.pitfall_hint.clone(),
                    reference_hint: new.reference_hint.clone(),
                    importance: new.importance,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    superseded: false,
                    last_accessed: Utc::now(),
                };
                let cloned = entry.clone();
                self.store(&entry)?;
                existing.push(entry);
                result.push(cloned);
            }
        }

        Ok(result)
    }

    pub fn store(&self, entry: &SubconsciousEntry) -> Result<()> {
        let dir = self.subconscious_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", entry.id));
        self.storage.write_json_atomic(&path, &entry)
    }

    /// 统计条目数
    pub fn count(&self) -> Result<u32> {
        Ok(self.load_all()?.len() as u32)
    }

    /// 加载可召回的条目（过滤 superseded）
    pub fn load_recallable(&self) -> Result<Vec<SubconsciousEntry>> {
        Ok(self
            .load_all()?
            .into_iter()
            .filter(|e| !e.superseded)
            .collect())
    }

    /// 标记条目为已取代
    pub fn mark_superseded(&self, id: &str) -> Result<()> {
        let all = self.load_all()?;
        for mut entry in all {
            if entry.id == id {
                entry.superseded = true;
                entry.updated_at = Utc::now();
                self.store(&entry)?;
                return Ok(());
            }
        }
        Ok(())
    }

    /// 更新条目 importance
    pub fn update_importance(&self, id: &str, importance: f64) -> Result<()> {
        let all = self.load_all()?;
        for mut entry in all {
            if entry.id == id {
                entry.importance = importance;
                entry.updated_at = Utc::now();
                self.store(&entry)?;
                return Ok(());
            }
        }
        Ok(())
    }

    /// 更新条目 last_accessed
    pub fn touch_accessed(&self, id: &str) -> Result<()> {
        let all = self.load_all()?;
        for mut entry in all {
            if entry.id == id {
                entry.last_accessed = Utc::now();
                self.store(&entry)?;
                return Ok(());
            }
        }
        Ok(())
    }
}

/// 新潜意识条目输入（由 LLM 分析产出）
#[derive(Debug, Clone, Serialize)]
pub struct NewSubconsciousEntry {
    pub topic: String,
    pub trigger_keywords: Vec<String>,
    /// 极简印象："了解过/做过X"
    pub impression: String,
    /// 踩坑摘要：一句话说坑在哪（可为空）
    pub pitfall_hint: String,
    /// 精确引用：指向 L2/L3 具体文件
    pub reference_hint: String,
    pub importance: f64,
}

/// 判断两个 topic 是否有重叠（任一 2+ 字片段出现在对方中）
fn topics_overlap(a: &str, b: &str) -> bool {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    // 完全包含
    if a_lower.contains(&b_lower) || b_lower.contains(&a_lower) {
        return true;
    }

    // 滑动窗口 2-4 字匹配
    let shorter = if a.chars().count() < b.chars().count() {
        &a_lower
    } else {
        &b_lower
    };
    let longer = if a.chars().count() < b.chars().count() {
        &b_lower
    } else {
        &a_lower
    };
    let chars: Vec<char> = shorter.chars().collect();

    for window in [4, 3, 2] {
        if chars.len() < window {
            continue;
        }
        for i in 0..=chars.len() - window {
            let fragment: String = chars[i..i + window].iter().collect();
            if longer.contains(&fragment) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, SubconsciousStore) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        (tmp, SubconsciousStore::new(storage))
    }

    #[test]
    fn store_and_load() {
        let (_tmp, store) = make_store();
        let entry = SubconsciousEntry {
            id: "sc-1".into(),
            topic: "记忆脑开发".into(),
            trigger_keywords: vec!["记忆脑".into(), "L2".into()],
            impression: "做过记忆脑开发".into(),
            pitfall_hint: "架构设计反复迭代".into(),
            reference_hint: "pitfall/".into(),
            importance: 0.9,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            superseded: false,
            last_accessed: Utc::now(),
        };
        store.store(&entry).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].topic, "记忆脑开发");
        assert_eq!(all[0].pitfall_hint, "架构设计反复迭代");
    }

    #[test]
    fn match_keywords_hit() {
        let (_tmp, store) = make_store();
        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "记忆脑开发".into(),
                trigger_keywords: vec!["记忆脑".into(), "L2".into()],
                impression: "做过记忆脑开发".into(),
                pitfall_hint: String::new(),
                reference_hint: "pitfall/".into(),
                importance: 0.9,
            }])
            .unwrap();

        let matched = store.match_keywords(&["记忆脑".into()], 10).unwrap();
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn match_keywords_miss() {
        let (_tmp, store) = make_store();
        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "记忆脑开发".into(),
                trigger_keywords: vec!["记忆脑".into()],
                impression: "做过记忆脑开发".into(),
                pitfall_hint: String::new(),
                reference_hint: "pitfall/".into(),
                importance: 0.9,
            }])
            .unwrap();

        let matched = store.match_keywords(&["量子计算".into()], 10).unwrap();
        assert!(matched.is_empty());
    }

    #[test]
    fn deserialize_old_json_without_new_fields() {
        // 模拟旧数据：没有 superseded、last_accessed、pitfall_hint 字段
        let json = r#"{"id":"sc-old","topic":"旧话题","trigger_keywords":["旧"],"impression":"旧印象","reference_hint":"","importance":0.5,"created_at":"2026-04-24T13:47:33.866383Z","updated_at":"2026-04-25T08:17:48.093285Z"}"#;
        let entry: SubconsciousEntry = serde_json::from_str(json).unwrap();
        assert!(!entry.superseded, "superseded should default to false");
        assert!(
            entry.last_accessed.timestamp() > 0,
            "last_accessed should be set"
        );
        assert!(
            entry.pitfall_hint.is_empty(),
            "pitfall_hint should default to empty"
        );
    }

    #[test]
    fn merge_updates_existing() {
        let (_tmp, store) = make_store();
        store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "记忆脑开发".into(),
                trigger_keywords: vec!["记忆脑".into()],
                impression: "了解过记忆脑".into(),
                pitfall_hint: String::new(),
                reference_hint: "pitfall/".into(),
                importance: 0.8,
            }])
            .unwrap();

        let result = store
            .merge_entries(&[NewSubconsciousEntry {
                topic: "记忆脑开发".into(),
                trigger_keywords: vec!["L2".into(), "短期记忆".into()],
                impression: "开发过记忆脑".into(),
                pitfall_hint: "四层架构反复迭代".into(),
                reference_hint: "pitfall/".into(),
                importance: 0.95,
            }])
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(store.count().unwrap(), 1); // 没有新增
        assert!(result[0].trigger_keywords.contains(&"记忆脑".to_string()));
        assert!(result[0].trigger_keywords.contains(&"L2".to_string()));
        assert_eq!(result[0].impression, "开发过记忆脑");
        assert_eq!(result[0].pitfall_hint, "四层架构反复迭代");
    }

    #[test]
    fn topics_overlap_works() {
        assert!(topics_overlap("记忆脑开发", "记忆脑"));
        assert!(topics_overlap("Ship-Core需求开发", "Ship需求开发"));
        assert!(!topics_overlap("记忆脑开发", "量子计算"));
    }
}
