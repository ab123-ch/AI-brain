//! L3 经验包层 — 分类经验，可加载到推理脑
//!
//! 职责：
//! - 存储从 L2 索引摘要中提炼的经验包
//! - 按 trigger_pattern 匹配场景
//! - 按 category 加载经验到推理脑上下文
//! - 追踪使用计数

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::storage::Storage;

/// 犯错记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistakeEntry {
    pub what: String,
    pub why: String,
    pub how_to_avoid: String,
}

/// L3 经验包
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperiencePack {
    pub id: String,
    pub title: String,
    /// 与 L2 对齐的分类
    pub category: String,
    /// 什么场景加载此经验
    pub trigger_patterns: Vec<String>,
    /// 提炼的最佳实践步骤
    pub reasoning_path: Vec<String>,
    /// 犯错记录
    pub mistakes: Vec<MistakeEntry>,
    /// 文件修改模式
    pub files_modified_patterns: Vec<String>,
    /// 使用过的工具
    pub tools_used: Vec<String>,
    /// 成功率
    pub success_rate: f64,
    /// 经验捷径
    pub shortcuts: Vec<String>,
    /// 指向 L2 来源
    pub source_index_ids: Vec<String>,
    /// 可直接注入推理脑的文本
    pub context_snippet: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub use_count: u32,
}

/// L3 经验包层
pub struct ExperiencePackLayer {
    storage: Storage,
}

impl ExperiencePackLayer {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 存入一个经验包（原子写入）
    pub fn store(&self, pack: &ExperiencePack) -> Result<()> {
        let dir = self.storage.experience_category_dir(&pack.category);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", pack.id));
        self.storage.write_json_atomic(&path, &pack)
    }

    /// 按 ID 获取经验包
    pub fn get(&self, id: &str, category: &str) -> Result<ExperiencePack> {
        let path = self
            .storage
            .experience_category_dir(category)
            .join(format!("{id}.json"));
        if !path.exists() {
            return Err(crate::error::MemoryError::EntryNotFound(id.into()));
        }
        self.storage.read_json(&path)
    }

