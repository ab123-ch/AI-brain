//! 四步分析编排器
//!
//! 协调：事实总结 → 用户画像 → 踩坑分析 → 自进化规则
//! 每步调用 LLM，解析返回，持久化到对应 Store。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use brain_core::types::PitfallCategory;

use crate::brain_state::BrainStateGenerator;
use crate::error::Result;
use crate::evolution::{EvolutionStore, NewEvolutionRule};
use crate::index_layer::IndexLayer;
use crate::pitfall::{NewPitfall, PitfallStore};
use crate::prompts;
use crate::storage::Storage;
use crate::subconscious::{NarrativeUpdate, SubconsciousStore};
use crate::summary::{SessionSummary, SessionSummaryStore};
use crate::user_profile::UserProfileStore;

/// LLM 调用 trait（由外部注入，brain-memory 不依赖 brain-llm）
pub trait AnalysisLlm: Send + Sync {
    fn complete(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send + '_>>;
}

/// 四步分析结果
#[derive(Debug, Default)]
pub struct AnalysisReport {
    pub fact_summary: String,
    pub profile_entries_added: usize,
    pub pitfalls_found: usize,
    pub rules_created: usize,
    pub subconscious_updated: bool,
    /// L2 会话总结已生成
    pub summary_generated: bool,
    /// Step0 记忆迭代处理条目数
    pub step0_iterations: usize,
    /// Step7 用户评估要求提取条目数
    pub eval_requirements_added: usize,
}

/// 四步分析编排器
pub struct FourStepAnalyzer {
    llm: Box<dyn AnalysisLlm>,
    base_dir: PathBuf,
    /// 当前会话 ID（用于生成 L2 总结）
    session_id: String,
}

impl FourStepAnalyzer {
    pub fn new(llm: Box<dyn AnalysisLlm>, base_dir: PathBuf, session_id: String) -> Self {
        Self {
            llm,
            base_dir,
            session_id,
        }
    }

