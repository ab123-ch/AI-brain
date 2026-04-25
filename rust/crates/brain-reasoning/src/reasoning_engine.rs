use std::sync::Arc;

use brain_core::types::{BroadcastMessage, SlowThinkResult};
use brain_llm::{ChatMessage, ChatRequest, LlmProvider};

use crate::error::Result;
use crate::pattern_matcher::PatternMatcher;

/// 慢思考推理引擎
///
/// 职责单一：根据输入生成推理结果。
/// LLM 可用时走 LLM 深度推理，不可用时降级为规则引擎。
pub struct ReasoningEngine {
    pattern_matcher: PatternMatcher,
    llm: Option<Arc<dyn LlmProvider>>,
}

impl ReasoningEngine {
    pub fn new(pattern_matcher: PatternMatcher) -> Self {
        Self {
            pattern_matcher,
            llm: None,
        }
    }

    /// 设置 LLM Provider（&mut self 版本，用于原地替换）
    pub fn set_llm(&mut self, llm: Arc<dyn LlmProvider>) {
        self.llm = Some(llm);
    }

    /// 注入 LLM Provider（builder 模式）
    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn LlmProvider>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// 是否已接入 LLM
    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// 执行慢思考推理
    ///
    /// 优先走 LLM，降级时走规则引擎。
    pub async fn reason(&self, msg: &BroadcastMessage) -> Result<SlowThinkResult> {
        match &self.llm {
            Some(provider) => self.reason_with_llm(provider, msg).await,
            None => Ok(self.reason_with_rules(msg)),
        }
    }

    /// LLM 深度推理
    async fn reason_with_llm(
        &self,
        provider: &Arc<dyn LlmProvider>,
        msg: &BroadcastMessage,
    ) -> Result<SlowThinkResult> {
        let negative_examples = self.pattern_matcher.get_negative_examples(msg, 3);
        let prompt = self.build_reasoning_prompt(msg, &negative_examples);

        tracing::info!("===== 推理脑 LLM 调用 =====");
        tracing::info!("输入问题: {}", msg.content);
        tracing::info!("发送 prompt:\n{}", prompt);

        let request = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::system(REASONING_SYSTEM_PROMPT),
                ChatMessage::user(&prompt),
            ],
            max_tokens: Some(2048),
            temperature: Some(0.7),
            tools: None,
            tool_choice: None,
        };

        match provider.complete(request).await {
            Ok(response) => {
                let content = response.text();
                tracing::info!("LLM 原始响应:\n{}", content);
                tracing::info!(
                    "Token 用量: prompt={}, completion={}, total={}",
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    response.usage.total_tokens
                );

                let (conclusion, reasoning_path) = self.parse_llm_response(&content);
                tracing::info!("解析结论: {}", conclusion);
                tracing::info!("解析推理路径: {:?}", reasoning_path);
                tracing::info!("===== 推理脑完成 =====");

                Ok(SlowThinkResult {
                    conclusion,
                    reasoning_path,
                    confidence: 0.75,
                    sources: vec![brain_core::types::KnowledgeSource::LlmReasoning {
                        model: provider.model().into(),
                    }],
                    new_experience: Some(brain_core::types::NewExperience {
                        trigger_pattern: self.extract_trigger_pattern(msg),
                        reasoning_path: vec!["LLM推理".into()],
                        tools_used: vec![],
                    }),
                })
            }
            Err(e) => {
                tracing::warn!("LLM 推理失败，降级到规则引擎: {e}");
                Ok(self.reason_with_rules(msg))
            }
        }
    }

    /// 规则引擎降级（原有逻辑）
    fn reason_with_rules(&self, msg: &BroadcastMessage) -> SlowThinkResult {
        let negative_examples = self.pattern_matcher.get_negative_examples(msg, 3);
        let reasoning_path = self.generate_rule_path(msg, &negative_examples);
        let conclusion = self.synthesize_conclusion(msg, &reasoning_path);

        SlowThinkResult {
            conclusion,
            reasoning_path,
            confidence: 0.6,
            sources: vec![brain_core::types::KnowledgeSource::LlmReasoning {
                model: "rule-engine".into(),
            }],
            new_experience: Some(brain_core::types::NewExperience {
                trigger_pattern: self.extract_trigger_pattern(msg),
                reasoning_path: vec!["规则推理".into()],
                tools_used: vec![],
            }),
        }
    }

    /// 保存新经验到经验库（供测试和 on_slow_think_result 使用）
    pub fn save_new_experience(
        &mut self,
        trigger_pattern: &str,
        reasoning_path: Vec<String>,
        tools_used: Vec<String>,
    ) -> String {
        let id = format!("exp-{}", chrono::Utc::now().timestamp_millis());
        let entry = crate::experience::ExperienceEntry {
            id: id.clone(),
            trigger_pattern: trigger_pattern.into(),
            reasoning_path,
            tools_used,
            success_rate: 0.5,
            usage_count: 1,
            is_negative: false,
            files_modified: Vec::new(),
            failure_reason: None,
            created_at: chrono::Utc::now(),
            last_used: chrono::Utc::now(),
        };
        self.pattern_matcher.experience_mut().store(entry);
        id
    }

    pub fn pattern_matcher(&self) -> &PatternMatcher {
        &self.pattern_matcher
    }

    pub fn pattern_matcher_mut(&mut self) -> &mut PatternMatcher {
        &mut self.pattern_matcher
    }

    // ─── 私有方法 ──────────────────────────────────────────────

    fn build_reasoning_prompt(&self, msg: &BroadcastMessage, negatives: &[String]) -> String {
        let mut prompt = format!(
            "请分析以下输入并给出推理结论：\n\n## 输入\n{}\n",
            msg.content
        );

        if !negatives.is_empty() {
            prompt.push_str("\n## 需要避免的错误路径\n");
            for n in negatives {
                prompt.push_str("- ");
                prompt.push_str(n);
                prompt.push('\n');
            }
        }

        prompt
            .push_str("\n## 输出格式\n先给出推理步骤（每步一行），最后以「结论：」开头给出结论。");
        prompt
    }

    fn parse_llm_response(&self, content: &str) -> (String, Vec<String>) {
        let mut steps = Vec::new();
        let mut conclusion = String::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("结论：") || trimmed.starts_with("结论:") {
                conclusion = trimmed
                    .trim_start_matches("结论：")
                    .trim_start_matches("结论:")
                    .trim()
                    .into();
            } else {
                steps.push(trimmed.into());
            }
        }

        if conclusion.is_empty() {
            conclusion = content.lines().last().unwrap_or("").into();
        }
        if steps.is_empty() {
            steps.push("LLM 推理完成".into());
        }

        (conclusion, steps)
    }

    fn generate_rule_path(
        &self,
        msg: &BroadcastMessage,
        negative_examples: &[String],
    ) -> Vec<String> {
        let mut path = Vec::new();
        path.push(format!(
            "分析输入: {}",
            msg.content.chars().take(100).collect::<String>()
        ));
        path.push("检索记忆脑中的相关历史".into());

        if !negative_examples.is_empty() {
            path.push(format!("参考反面案例: {}", negative_examples.join("; ")));
        }

        path.push("基于历史经验和当前需求制定推理方案".into());
        path.push("验证推理路径的合理性".into());
        path
    }

    fn synthesize_conclusion(&self, msg: &BroadcastMessage, reasoning_path: &[String]) -> String {
        format!("基于 {} 步推理分析: {}", reasoning_path.len(), msg.content)
    }

    fn extract_trigger_pattern(&self, msg: &BroadcastMessage) -> String {
        msg.content
            .split(&[' ', ',', '，', '。', '、', '；', '？', '！'][..])
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() >= 2)
            .take(3)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// 推理脑 LLM System Prompt