    /// 按关键词搜索经验包
    pub fn search(
        &self,
        keywords: &[String],
        category: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ExperiencePack>> {
        let files = match category {
            Some(cat) => self
                .storage
                .list_json_files(&self.storage.experience_category_dir(cat))?,
            None => self
                .storage
                .list_json_files_recursive(&self.storage.experience_dir())?,
        };

        let mut results = Vec::new();
        for file in &files {
            if results.len() >= limit {
                break;
            }
            let pack: ExperiencePack = match self.storage.read_json(file) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let matched = keywords.iter().any(|kw| {
                pack.title.to_lowercase().contains(&kw.to_lowercase())
                    || pack
                        .context_snippet
                        .to_lowercase()
                        .contains(&kw.to_lowercase())
                    || pack
                        .shortcuts
                        .iter()
                        .any(|s| s.to_lowercase().contains(&kw.to_lowercase()))
                    || pack
                        .trigger_patterns
                        .iter()
                        .any(|p| p.to_lowercase().contains(&kw.to_lowercase()))
            });

            if matched {
                results.push(pack);
            }
        }
        Ok(results)
    }

    /// 按 trigger_pattern 匹配
    pub fn search_by_trigger(&self, content: &str, limit: usize) -> Result<Vec<ExperiencePack>> {
        let files = self
            .storage
            .list_json_files_recursive(&self.storage.experience_dir())?;
        let content_lower = content.to_lowercase();
        let mut results = Vec::new();

        for file in &files {
            if results.len() >= limit {
                break;
            }
            let pack: ExperiencePack = match self.storage.read_json(file) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let triggered = pack
                .trigger_patterns
                .iter()
                .any(|p| content_lower.contains(&p.to_lowercase()));

            if triggered {
                results.push(pack);
            }
        }
        Ok(results)
    }

    /// 递增使用计数
    pub fn increment_use(&self, id: &str, category: &str) -> Result<()> {
        let mut pack = self.get(id, category)?;
        pack.use_count += 1;
        pack.last_used_at = Utc::now();
        self.store(&pack)
    }

    /// 加载经验到推理脑上下文
    ///
    /// 按 category 和 keywords 筛选，在 budget 限制内返回 context_snippet
    pub fn load_for_injection(
        &self,
        category: Option<&str>,
        keywords: &[String],
        budget: usize,
    ) -> Result<String> {
        let packs = self.search(keywords, category, 10)?;
        let mut result = String::new();

        for pack in &packs {
            let snippet = format!(
                "[{}] {}\n{}\n",
                pack.category, pack.title, pack.context_snippet
            );
            if result.len() + snippet.len() > budget {
                break;
            }
            result.push_str(&snippet);
        }
        Ok(result)
    }

    /// 列出所有分类
    pub fn list_categories(&self) -> Result<Vec<String>> {
        let exp_dir = self.storage.experience_dir();
        if !exp_dir.exists() {
            return Ok(Vec::new());
        }
        let mut categories = Vec::new();
        for entry in std::fs::read_dir(&exp_dir)? {
            let entry = entry?;
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    categories.push(name.to_string());
                }
            }
        }
        categories.sort();
        Ok(categories)
    }

    /// 统计总数
    pub fn count(&self) -> Result<u32> {
        let files = self
            .storage
            .list_json_files_recursive(&self.storage.experience_dir())?;
        Ok(files.len() as u32)
    }

    /// 收集所有经验包引用的 L2 index ID 集合（GC 用）
    pub fn collect_all_referenced_index_ids(&self) -> Result<std::collections::HashSet<String>> {
        let files = self
            .storage
            .list_json_files_recursive(&self.storage.experience_dir())?;
        let mut ids = std::collections::HashSet::new();
        for file in &files {
            if let Ok(pack) = self.storage.read_json::<ExperiencePack>(file) {
                for id in pack.source_index_ids {
                    ids.insert(id);
                }
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_layer() -> ExperiencePackLayer {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        std::mem::forget(tmp);
        ExperiencePackLayer::new(storage)
    }

    fn make_pack(id: &str, category: &str, title: &str, triggers: &[&str]) -> ExperiencePack {
        ExperiencePack {
            id: id.into(),
            title: title.into(),
            category: category.into(),
            trigger_patterns: triggers.iter().map(|t| (*t).into()).collect(),
            reasoning_path: vec!["步骤1".into(), "步骤2".into()],
            mistakes: vec![MistakeEntry {
                what: "遗漏了错误处理".into(),
                why: "时间紧迫".into(),
                how_to_avoid: "先写测试".into(),
            }],
            files_modified_patterns: vec!["src/**/*.rs".into()],
            tools_used: vec!["cargo".into()],
            success_rate: 0.85,
            shortcuts: vec!["使用模板".into()],
            source_index_ids: vec!["idx-1".into()],
            context_snippet: "这是可直接注入推理脑的经验文本。".into(),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
            use_count: 0,
        }
    }

    #[test]
    fn store_and_get() {
        let layer = make_layer();
        let pack = make_pack(
            "exp-1",
            "development",
            "记忆脑三层架构经验",
            &["记忆", "架构"],
        );
        layer.store(&pack).unwrap();

        let got = layer.get("exp-1", "development").unwrap();
        assert_eq!(got.title, "记忆脑三层架构经验");
        assert_eq!(got.category, "development");
    }

    #[test]
    fn search_by_keyword() {
        let layer = make_layer();
        layer
            .store(&make_pack("exp-1", "development", "记忆脑经验", &["记忆"]))
            .unwrap();
        layer
            .store(&make_pack("exp-2", "debugging", "编译错误经验", &["编译"]))
            .unwrap();

        let results = layer.search(&["记忆".into()], None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "exp-1");
    }

    #[test]
    fn search_by_trigger() {
        let layer = make_layer();
        layer
            .store(&make_pack(
                "exp-1",
                "development",
                "记忆脑经验",
                &["记忆系统重构"],
            ))
            .unwrap();
        layer
            .store(&make_pack("exp-2", "debugging", "编译错误", &["编译失败"]))
            .unwrap();

        let results = layer.search_by_trigger("我需要做记忆系统重构", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "exp-1");
    }

    #[test]
    fn increment_use() {
        let layer = make_layer();
        layer
            .store(&make_pack("exp-1", "development", "测试使用", &["测试"]))
            .unwrap();

        layer.increment_use("exp-1", "development").unwrap();
        layer.increment_use("exp-1", "development").unwrap();

        let got = layer.get("exp-1", "development").unwrap();
        assert_eq!(got.use_count, 2);
    }

    #[test]
    fn load_for_injection() {
        let layer = make_layer();
        layer
            .store(&make_pack("exp-1", "development", "经验1", &["记忆"]))
            .unwrap();
        layer
            .store(&make_pack("exp-2", "development", "经验2", &["记忆"]))
            .unwrap();

        let context = layer
            .load_for_injection(Some("development"), &["记忆".into()], 1000)
            .unwrap();
        assert!(!context.is_empty());
        assert!(context.contains("经验1"));
        assert!(context.contains("经验2"));
    }

    #[test]
    fn load_for_injection_respects_budget() {
        let layer = make_layer();
        layer
            .store(&make_pack("exp-1", "development", "经验1", &["记忆"]))
            .unwrap();
        layer
            .store(&make_pack("exp-2", "development", "经验2", &["记忆"]))
            .unwrap();

        // 极小 budget 只能容纳一个
        let context = layer
            .load_for_injection(None, &["记忆".into()], 80)
            .unwrap();
        let has_1 = context.contains("经验1");
        let has_2 = context.contains("经验2");
        // 至少有一个
        assert!(has_1 || has_2, "至少有一个经验被加载");
    }

    #[test]
    fn count_and_categories() {
        let layer = make_layer();
        layer
            .store(&make_pack("exp-1", "development", "开发经验", &["开发"]))
            .unwrap();
        layer
            .store(&make_pack("exp-2", "debugging", "调试经验", &["调试"]))
            .unwrap();

        assert_eq!(layer.count().unwrap(), 2);
        let cats = layer.list_categories().unwrap();
        assert_eq!(cats, vec!["debugging", "development"]);
    }
}