    /// 执行完整的四步分析
    pub async fn run(&self, conversation_json: &str) -> AnalysisReport {
        let mut report = AnalysisReport::default();

        // 加载上一轮的事实摘要（增量总结的基础）
        let previous_summary = self.load_fact_summary();

        // Step 1: 事实总结（增量：已有摘要 + 新对话 → 更新后摘要）
        let fact_summary = match self
            .step1_fact_summary(conversation_json, previous_summary.as_deref())
            .await
        {
            Ok(s) => {
                tracing::info!("四步分析 Step1 完成: 事实总结 {} 字", s.chars().count());
                self.save_fact_summary(&s);
                s
            }
            Err(e) => {
                tracing::warn!("四步分析 Step1 失败: {e}");
                return report;
            }
        };
        report.fact_summary = fact_summary.clone();

        // Step 0: 记忆迭代（Step1 之后、Step2 之前）
        match self.step0_memory_iteration(&fact_summary).await {
            Ok(count) => {
                if count > 0 {
                    tracing::info!("Step0 记忆迭代完成: 处理 {} 条", count);
                }
                report.step0_iterations = count;
            }
            Err(e) => {
                // Step0 失败不阻塞后续 Step2~Step6
                tracing::warn!("Step0 记忆迭代失败（不阻塞后续）: {e}");
            }
        }

        // Step 2: 用户画像
        match self
            .step2_user_profile(&fact_summary, conversation_json)
            .await
        {
            Ok(added) => {
                tracing::info!("四步分析 Step2 完成: 新增 {added} 条画像");
                report.profile_entries_added = added;
            }
            Err(e) => tracing::warn!("四步分析 Step2 失败: {e}"),
        }

        // Step 3: 踩坑分析
        let new_pitfalls = match self.step3_pitfall(&fact_summary, conversation_json).await {
            Ok(p) => {
                tracing::info!("四步分析 Step3 完成: 发现 {} 个踩坑", p.len());
                report.pitfalls_found = p.len();
                p
            }
            Err(e) => {
                tracing::warn!("四步分析 Step3 失败: {e}");
                Vec::new()
            }
        };

        // Step 4: 自进化规则（基于踩坑结果）
        let evolution_text = if !new_pitfalls.is_empty() {
            match self.step4_evolution(&new_pitfalls).await {
                Ok(rules) => {
                    tracing::info!("四步分析 Step4 完成: 创建 {} 条规则", rules.len());
                    report.rules_created = rules.len();
                    rules
                        .iter()
                        .map(|r| format!("- {}", r.rule))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                Err(e) => {
                    tracing::warn!("四步分析 Step4 失败: {e}");
                    String::new()
                }
            }
        } else {
            String::new()
        };

        // Step 5: 潜意识叙事更新
        match self
            .step5_subconscious(&fact_summary, &new_pitfalls, &evolution_text)
            .await
        {
            Ok(updated) => {
                if updated {
                    tracing::info!("四步分析 Step5 完成: 潜意识叙事已更新");
                }
                report.subconscious_updated = updated;
            }
            Err(e) => tracing::warn!("四步分析 Step5 失败: {e}"),
        }

        // Step 6: 生成 L2 会话总结
        let pitfalls_text = if new_pitfalls.is_empty() {
            "（无）".into()
        } else {
            new_pitfalls
                .iter()
                .map(|p| format!("- [{:?}] {}", p.category, p.description))
                .collect::<Vec<_>>()
                .join("\n")
        };
        match self.step6_summary(&fact_summary, &pitfalls_text).await {
            Ok(true) => {
                tracing::info!("四步分析 Step6 完成: L2 会话总结已生成");
                report.summary_generated = true;
            }
            Ok(false) => {
                tracing::info!("四步分析 Step6 跳过（已有总结）");
            }
            Err(e) => tracing::warn!("四步分析 Step6 失败: {e}"),
        }

        // Step 7: 用户评估要求提取
        match self
            .step7_eval_requirements(&fact_summary, conversation_json)
            .await
        {
            Ok(added) => {
                if added > 0 {
                    tracing::info!("四步分析 Step7 完成: 提取 {} 条用户评估要求", added);
                }
                report.eval_requirements_added = added;
            }
            Err(e) => tracing::warn!("四步分析 Step7 失败: {e}"),
        }

        // 生成 BrainState 快照
        if let Err(e) = self.persist_brain_state(&fact_summary) {
            tracing::warn!("BrainState 快照生成失败: {e}");
        }

        // 记忆衰减 + 淘汰
        match crate::importance::ImportanceManager::decay_all(&self.base_dir) {
            Ok(decay_report) => {
                if decay_report.pruned_count > 0 {
                    tracing::info!("记忆衰减完成: {} 条淘汰", decay_report.pruned_count,);
                }
            }
            Err(e) => tracing::warn!("记忆衰减失败: {e}"),
        }

        report
    }

    // ─── Step 0: 记忆迭代分类 ──────────────────────────────────

    /// Step0: 加载已有记忆，让 LLM 分类与新事实的关系，然后确定性消解
    async fn step0_memory_iteration(
        &self,
        fact_summary: &str,
    ) -> std::result::Result<usize, String> {
        use crate::memory_iteration::{
            resolve_action, ExistingMemory, IterationResult, MemoryStoreType,
        };

        // 1. 收集已有可召回记忆
        let storage = Storage::new_lazy(self.base_dir.clone());
        let mut existing = Vec::new();

        // Summary
        let summary_store = SessionSummaryStore::new(storage.clone());
        if let Ok(summaries) = summary_store.find_recallable() {
            for s in &summaries {
                existing.push(ExistingMemory {
                    id: s.session_id.clone(),
                    store_type: MemoryStoreType::Summary,
                    content: s.fact_summary.clone(),
                    created_at: s.created_at,
                });
            }
        }

        // Pitfall
        let pitfall_store = PitfallStore::new(storage.clone());
        if let Ok(pitfalls) = pitfall_store.load_active() {
            for p in &pitfalls {
                existing.push(ExistingMemory {
                    id: p.id.clone(),
                    store_type: MemoryStoreType::Pitfall,
                    content: p.description.clone(),
                    created_at: p.occurred_at,
                });
            }
        }

        // Evolution
        let evo_store = EvolutionStore::new(storage.clone());
        if let Ok(rules) = evo_store.load_active() {
            for r in &rules {
                existing.push(ExistingMemory {
                    id: r.id.clone(),
                    store_type: MemoryStoreType::Evolution,
                    content: r.rule.clone(),
                    created_at: r.created_at,
                });
            }
        }

        if existing.is_empty() {
            return Ok(0);
        }

        // 2. 调 LLM 分类
        let entries_json = serde_json::to_string(&existing).unwrap_or_default();
        let prompt = prompts::build_step0_prompt(fact_summary, &entries_json);
        let response = self.llm.complete(&prompt).await?;

        // 3. 解析 LLM 返回
        let iterations: Vec<IterationResult> = match serde_json::from_str(&extract_json(&response))
        {
            Ok(v) => v,
            Err(_) => return Ok(0),
        };

        if iterations.is_empty() {
            return Ok(0);
        }

        // 4. 确定性冲突消解
        let mut processed = 0usize;
        for iter in &iterations {
            let action = resolve_action(&iter.relation);
            // 查找 store_type 并标记 superseded
            if let Some(em) = existing.iter().find(|e| e.id == iter.id) {
                match action {
                    crate::memory_iteration::IterationAction::MarkSuperseded
                    | crate::memory_iteration::IterationAction::ReplaceWithNew => {
                        let _ = match em.store_type {
                            MemoryStoreType::Subconscious => {
                                // 叙事模型不再支持 mark_superseded，跳过
                                Ok(())
                            }
                            MemoryStoreType::Summary => summary_store.mark_superseded(&em.id),
                            MemoryStoreType::Pitfall => pitfall_store.mark_superseded(&em.id),
                            MemoryStoreType::Evolution => evo_store.mark_superseded(&em.id),
                        };
                        processed += 1;
                    }
                    crate::memory_iteration::IterationAction::KeepBoth
                    | crate::memory_iteration::IterationAction::Skip => {}
                }
            }
        }

        Ok(processed)
    }

    // ─── Step 1: 事实总结（增量） ──────────────────────────────────────

    /// 加载上一轮的事实摘要
    fn load_fact_summary(&self) -> Option<String> {
        let path = self.base_dir.join("fact_summary.json");
        if !path.exists() {
            return None;
        }
        let data = std::fs::read_to_string(&path).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
        parsed
            .get("summary")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    async fn step1_fact_summary(
        &self,
        conversation_json: &str,
        previous_summary: Option<&str>,
    ) -> std::result::Result<String, String> {
        let prompt = prompts::build_step1_prompt(conversation_json, previous_summary);
        let response = self.llm.complete(&prompt).await?;
        let summary = response.trim().to_string();
        if summary.is_empty() {
            return Err("LLM 返回空的事实总结".into());
        }
        Ok(summary)
    }

    // ─── Step 2: 用户画像 ──────────────────────────────────────

    async fn step2_user_profile(
        &self,
        fact_summary: &str,
        conversation_json: &str,
    ) -> std::result::Result<usize, String> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let profile_store = UserProfileStore::new(storage);

        let existing = profile_store.load().map_err(|e| e.to_string())?;
        let existing_explicit = existing.explicit_preferences.join(", ");
        let existing_implicit = existing.implicit_preferences.join(", ");
        let existing_taboos = existing.taboos.join(", ");
        let existing_habits = existing.habits.join(", ");

        let prompt = prompts::build_step2_prompt(
            fact_summary,
            conversation_json,
            &existing_explicit,
            &existing_implicit,
            &existing_taboos,
            &existing_habits,
        );
        let response = self.llm.complete(&prompt).await?;

        // 解析 JSON
        let parsed: serde_json::Value = serde_json::from_str(&extract_json(&response))
            .map_err(|e| format!("Step2 JSON 解析失败: {e}"))?;

        let explicit = parse_string_array(&parsed, "explicit_preferences");
        let implicit = parse_string_array(&parsed, "implicit_preferences");
        let taboos = parse_string_array(&parsed, "taboos");
        let habits = parse_string_array(&parsed, "habits");

        let total = explicit.len() + implicit.len() + taboos.len() + habits.len();

        let storage = Storage::new_lazy(self.base_dir.clone());
        let profile_store = UserProfileStore::new(storage);
        profile_store
            .merge_analysis(&explicit, &implicit, &taboos, &habits)
            .map_err(|e| e.to_string())?;

        Ok(total)
    }

    // ─── Step 3: 踩坑分析 ──────────────────────────────────────

    async fn step3_pitfall(
        &self,
        fact_summary: &str,
        conversation_json: &str,
    ) -> std::result::Result<Vec<NewPitfall>, String> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let pitfall_store = PitfallStore::new(storage);

        let existing = pitfall_store.load_all().map_err(|e| e.to_string())?;
        let existing_text = existing
            .iter()
            .map(|p| format!("- [{:?}] {}", p.category, p.description))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = prompts::build_step3_prompt(fact_summary, conversation_json, &existing_text);
        let response = self.llm.complete(&prompt).await?;

        let parsed: serde_json::Value = serde_json::from_str(&extract_json(&response))
            .map_err(|e| format!("Step3 JSON 解析失败: {e}"))?;

        let pitfalls_arr = parsed
            .get("pitfalls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut new_pitfalls = Vec::new();
        for p in &pitfalls_arr {
            let category = p
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("Other");
            let description = p
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let user_correction = p
                .get("user_correction")
                .and_then(|v| v.as_str())
                .map(String::from);

            if description.is_empty() {
                continue;
            }

            new_pitfalls.push(NewPitfall {
                category: parse_pitfall_category(category),
                description,
                user_correction,
            });
        }

        // 持久化
        let storage = Storage::new_lazy(self.base_dir.clone());
        let pitfall_store = PitfallStore::new(storage);
        pitfall_store
            .merge_analysis(&new_pitfalls)
            .map_err(|e| e.to_string())?;

        Ok(new_pitfalls)
    }

    // ─── Step 4: 自进化规则 ──────────────────────────────────────

    async fn step4_evolution(
        &self,
        new_pitfalls: &[NewPitfall],
    ) -> std::result::Result<Vec<NewEvolutionRule>, String> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let evolution_store = EvolutionStore::new(storage);

        let existing = evolution_store.load_all().map_err(|e| e.to_string())?;
        let existing_text = existing
            .iter()
            .map(|r| format!("- [P{}] {}", r.priority, r.rule))
            .collect::<Vec<_>>()
            .join("\n");

        let pitfalls_json = serde_json::to_string(new_pitfalls).unwrap_or_default();
        let prompt = prompts::build_step4_prompt(&pitfalls_json, &existing_text);
        let response = self.llm.complete(&prompt).await?;

        let parsed: serde_json::Value = serde_json::from_str(&extract_json(&response))
            .map_err(|e| format!("Step4 JSON 解析失败: {e}"))?;

        let rules_arr = parsed
            .get("rules")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut new_rules = Vec::new();
        for r in &rules_arr {
            let rule = r
                .get("rule")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if rule.is_empty() {
                continue;
            }

            let source_ids = r
                .get("source_pitfall_ids")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            let priority = r.get("priority").and_then(|v| v.as_u64()).unwrap_or(3) as u8;

            new_rules.push(NewEvolutionRule {
                rule,
                source_pitfall_ids: source_ids,
                priority: priority.min(5),
            });
        }

        // 持久化
        let storage = Storage::new_lazy(self.base_dir.clone());
        let evolution_store = EvolutionStore::new(storage);
        evolution_store
            .merge_analysis(&new_rules)
            .map_err(|e| e.to_string())?;

        Ok(new_rules)
    }

