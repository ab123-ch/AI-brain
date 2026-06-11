//! 四步浓缩引擎
//!
//! 替代旧 analyzer.rs 的 8 步分析，改为 4 步金字塔浓缩：
//! Step 1: L1→L2 任务拆分
//! Step 2: L2→L3 经验抽象
//! Step 3: L3→L4 触发词提取
//! Step 4: Profile + EvalInfo
//!
//! 每步都是全量重生成，覆盖旧数据。

use serde::Deserialize;
use tracing;

use crate::abstract_layer::AbstractLayer;
use crate::error::{MemoryError, Result};
use crate::profile_eval::{EvalInfoStore, ProfileStore};
use crate::prompts;
use crate::pyramid_storage::PyramidStorage;
use crate::pyramid_types::{SubconsciousData, TaskSummary, TypeExperience};
use crate::raw_pool::RawPool;
use crate::subconscious_pool::SubconsciousPool;
use crate::summary_pool::SummaryPool;

/// LLM 分析接口（由调用方实现）
pub trait AnalysisLlm: Send + Sync {
    /// 结构化调用: 分离 system（稳定模板）和 user（变化数据）
    fn analyze_structured(
        &self,
        system: &str,
        user: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send + '_>,
    >;

    /// 向后兼容: 单条 prompt 全放 user
    fn analyze(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send + '_>,
    > {
        self.analyze_structured("", prompt)
    }
}

/// 浓缩报告
#[derive(Debug, Clone)]
pub struct ConcentrationReport {
    pub step1_tasks: usize,
    pub step2_types: usize,
    pub step3_triggers: usize,
    pub step3_narrative_chars: usize,
    pub step4_profile_updated: bool,
    pub errors: Vec<String>,
}

/// 四步浓缩引擎
pub struct ConcentrationEngine {
    storage: PyramidStorage,
    #[allow(dead_code)]
    session_id: String,
}

impl ConcentrationEngine {
    /// 创建浓缩引擎
    pub fn new(storage: PyramidStorage, session_id: String) -> Self {
        Self {
            storage,
            session_id,
        }
    }

    /// 执行四步浓缩
    pub async fn run(&self, llm: &dyn AnalysisLlm) -> ConcentrationReport {
        let mut report = ConcentrationReport {
            step1_tasks: 0,
            step2_types: 0,
            step3_triggers: 0,
            step3_narrative_chars: 0,
            step4_profile_updated: false,
            errors: Vec::new(),
        };

        // 准备对话数据
        let conversation_json = match self.gather_conversations() {
            Ok(json) => json,
            Err(e) => {
                report.errors.push(format!("gather_conversations: {e}"));
                return report;
            }
        };

        if conversation_json == "[]" {
            tracing::info!("四步浓缩: 无对话数据，跳过");
            return report;
        }

        // Step 1: L1→L2 任务拆分（失败则终止）
        match self.step1_l1_to_l2(llm, &conversation_json).await {
            Ok(count) => {
                report.step1_tasks = count;
                tracing::info!("四步浓缩 Step1 完成: {count} 个任务");
            }
            Err(e) => {
                report.errors.push(format!("Step1: {e}"));
                tracing::warn!("四步浓缩 Step1 失败: {e}");
                return report;
            }
        }

        // Step 2: L2→L3 经验抽象（失败继续）
        match self.step2_l2_to_l3(llm).await {
            Ok(count) => {
                report.step2_types = count;
                tracing::info!("四步浓缩 Step2 完成: {count} 个类型");
            }
            Err(e) => {
                report.errors.push(format!("Step2: {e}"));
                tracing::warn!("四步浓缩 Step2 失败: {e}");
            }
        }

        // Step 3: L3→L4 触发词提取（失败继续）
        match self.step3_l3_to_l4(llm).await {
            Ok((triggers, chars)) => {
                report.step3_triggers = triggers;
                report.step3_narrative_chars = chars;
                tracing::info!("四步浓缩 Step3 完成: {triggers} 个触发词, {chars} 字叙事");
            }
            Err(e) => {
                report.errors.push(format!("Step3: {e}"));
                tracing::warn!("四步浓缩 Step3 失败: {e}");
            }
        }

        // Step 4: Profile + EvalInfo（失败继续）
        match self.step4_profile(llm, &conversation_json).await {
            Ok(()) => {
                report.step4_profile_updated = true;
                tracing::info!("四步浓缩 Step4 完成");
            }
            Err(e) => {
                report.errors.push(format!("Step4: {e}"));
                tracing::warn!("四步浓缩 Step4 失败: {e}");
            }
        }

        report
    }

    /// 收集所有 L1 对话数据
    fn gather_conversations(&self) -> Result<String> {
        let pool = RawPool::new(self.storage.clone());
        let sessions = pool.list_sessions()?;
        let mut all_turns = Vec::new();
        for sid in &sessions {
            all_turns.extend(pool.read_session(sid)?);
        }
        Ok(serde_json::to_string_pretty(&all_turns)?)
    }

    /// Step 1: L1→L2 任务拆分
    async fn step1_l1_to_l2(
        &self,
        llm: &dyn AnalysisLlm,
        conversation_json: &str,
    ) -> Result<usize> {
        // 读取现有 L2 索引
        let summary_pool = SummaryPool::new(self.storage.clone());
        let existing_index = summary_pool.load_index()?;
        let existing_json = serde_json::to_string_pretty(&existing_index)?;

        // 调用 LLM
        let prompt = prompts::build_concentration_step1_prompt(conversation_json, &existing_json);
        let response = llm
            .analyze(&prompt)
            .await
            .map_err(MemoryError::ConsolidationFailed)?;

        // 解析返回
        let tasks: Vec<TaskSummary> = parse_json_response(&response)?;

        // 全量覆盖 L2
        summary_pool.regenerate(tasks.clone())?;

        Ok(tasks.len())
    }

    /// Step 2: L2→L3 经验抽象
    async fn step2_l2_to_l3(&self, llm: &dyn AnalysisLlm) -> Result<usize> {
        let summary_pool = SummaryPool::new(self.storage.clone());
        let abstract_layer = AbstractLayer::new(self.storage.clone());

        // 读取 L2 数据
        let l2_data = summary_pool.load_all()?;
        let l2_json = serde_json::to_string_pretty(&l2_data)?;

        // 读取现有 L3
        let existing_l3 = abstract_layer.load_all()?;
        let existing_l3_json = serde_json::to_string_pretty(&existing_l3)?;

        // 调用 LLM
        let prompt = prompts::build_concentration_step2_prompt(&l2_json, &existing_l3_json);
        let response = llm
            .analyze(&prompt)
            .await
            .map_err(MemoryError::ConsolidationFailed)?;

        // 解析返回
        let type_experiences: Vec<TypeExperience> = parse_json_response(&response)?;

        // 全量覆盖 L3
        abstract_layer.regenerate(type_experiences.clone())?;

        Ok(type_experiences.len())
    }

    /// Step 3: L3→L4 触发词提取
    async fn step3_l3_to_l4(&self, llm: &dyn AnalysisLlm) -> Result<(usize, usize)> {
        let abstract_layer = AbstractLayer::new(self.storage.clone());
        let subconscious = SubconsciousPool::new(self.storage.clone());

        // 读取 L3
        let l3_data = abstract_layer.load_all()?;
        let l3_json = serde_json::to_string_pretty(&l3_data)?;

        // 读取现有 L4
        let existing_l4 = subconscious.load()?;
        let existing_l4_json = serde_json::to_string_pretty(&existing_l4)?;

        // 调用 LLM
        let prompt = prompts::build_concentration_step3_prompt(&l3_json, &existing_l4_json);
        let response = llm
            .analyze(&prompt)
            .await
            .map_err(MemoryError::ConsolidationFailed)?;

        // 解析返回
        let data: SubconsciousData = parse_json_response(&response)?;

        let trigger_count = data.triggers.len();
        let narrative_chars = data.narrative.chars().count();

        // 全量覆盖 L4
        subconscious.regenerate(&data)?;

        Ok((trigger_count, narrative_chars))
    }

    /// Step 4: Profile + EvalInfo
    async fn step4_profile(&self, llm: &dyn AnalysisLlm, conversation_json: &str) -> Result<()> {
        let profile_store = ProfileStore::new(self.storage.clone());
        let eval_store = EvalInfoStore::new(self.storage.clone());

        // 读取现有数据
        let existing_profile = profile_store.summary().unwrap_or_default();
        let existing_eval = eval_store.load()?;
        let existing_eval_json = serde_json::to_string_pretty(&existing_eval)?;

        // 调用 LLM
        let prompt = prompts::build_concentration_step4_prompt(
            conversation_json,
            &existing_profile,
            &existing_eval_json,
        );
        let response = llm
            .analyze(&prompt)
            .await
            .map_err(MemoryError::ConsolidationFailed)?;

        // 解析返回
        #[derive(Deserialize)]
        struct Step4Response {
            #[serde(default)]
            profile: String,
            #[serde(default)]
            requirements: Vec<String>,
            #[serde(default)]
            pitfalls: Vec<String>,
            #[serde(default)]
            rules: Vec<String>,
        }
        let parsed: Step4Response = parse_json_response(&response)?;

        // 全量覆盖
        if !parsed.profile.is_empty() {
            profile_store.regenerate(&parsed.profile)?;
        }
        eval_store.regenerate(parsed.requirements, parsed.pitfalls, parsed.rules)?;

        Ok(())
    }
}

/// 从 LLM 返回中提取 JSON（支持 markdown 代码块包裹）
fn parse_json_response<T: serde::de::DeserializeOwned>(response: &str) -> Result<T> {
    let json_str = extract_json_str(response);
    serde_json::from_str(json_str)
        .map_err(|e| MemoryError::ConsolidationFailed(format!("JSON解析失败: {e}")))
}

fn extract_json_str(response: &str) -> &str {
    let trimmed = response.trim();

    // 尝试提取 ```json ... ``` 包裹的内容
    if let Some(start) = trimmed.find("```json") {
        let json_start = start + 7;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim();
        }
    }

    // 尝试提取 ``` ... ``` 包裹的内容
    if let Some(start) = trimmed.find("```") {
        let json_start = start + 3;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim();
        }
    }

    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_raw() {
        let json = r#"{"triggers":[],"narrative":"test"}"#;
        assert_eq!(extract_json_str(json), json);
    }

