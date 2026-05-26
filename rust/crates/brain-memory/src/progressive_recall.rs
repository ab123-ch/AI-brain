//! 渐进式召回引擎
//!
//! 自顶向下渐进召回：L4(触发词) → L3(经验) → L2(任务摘要) → L1(原始记忆)
//! LLM 驱动，每层自主决定是否继续深入。

use crate::abstract_layer::AbstractLayer;
use crate::error::Result;
use crate::profile_eval::ProfileStore;
use crate::pyramid_storage::PyramidStorage;
use crate::pyramid_types::{Experience, PyramidLayer};
use crate::subconscious_pool::SubconsciousPool;

/// 召回命中结果
#[derive(Debug, Clone)]
pub struct RecallHit {
    /// 命中的层级
    pub layer: PyramidLayer,
    /// 命中的内容
    pub content: String,
    /// 来源标识（如 task_id, type_name, session_id）
    pub source: String,
}

/// 自动注入内容（启动时注入，无需 LLM 调用）
#[derive(Debug, Clone)]
pub struct InjectContext {
    /// L4 潜意识注入文本
    pub subconscious_text: String,
    /// 用户画像
    pub profile: String,
    /// 可注入的经验规则
    pub injectable_experiences: Vec<Experience>,
}

/// 渐进式召回引擎
pub struct ProgressiveRecall {
    storage: PyramidStorage,
}

impl ProgressiveRecall {
    /// 创建 ProgressiveRecall
    pub fn new(storage: PyramidStorage) -> Self {
        Self { storage }
    }

    /// 自动注入内容（潜意识 + 画像 + injectable 经验）
    pub fn auto_inject(&self) -> Result<InjectContext> {
        let subconscious = SubconsciousPool::new(self.storage.clone());
        let profile_store = ProfileStore::new(self.storage.clone());
        let abstract_layer = AbstractLayer::new(self.storage.clone());

        let subconscious_text = subconscious.inject_text()?;
        let profile = profile_store.summary()?;
        let injectable = abstract_layer.load_injectable()?;

        Ok(InjectContext {
            subconscious_text,
            profile,
            injectable_experiences: injectable,
        })
    }

    /// 生成注入文本（用于 system prompt）
    pub fn build_inject_text(&self) -> Result<String> {
        let ctx = self.auto_inject()?;
        let mut parts = Vec::new();

        if !ctx.subconscious_text.is_empty() {
            parts.push(ctx.subconscious_text);
        }

        if !ctx.profile.is_empty() {
            parts.push(format!("[用户画像] {}", ctx.profile));
        }

        if !ctx.injectable_experiences.is_empty() {
            let exp_text: Vec<String> = ctx
                .injectable_experiences
                .iter()
                .map(|e| format!("- {}: {}", e.pattern, e.description))
                .collect();
            parts.push(format!("[核心经验]\n{}", exp_text.join("\n")));
        }

        Ok(parts.join("\n\n"))
    }

    /// 渐进式召回（自顶向下）
    pub fn recall(&self, query: &str, max_depth: PyramidLayer) -> Result<Vec<RecallHit>> {
        let mut hits = Vec::new();

        // L4 触发词匹配
        let l4_matches = self.match_triggers(query)?;
        hits.extend(l4_matches);
        if max_depth == PyramidLayer::Subconscious {
            return Ok(hits);
        }

        // L3 经验查找
        let l3_matches = self.find_in_abstract(&hits)?;
        hits.extend(l3_matches);
        if max_depth == PyramidLayer::Abstract {
            return Ok(hits);
        }

        // L2 任务摘要
        let l2_matches = self.find_in_summary(&hits)?;
        hits.extend(l2_matches);
        if max_depth == PyramidLayer::Summary {
            return Ok(hits);
        }

        // L1 原始记忆
        let l1_matches = self.find_in_raw(&hits)?;
        hits.extend(l1_matches);

        Ok(hits)
    }

    /// L4 触发词匹配
    fn match_triggers(&self, query: &str) -> Result<Vec<RecallHit>> {
        let subconscious = SubconsciousPool::new(self.storage.clone());
        let triggers = subconscious.match_triggers(query)?;

        Ok(triggers
            .into_iter()
            .map(|t| RecallHit {
                layer: PyramidLayer::Subconscious,
                content: format!("触发词: {} → 任务 {}", t.keyword, t.l2_task),
                source: t.keyword,
            })
            .collect())
    }

    /// L3 经验查找
    fn find_in_abstract(&self, l4_hits: &[RecallHit]) -> Result<Vec<RecallHit>> {
        let abstract_layer = AbstractLayer::new(self.storage.clone());
        let all = abstract_layer.load_all()?;
        let mut hits = Vec::new();

        // 基于 L4 命中的触发词查找 L3
        for l4 in l4_hits {
            for te in &all {
                // 检查经验模式是否匹配
                let matched = te.experiences.iter().any(|e| {
                    l4.content.to_lowercase().contains(&e.pattern.to_lowercase())
                });
                if matched {
                    let exp_summary: Vec<String> = te
                        .experiences
                        .iter()
                        .map(|e| format!("{}: {}", e.pattern, e.description))
                        .collect();
                    hits.push(RecallHit {
                        layer: PyramidLayer::Abstract,
                        content: exp_summary.join("\n"),
                        source: format!("{:?}", te.task_type),
                    });
                }
            }
        }

        Ok(hits)
    }