    // ─── Step 5: 潜意识叙事更新 ──────────────────────────────────────

    async fn step5_subconscious(
        &self,
        fact_summary: &str,
        pitfalls: &[NewPitfall],
        evolution_text: &str,
    ) -> std::result::Result<bool, String> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let sc_store = SubconsciousStore::new(storage);

        // 加载已有叙事
        let existing_text = match sc_store.load() {
            Ok(Some(n)) => n.narrative,
            Ok(None) => "无，这是首次生成".to_string(),
            Err(e) => {
                tracing::warn!("Step5 加载叙事失败: {e}");
                "无，这是首次生成".to_string()
            }
        };

        let pitfalls_text = if pitfalls.is_empty() {
            "（无新增）".into()
        } else {
            pitfalls
                .iter()
                .map(|p| format!("- [{:?}] {}", p.category, p.description))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let prompt = prompts::build_step5_prompt(
            fact_summary,
            &pitfalls_text,
            evolution_text,
            &existing_text,
        );
        let response = self.llm.complete(&prompt).await?;

        let parsed: serde_json::Value = serde_json::from_str(&extract_json(&response))
            .map_err(|e| format!("Step5 JSON 解析失败: {e}"))?;

        let narrative = parsed
            .get("narrative")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if narrative.is_empty() {
            return Ok(false);
        }

        let new_keywords: Vec<String> = parsed
            .get("new_keywords")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .filter(|s| !s.trim().is_empty())
            .collect();

        // 持久化更新
        let storage = Storage::new_lazy(self.base_dir.clone());
        let sc_store = SubconsciousStore::new(storage);
        sc_store
            .update(&NarrativeUpdate {
                narrative,
                new_keywords,
            })
            .map_err(|e| e.to_string())?;

        Ok(true)
    }