const REASONING_SYSTEM_PROMPT: &str = "\
你是一个深度推理引擎。你的任务是：
1. 分析输入内容
2. 参考提供的反面案例（避免重复犯错）
3. 逐步推理，给出清晰的推理路径
4. 最后给出明确结论

输出格式：
- 每个推理步骤占一行
- 最后一行以「结论：」开头

保持简洁、准确、有逻辑。";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experience::ExperienceStore;
    use brain_core::types::BrainContext;
    use chrono::Utc;
    use tempfile::TempDir;

    fn make_engine() -> ReasoningEngine {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("exp.json");
        let store = ExperienceStore::new(path, 0.5);
        let matcher = PatternMatcher::new(store);
        std::mem::forget(tmp);
        ReasoningEngine::new(matcher)
    }

    fn make_broadcast(content: &str) -> BroadcastMessage {
        BroadcastMessage {
            content: content.into(),
            raw_input: content.into(),
            context: BrainContext {
                current_date: "2026-04-04".into(),
                cwd: "/test".into(),
                git_branch: Some("main".into()),
                platform: "darwin".into(),
            },
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn reason_with_rules_fallback() {
        let engine = make_engine();
        let msg = make_broadcast("帮我分析代码中的性能问题");
        let result = engine.reason(&msg).await.unwrap();
        assert!(!result.reasoning_path.is_empty());
        assert!(result.confidence > 0.0);
        assert!(result.new_experience.is_some());
        // 规则引擎置信度 0.6
        assert!((result.confidence - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn has_llm_false_by_default() {
        let engine = make_engine();
        assert!(!engine.has_llm());
    }

    #[test]
    fn parse_llm_response_extracts_conclusion() {
        let engine = make_engine();
        let (conclusion, steps) =
            engine.parse_llm_response("第一步：分析问题\n第二步：查找原因\n结论：问题已定位");
        assert_eq!(conclusion, "问题已定位");
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn parse_llm_response_no_conclusion_marker() {
        let engine = make_engine();
        let (conclusion, _steps) = engine.parse_llm_response("只有一行内容");
        assert_eq!(conclusion, "只有一行内容");
    }

    #[test]
    fn save_new_experience() {
        let mut engine = make_engine();
        let id = engine.save_new_experience(
            "代码,性能优化",
            vec!["分析热点".into(), "定位瓶颈".into()],
            vec!["Read".into(), "Grep".into()],
        );
        assert!(!id.is_empty());
        let exp = engine.pattern_matcher().experience().get(&id);
        assert!(exp.is_some());
    }
}