    #[test]
    fn extract_json_from_markdown() {
        let md = "Here is the result:\n```json\n{\"triggers\":[]}\n```\nDone";
        assert_eq!(extract_json_str(md), "{\"triggers\":[]}");
    }

    #[test]
    fn extract_json_from_code_block() {
        let md = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json_str(md), "{\"key\": \"value\"}");
    }

    #[test]
    fn parse_json_response_works() {
        let response = r#"{"task_id":"t-1","task_type":"Coding","task_name":"Test","summary":"A task","l1_refs":[],"tags":[],"importance":0.5,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let result: TaskSummary = parse_json_response(response).unwrap();
        assert_eq!(result.task_id, "t-1");
    }

    #[test]
    fn parse_json_response_with_markdown() {
        let response =
            "```json\n{\"profile\":\"test\",\"requirements\":[],\"pitfalls\":[],\"rules\":[]}\n```";
        let result: serde_json::Value = parse_json_response(response).unwrap();
        assert_eq!(result["profile"], "test");
    }

    #[test]
    fn concentration_report_default() {
        let report = ConcentrationReport {
            step1_tasks: 0,
            step2_types: 0,
            step3_triggers: 0,
            step3_narrative_chars: 0,
            step4_profile_updated: false,
            errors: Vec::new(),
        };
        assert!(report.errors.is_empty());
    }

    /// Mock LLM 用于测试
    struct MockLlm {
        response: String,
    }

    impl MockLlm {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
            }
        }
    }

    impl AnalysisLlm for MockLlm {
        fn analyze_structured(
            &self,
            _system: &str,
            _user: &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send + '_>,
        > {
            let response = self.response.clone();
            Box::pin(async move { Ok(response) })
        }
    }

    #[tokio::test]
    async fn step1_l1_to_l2_with_mock() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        storage.ensure_dirs().unwrap();

        // 写一些 L1 数据
        let raw = RawPool::new(storage.clone());
        raw.append_turn("sess-001", "User", "帮我修复TUI鼠标问题", None)
            .unwrap();

        let engine = ConcentrationEngine::new(storage.clone(), "sess-001".into());

        let mock_response = r#"[
            {
                "task_id": "task-001",
                "task_type": "Coding",
                "task_name": "TUI鼠标修复",
                "summary": "修复鼠标捕获问题",
                "l1_refs": [{"session": "sess-001", "paragraphs": [0]}],
                "tags": ["TUI", "鼠标"],
                "importance": 0.8,
                "created_at": "2026-05-25T00:00:00Z",
                "updated_at": "2026-05-25T00:00:00Z"
            }
        ]"#;

        let llm = MockLlm::new(mock_response);
        let report = engine.run(&llm).await;

        assert_eq!(report.step1_tasks, 1);
        // Mock 返回的是 Step1 格式，Step2/3/4 会解析失败，这是预期的
        assert!(report.errors.iter().any(|e| e.starts_with("Step2:")));
    }

    #[tokio::test]
    async fn step1_failure_stops_pipeline() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        storage.ensure_dirs().unwrap();

        let raw = RawPool::new(storage.clone());
        raw.append_turn("sess-001", "User", "hello", None).unwrap();

        let engine = ConcentrationEngine::new(storage.clone(), "sess-001".into());
        let llm = MockLlm::new("not valid json");

        let report = engine.run(&llm).await;
        assert!(!report.errors.is_empty());
        // Step1 失败应该终止后续步骤
        assert_eq!(report.step2_types, 0);
        assert_eq!(report.step3_triggers, 0);
    }

    #[tokio::test]
    async fn empty_conversations_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        storage.ensure_dirs().unwrap();

        let engine = ConcentrationEngine::new(storage, "sess-none".into());
        let llm = MockLlm::new("{}");
        let report = engine.run(&llm).await;

        assert_eq!(report.step1_tasks, 0);
        assert!(report.errors.is_empty());
    }
}