    // ─── Step 7: 用户评估要求提取 ──────────────────────────────────────

    /// 从对话中提取用户对评估脑的要求、纠正和偏好
    async fn step7_eval_requirements(
        &self,
        fact_summary: &str,
        conversation_json: &str,
    ) -> std::result::Result<usize, String> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let req_store = crate::eval_requirement::EvalRequirementStore::new(storage);

        let existing = req_store.load_active().map_err(|e| e.to_string())?;
        let existing_text = if existing.is_empty() {
            "（无）".into()
        } else {
            existing
                .iter()
                .map(|r| format!("- {}", r.content))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let prompt = prompts::build_step7_prompt(fact_summary, conversation_json, &existing_text);
        let response = self.llm.complete(&prompt).await?;

        let parsed: serde_json::Value = serde_json::from_str(&extract_json(&response))
            .map_err(|e| format!("Step7 JSON 解析失败: {e}"))?;

        let requirements_arr = parsed
            .get("requirements")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut added = 0;
        for req in &requirements_arr {
            let content = req
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            if content.is_empty() {
                continue;
            }

            let source = req
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("记忆脑分析")
                .to_string();

            match req_store.add(&content, &source) {
                Ok(_) => added += 1,
                Err(e) => tracing::warn!("Step7 保存评估要求失败: {e}"),
            }
        }

        Ok(added)
    }