    /// L2 任务摘要查找
    fn find_in_summary(&self, _l3_hits: &[RecallHit]) -> Result<Vec<RecallHit>> {
        // TODO: 基于 L3 命中的 task refs 查找 L2 摘要
        Ok(Vec::new())
    }

    /// L1 原始记忆查找
    fn find_in_raw(&self, _l2_hits: &[RecallHit]) -> Result<Vec<RecallHit>> {
        // TODO: 基于 L2 命中的 l1_refs 定位原始段落
        Ok(Vec::new())
    }

    /// 获取底层 PyramidStorage
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid_types::{
        SubconsciousData, SubconsciousTrigger, TaskType, TypeExperience,
        Experience,
    };
    use chrono::Utc;

    fn make_recall(persona_id: &str) -> (ProgressiveRecall, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), persona_id);
        storage.ensure_dirs().unwrap();
        (ProgressiveRecall::new(storage), tmp)
    }

    #[test]
    fn auto_inject_empty() {
        let (recall, _tmp) = make_recall("test");
        let ctx = recall.auto_inject().unwrap();
        assert!(ctx.subconscious_text.is_empty());
        assert!(ctx.profile.is_empty());
        assert!(ctx.injectable_experiences.is_empty());
    }

    #[test]
    fn auto_inject_with_data() {
        let (recall, _tmp) = make_recall("test");

        // 写入 L4
        let sub = SubconsciousPool::new(recall.storage().clone());
        sub.regenerate(&SubconsciousData {
            triggers: vec![SubconsciousTrigger {
                keyword: "Rust".into(),
                l3_type: TaskType::Coding,
                l2_task: "task-001".into(),
            }],
            narrative: "Rust开发者".into(),
            version: 1,
            updated_at: Utc::now(),
        })
        .unwrap();

        // 写入 Profile
        let profile = ProfileStore::new(recall.storage().clone());
        profile.regenerate("全栈开发者").unwrap();

        // 写入 L3 injectable
        let abstract_layer = AbstractLayer::new(recall.storage().clone());
        abstract_layer
            .regenerate(vec![TypeExperience {
                task_type: TaskType::Coding,
                experiences: vec![Experience {
                    pattern: "TDD".into(),
                    description: "先写测试".into(),
                    source_tasks: vec!["t-1".into()],
                    frequency: 3,
                    injectable: true,
                }],
                l2_refs: vec!["t-1".into()],
                index: vec![],
                updated_at: Utc::now(),
            }])
            .unwrap();

        let ctx = recall.auto_inject().unwrap();
        assert!(ctx.subconscious_text.contains("Rust开发者"));
        assert_eq!(ctx.profile, "全栈开发者");
        assert_eq!(ctx.injectable_experiences.len(), 1);
        assert_eq!(ctx.injectable_experiences[0].pattern, "TDD");
    }

    #[test]
    fn build_inject_text() {
        let (recall, _tmp) = make_recall("test");

        let profile = ProfileStore::new(recall.storage().clone());
        profile.regenerate("开发者").unwrap();

        let text = recall.build_inject_text().unwrap();
        assert!(text.contains("[用户画像]"));
        assert!(text.contains("开发者"));
    }

    #[test]
    fn recall_l4_triggers_match() {
        let (recall, _tmp) = make_recall("test");

        // 写入 L4
        let sub = SubconsciousPool::new(recall.storage().clone());
        sub.regenerate(&SubconsciousData {
            triggers: vec![
                SubconsciousTrigger {
                    keyword: "红冲逻辑".into(),
                    l3_type: TaskType::Coding,
                    l2_task: "task-033".into(),
                },
                SubconsciousTrigger {
                    keyword: "TUI".into(),
                    l3_type: TaskType::Coding,
                    l2_task: "task-001".into(),
                },
            ],
            narrative: "Rust开发者".into(),
            version: 1,
            updated_at: Utc::now(),
        })
        .unwrap();

        // 只到 L4 层
        let hits = recall
            .recall("我要改红冲逻辑", PyramidLayer::Subconscious)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("红冲逻辑"));
        assert_eq!(hits[0].layer, PyramidLayer::Subconscious);
    }

    #[test]
    fn recall_no_match() {
        let (recall, _tmp) = make_recall("test");
        let hits = recall
            .recall("今天天气很好", PyramidLayer::Subconscious)
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn recall_persona_isolation() {
        let tmp = tempfile::tempdir().unwrap();

        let sa = PyramidStorage::new(tmp.path().to_path_buf(), "a");
        sa.ensure_dirs().unwrap();
        let sb = PyramidStorage::new(tmp.path().to_path_buf(), "b");
        sb.ensure_dirs().unwrap();

        let recall_a = ProgressiveRecall::new(sa);
        let recall_b = ProgressiveRecall::new(sb);

        // A 有画像，B 没有
        let profile_a = ProfileStore::new(recall_a.storage().clone());
        profile_a.regenerate("A画像").unwrap();

        let text_a = recall_a.build_inject_text().unwrap();
        let text_b = recall_b.build_inject_text().unwrap();

        assert!(text_a.contains("A画像"));
        assert!(!text_b.contains("A画像"));
    }
}
