//! 潜意识层（流动叙事）
//!
//! 职责：
//! - 维护一段流动叙事文本，描述用户做过什么、踩过什么坑
//! - 由 LLM 负责合并/覆盖/追加决策
//! - 追求最少上下文占用 × 最大触发覆盖面
//!
//! 存储路径：`memory/subconscious/narrative.json`（单文件）
//!
//! 旧数据迁移：`load()` 检测无 narrative.json 但有 sc-*.json 时，自动融合为叙事文本

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Storage;

/// 叙事文本最大长度（字）
const MAX_NARRATIVE_CHARS: usize = 500;

/// 流动叙事数据模型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubconsciousNarrative {
    /// 流动叙事文本（"我做过X，做过Y，比对过Z"）
    pub narrative: String,
    /// 扁平关键词列表（用于快速匹配）
    pub trigger_keywords: Vec<String>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub version: u32,
}

/// LLM 产出的叙事更新
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrativeUpdate {
    /// 更新后的完整叙事（空字符串表示不更新）
    pub narrative: String,
    /// 新增关键词
    pub new_keywords: Vec<String>,
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

    fn narrative_path(&self) -> std::path::PathBuf {
        self.subconscious_dir().join("narrative.json")
    }

    /// 加载叙事（自动迁移旧数据）
    pub fn load(&self) -> Result<Option<SubconsciousNarrative>> {
        let path = self.narrative_path();
        if path.exists() {
            let narrative = self.storage.read_json::<SubconsciousNarrative>(&path)?;
            return Ok(Some(narrative));
        }

        // 尝试从旧格式迁移
        self.migrate_from_old_entries()
    }

    /// 保存叙事
    pub fn save(&self, narrative: &SubconsciousNarrative) -> Result<()> {
        let dir = self.subconscious_dir();
        std::fs::create_dir_all(&dir)?;
        let path = self.narrative_path();
        self.storage.write_json_atomic(&path, narrative)
    }

    /// 更新叙事：合并关键词去重 + 截断 + version++
    pub fn update(&self, update: &NarrativeUpdate) -> Result<()> {
        let mut current = match self.load()? {
            Some(n) => n,
            None => SubconsciousNarrative {
                narrative: String::new(),
                trigger_keywords: Vec::new(),
                updated_at: Utc::now(),
                created_at: Utc::now(),
                version: 0,
            },
        };

        // 合并关键词（去重，不区分大小写）
        for kw in &update.new_keywords {
            let kw_lower = kw.to_lowercase();
            if !current
                .trigger_keywords
                .iter()
                .any(|k| k.to_lowercase() == kw_lower)
            {
                current.trigger_keywords.push(kw.clone());
            }
        }

        // 更新叙事文本
        current.narrative = truncate_chars(&update.narrative, MAX_NARRATIVE_CHARS);
        current.updated_at = Utc::now();
        current.version += 1;

        self.save(&current)
    }

    /// 关键词匹配（trigger_keywords + 叙事文本双重匹配）
    pub fn match_keywords(&self, keywords: &[String]) -> Result<bool> {
        let Some(narrative) = self.load()? else {
            return Ok(false);
        };

        for kw in keywords {
            let kw_lower = kw.to_lowercase();
            // 检查 trigger_keywords
            if narrative
                .trigger_keywords
                .iter()
                .any(|k| k.to_lowercase().contains(&kw_lower))
            {
                return Ok(true);
            }
            // 检查叙事文本
            if narrative.narrative.to_lowercase().contains(&kw_lower) {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// 统计（兼容接口，返回 0 或 1）
    pub fn count(&self) -> Result<u32> {
        Ok(u32::from(self.load()?.is_some()))
    }

    /// 从旧格式（sc-*.json 多条目）迁移为叙事文本
    #[allow(clippy::option_if_let_else)]
    fn migrate_from_old_entries(&self) -> Result<Option<SubconsciousNarrative>> {
        let dir = self.subconscious_dir();
        if !dir.exists() {
            return Ok(None);
        }

        let files = self.storage.list_json_files(&dir)?;
        let sc_files: Vec<_> = files
            .iter()
            .filter(|f| {
                let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.starts_with("sc-") && name.ends_with(".json")
            })
            .collect();

        if sc_files.is_empty() {
            return Ok(None);
        }

        // 解析旧条目
        #[derive(Deserialize)]
        struct OldEntry {
            #[serde(default)]
            topic: String,
            #[serde(default)]
            impression: String,
            #[serde(default)]
            pitfall_hint: String,
            #[serde(default)]
            trigger_keywords: Vec<String>,
            #[serde(default)]
            superseded: bool,
        }

        let mut active_topics = Vec::new();
        let mut all_keywords = Vec::new();

        for file in &sc_files {
            if let Ok(old) = self.storage.read_json::<OldEntry>(file) {
                if old.superseded {
                    continue;
                }
                if !old.topic.is_empty() || !old.impression.is_empty() {
                    let mut desc = String::new();
                    if !old.impression.is_empty() {
                        desc.push_str(&old.impression);
                    }
                    if !old.pitfall_hint.is_empty() {
                        desc.push_str("，踩过");
                        desc.push_str(&old.pitfall_hint);
                    }
                    active_topics.push(desc);
                }
                for kw in old.trigger_keywords {
                    let kw_lower = kw.to_lowercase();
                    if !all_keywords
                        .iter()
                        .any(|k: &String| k.to_lowercase() == kw_lower)
                    {
                        all_keywords.push(kw);
                    }
                }
            }
        }

        if active_topics.is_empty() {
            return Ok(None);
        }

        // 融合为叙事文本
        let narrative = truncate_chars(&active_topics.join("，"), MAX_NARRATIVE_CHARS);
        let migrated = SubconsciousNarrative {
            narrative,
            trigger_keywords: all_keywords,
            updated_at: Utc::now(),
            created_at: Utc::now(),
            version: 1,
        };

        // 保存新格式（旧文件保留不删除）
        self.save(&migrated)?;

        Ok(Some(migrated))
    }
}

/// 按字符数截断
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
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

    fn make_narrative(text: &str) -> SubconsciousNarrative {
        SubconsciousNarrative {
            narrative: text.to_string(),
            trigger_keywords: vec!["测试".to_string()],
            updated_at: Utc::now(),
            created_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn narrative_save_and_load() {
        let (_tmp, store) = make_store();
        let n = make_narrative("做过记忆脑开发");
        store.save(&n).unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.narrative, "做过记忆脑开发");
        assert_eq!(loaded.trigger_keywords, vec!["测试"]);
        assert_eq!(loaded.version, 0);
    }

    #[test]
    fn narrative_load_nonexistent() {
        let (_tmp, store) = make_store();
        let result = store.load().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn narrative_update_merges_keywords() {
        let (_tmp, store) = make_store();
        let n = make_narrative("初始叙事");
        store.save(&n).unwrap();

        store
            .update(&NarrativeUpdate {
                narrative: "更新叙事".into(),
                new_keywords: vec!["新词".into(), "测试".into()], // "测试" 已存在，应去重
            })
            .unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.trigger_keywords.len(), 2); // "测试" + "新词"
        assert!(loaded.trigger_keywords.contains(&"测试".to_string()));
        assert!(loaded.trigger_keywords.contains(&"新词".to_string()));
    }

    #[test]
    fn narrative_update_increments_version() {
        let (_tmp, store) = make_store();
        let n = make_narrative("初始");
        store.save(&n).unwrap();
        assert_eq!(store.load().unwrap().unwrap().version, 0);

        store
            .update(&NarrativeUpdate {
                narrative: "第一次更新".into(),
                new_keywords: vec![],
            })
            .unwrap();
        assert_eq!(store.load().unwrap().unwrap().version, 1);

        store
            .update(&NarrativeUpdate {
                narrative: "第二次更新".into(),
                new_keywords: vec![],
            })
            .unwrap();
        assert_eq!(store.load().unwrap().unwrap().version, 2);
    }

    #[test]
    fn narrative_truncation_protection() {
        let (_tmp, store) = make_store();
        let long_narrative: String = "很".repeat(600);
        store
            .update(&NarrativeUpdate {
                narrative: long_narrative.clone(),
                new_keywords: vec![],
            })
            .unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.narrative.chars().count(), MAX_NARRATIVE_CHARS);
    }

    #[test]
    fn match_keywords_hit_on_keyword() {
        let (_tmp, store) = make_store();
        let mut n = make_narrative("做过记忆脑开发");
        n.trigger_keywords = vec!["记忆脑".into(), "Rust".into()];
        store.save(&n).unwrap();

        assert!(store.match_keywords(&["记忆脑".to_string()]).unwrap());
    }

    #[test]
    fn match_keywords_hit_on_narrative_text() {
        let (_tmp, store) = make_store();
        let n = SubconsciousNarrative {
            narrative: "优化过网文写作提示词".into(),
            trigger_keywords: vec!["写作".into()],
            updated_at: Utc::now(),
            created_at: Utc::now(),
            version: 0,
        };
        store.save(&n).unwrap();

        // "提示词" 在叙事文本中但不在 trigger_keywords 中
        assert!(store.match_keywords(&["提示词".to_string()]).unwrap());
    }

    #[test]
    fn match_keywords_miss() {
        let (_tmp, store) = make_store();
        let n = make_narrative("做过记忆脑开发");
        store.save(&n).unwrap();

        assert!(!store.match_keywords(&["量子计算".to_string()]).unwrap());
    }

    #[test]
    fn migrate_from_old_entries() {
        let (_tmp, store) = make_store();
        let dir = store.subconscious_dir();
        std::fs::create_dir_all(&dir).unwrap();

        // 创建旧格式文件
        let old1 = serde_json::json!({
            "id": "sc-001",
            "topic": "记忆输出规则",
            "trigger_keywords": ["记忆", "输出"],
            "impression": "了解过记忆输出规则",
            "pitfall_hint": "",
            "reference_hint": "",
            "importance": 0.8,
            "superseded": false,
            "created_at": "2026-04-24T00:00:00Z",
            "updated_at": "2026-04-24T00:00:00Z"
        });
        let old2 = serde_json::json!({
            "id": "sc-002",
            "topic": "Claude Code技能系统",
            "trigger_keywords": ["Claude Code", "技能"],
            "impression": "了解过Claude Code技能系统",
            "pitfall_hint": "技能加载有坑",
            "reference_hint": "",
            "importance": 0.9,
            "superseded": false,
            "created_at": "2026-04-25T00:00:00Z",
            "updated_at": "2026-04-25T00:00:00Z"
        });
        std::fs::write(
            dir.join("sc-001.json"),
            serde_json::to_string(&old1).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("sc-002.json"),
            serde_json::to_string(&old2).unwrap(),
        )
        .unwrap();

        let result = store.load().unwrap().unwrap();
        assert!(!result.narrative.is_empty());
        assert!(result.narrative.contains("记忆输出规则"));
        assert!(result.narrative.contains("技能加载有坑"));
        assert!(result.trigger_keywords.contains(&"记忆".to_string()));
        assert!(result.trigger_keywords.contains(&"技能".to_string()));
        assert_eq!(result.version, 1);
    }

    #[test]
    fn migrate_skips_superseded() {
        let (_tmp, store) = make_store();
        let dir = store.subconscious_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let active = serde_json::json!({
            "id": "sc-active",
            "topic": "活跃主题",
            "trigger_keywords": ["活跃"],
            "impression": "活跃印象",
            "pitfall_hint": "",
            "reference_hint": "",
            "importance": 0.7,
            "superseded": false,
            "created_at": "2026-04-24T00:00:00Z",
            "updated_at": "2026-04-24T00:00:00Z"
        });
        let superseded = serde_json::json!({
            "id": "sc-dead",
            "topic": "已废弃",
            "trigger_keywords": ["废弃"],
            "impression": "废弃印象",
            "pitfall_hint": "",
            "reference_hint": "",
            "importance": 0.3,
            "superseded": true,
            "created_at": "2026-04-20T00:00:00Z",
            "updated_at": "2026-04-20T00:00:00Z"
        });
        std::fs::write(
            dir.join("sc-active.json"),
            serde_json::to_string(&active).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("sc-dead.json"),
            serde_json::to_string(&superseded).unwrap(),
        )
        .unwrap();

        let result = store.load().unwrap().unwrap();
        assert!(result.narrative.contains("活跃印象"));
        assert!(!result.narrative.contains("废弃"));
        assert!(!result.trigger_keywords.contains(&"废弃".to_string()));
    }

    #[test]
    fn deserialize_narrative_without_version() {
        let json = r#"{"narrative":"测试","trigger_keywords":["test"],"updated_at":"2026-05-01T00:00:00Z","created_at":"2026-05-01T00:00:00Z"}"#;
        let n: SubconsciousNarrative = serde_json::from_str(json).unwrap();
        assert_eq!(n.version, 0);
        assert_eq!(n.narrative, "测试");
    }
}