    // ─── Step 6: L2 会话总结 ──────────────────────────────────────

    /// 生成 L2 会话总结
    async fn step6_summary(
        &self,
        fact_summary: &str,
        pitfalls_text: &str,
    ) -> std::result::Result<bool, String> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let summary_store = SessionSummaryStore::new(storage);

        if summary_store
            .load(&self.session_id)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Ok(false);
        }

        let sc_store = SubconsciousStore::new(Storage::new_lazy(self.base_dir.clone()));
        let existing_text = match sc_store.load() {
            Ok(Some(n)) if !n.narrative.is_empty() => n.narrative,
            _ => "（无）".into(),
        };

        let prompt = prompts::build_step6_prompt(fact_summary, pitfalls_text, &existing_text);
        let response = self.llm.complete(&prompt).await?;

        let parsed: serde_json::Value = serde_json::from_str(&extract_json(&response))
            .map_err(|e| format!("Step6 JSON 解析失败: {e}"))?;

        let fact_summary_text = parsed
            .get("fact_summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if fact_summary_text.is_empty() {
            return Err("Step6 LLM 返回空的 fact_summary".into());
        }

        let tags: Vec<String> = parsed
            .get("tags")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let pitfalls_arr: Vec<String> = parsed
            .get("pitfalls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let decisions_arr: Vec<String> = parsed
            .get("decisions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let summary = SessionSummary {
            session_id: self.session_id.clone(),
            session_start: chrono::Utc::now(),
            session_end: chrono::Utc::now(),
            fact_summary: fact_summary_text,
            tags,
            pitfalls: pitfalls_arr,
            decisions: decisions_arr,
            archived: false,
            superseded: false,
            created_at: chrono::Utc::now(),
        };
        summary_store.save(&summary).map_err(|e| e.to_string())?;

        Ok(true)
    }

    // ─── 辅助 ────────────────────────────────────────────────

    /// 保存 fact_summary 到文件
    fn save_fact_summary(&self, summary: &str) {
        let path = self.base_dir.join("fact_summary.json");
        let wrapped = serde_json::json!({
            "summary": summary,
            "updated_at": chrono::Utc::now().to_rfc3339()
        });
        if let Err(e) = std::fs::write(
            &path,
            serde_json::to_string_pretty(&wrapped).unwrap_or_default(),
        ) {
            tracing::warn!("fact_summary 写入失败: {e}");
        }
    }

    /// 生成并持久化 BrainState 快照
    fn persist_brain_state(&self, fact_summary: &str) -> Result<()> {
        let storage = Storage::new_lazy(self.base_dir.clone());
        let generator = BrainStateGenerator::new(
            storage,
            UserProfileStore::new(Storage::new_lazy(self.base_dir.clone())),
            PitfallStore::new(Storage::new_lazy(self.base_dir.clone())),
            EvolutionStore::new(Storage::new_lazy(self.base_dir.clone())),
            IndexLayer::new(Storage::new_lazy(self.base_dir.clone())),
        );
        let state = generator.generate_and_persist(fact_summary)?;
        tracing::info!(
            "BrainState 快照已生成: {} 个踩坑, {} 条规则",
            state.active_pitfalls.len(),
            state.evolution_rules.len()
        );
        Ok(())
    }
}

/// 从 LLM 返回中提取 JSON（可能被 markdown 包裹）
fn extract_json(text: &str) -> String {
    let trimmed = text.trim();

    // 尝试提取 ```json ... ``` 块
    if let Some(start) = trimmed.find("```json") {
        let json_start = start + 7;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim().to_string();
        }
    }

    // 尝试提取 ``` ... ``` 块
    if let Some(start) = trimmed.find("```") {
        let json_start = start + 3;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim().to_string();
        }
    }

    // 尝试找第一个 { 或 [
    let start = trimmed.find('{').or_else(|| trimmed.find('['));
    if let Some(s) = start {
        let end = find_matching_brace(trimmed, s);
        return trimmed[s..end].to_string();
    }

    trimmed.to_string()
}

/// 找到匹配的闭合括号
fn find_matching_brace(s: &str, start: usize) -> usize {
    let open = s.chars().nth(start).unwrap_or('{');
    let close = if open == '{' { '}' } else { ']' };
    let mut depth = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, ch) in s.char_indices().skip(start) {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
    }
    s.len()
}

/// 从 JSON Value 中提取字符串数组
fn parse_string_array(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// 解析踩坑类别字符串
fn parse_pitfall_category(s: &str) -> PitfallCategory {
    match s {
        "ToolFailure" => PitfallCategory::ToolFailure,
        "WrongAnswer" => PitfallCategory::WrongAnswer,
        "LazyBehavior" => PitfallCategory::LazyBehavior,
        "FormatIssue" => PitfallCategory::FormatIssue,
        _ => PitfallCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json(input), "{\"key\": \"value\"}");
    }

    #[test]
    fn extract_json_from_raw() {
        let input = "some text {\"a\": 1} more text";
        assert_eq!(extract_json(input), "{\"a\": 1}");
    }

    #[test]
    fn extract_json_from_json_block() {
        let input = "```json\n{\"x\": [1, 2]}\n```";
        assert_eq!(extract_json(input), "{\"x\": [1, 2]}");
    }

    #[test]
    fn parse_string_array_works() {
        let v: serde_json::Value = serde_json::from_str("{\"items\": [\"a\", \"b\"]}").unwrap();
        let arr = parse_string_array(&v, "items");
        assert_eq!(arr, vec!["a", "b"]);
    }

    #[test]
    fn parse_string_array_missing_key() {
        let v: serde_json::Value = serde_json::from_str("{}").unwrap();
        let arr = parse_string_array(&v, "items");
        assert!(arr.is_empty());
    }
}
