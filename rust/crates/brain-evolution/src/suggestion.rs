use brain_core::types::BrainId;
use serde::{Deserialize, Serialize};

use crate::template::{BrainTemplate, FastThinkRule};

/// 任务模式 — 用于检测是否需要新副脑
#[derive(Debug, Clone, Serialize, Deserialize)]
#[must_use]
pub struct TaskPattern {
    /// 该模式的关键词
    pub keywords: Vec<String>,
    /// 出现频率
    pub frequency: u32,
    /// 参与副脑的平均置信度
    pub avg_confidence: f64,
    /// 参与过的副脑
    pub participating_brains: Vec<BrainId>,
}

/// 创建建议
#[derive(Debug, Clone, Serialize, Deserialize)]
#[must_use]
pub struct CreationSuggestion {
    /// LLM 给出的创建理由
    pub reason: String,
    /// 建议的副脑名称
    pub suggested_name: String,
    /// 建议的能力标签
    pub suggested_capabilities: Vec<String>,
    /// 建议从哪个副脑分裂
    pub parent_brain: Option<BrainId>,
    /// 建议的 system prompt
    pub suggested_prompt: String,
    /// 建议的快思考规则关键词
    pub suggested_keywords: Vec<String>,
    /// 建议置信度
    pub confidence: f64,
}

/// 建议引擎 — 基于任务模式分析是否需要新副脑
///
/// 触发条件：
/// 1. 某类任务频率 >= min_tasks
/// 2. 参与副脑的平均置信度 < confidence_threshold
/// 3. 该模式没有匹配到现有内置副脑
#[must_use]
pub struct SuggestionEngine {
    /// 已观察到的任务模式
    patterns: Vec<TaskPattern>,
    /// 触发建议的最低任务数
    min_tasks_before_suggest: u32,
    /// 触发建议的最高平均置信度
    confidence_threshold: f64,
    /// 已生成的建议（避免重复）
    suggestions: Vec<CreationSuggestion>,
}

impl SuggestionEngine {
    pub fn new() -> Self {
        Self {
            patterns: Vec::new(),
            min_tasks_before_suggest: 5,
            confidence_threshold: 0.5,
            suggestions: Vec::new(),
        }
    }

    /// 使用自定义阈值
    pub fn with_thresholds(mut self, min_tasks: u32, confidence_threshold: f64) -> Self {
        self.min_tasks_before_suggest = min_tasks;
        self.confidence_threshold = confidence_threshold;
        self
    }

    /// 记录一次任务模式
    ///
    /// 如果该模式已存在，增加频率并更新平均置信度。
    /// 如果是新模式，创建新条目。
    pub fn record_pattern(
        &mut self,
        keywords: Vec<String>,
        confidence: f64,
        participating_brains: Vec<BrainId>,
    ) {
        // 查找已有模式（简单关键词集合匹配）
        let existing = self.patterns.iter_mut().find(|p| {
            let set_a: std::collections::HashSet<_> = p.keywords.iter().collect();
            let set_b: std::collections::HashSet<_> = keywords.iter().collect();
            set_a == set_b
        });

        if let Some(pattern) = existing {
            pattern.frequency += 1;
            // 指数移动平均更新置信度
            pattern.avg_confidence = pattern.avg_confidence * 0.7 + confidence * 0.3;
        } else {
            self.patterns.push(TaskPattern {
                keywords,
                frequency: 1,
                avg_confidence: confidence,
                participating_brains,
            });
        }
    }

    /// 检查是否有应该建议创建新副脑的模式
    ///
    /// 条件：频率 >= min_tasks 且 平均置信度 < threshold
    pub fn check_suggestions(&mut self) -> Vec<&TaskPattern> {
        self.patterns
            .iter()
            .filter(|p| {
                p.frequency >= self.min_tasks_before_suggest
                    && p.avg_confidence < self.confidence_threshold
            })
            .collect()
    }

    /// 从模式生成建议（规则化生成，不依赖 LLM 调用）
    ///
    /// LLM 调用由上层（Orchestrator）处理，这里只做规则匹配。
    pub fn generate_rule_based_suggestion(&self, pattern: &TaskPattern) -> CreationSuggestion {
        let name = format!(
            "auto-{}",
            pattern
                .keywords
                .first()
                .map_or_else(|| "unknown".into(), |k| k.replace(' ', "-"))
        );

        CreationSuggestion {
            reason: format!(
                "检测到任务模式 [{:?}] 出现 {} 次，平均置信度 {:.2}（低于阈值 {:.2}），建议创建专用副脑",
                pattern.keywords,
                pattern.frequency,
                pattern.avg_confidence,
                self.confidence_threshold
            ),
            suggested_name: name,
            suggested_capabilities: pattern.keywords.clone(),
            parent_brain: pattern.participating_brains.first().cloned(),
            suggested_prompt: format!(
                "你是一个专注于 {} 的专家助手。",
                pattern.keywords.join("、")
            ),
            suggested_keywords: pattern.keywords.clone(),
            confidence: 1.0 - pattern.avg_confidence, // 置信度越低，建议越强
        }
    }

    /// 记录已生成的建议（避免重复）
    pub fn add_suggestion(&mut self, suggestion: CreationSuggestion) {
        self.suggestions.push(suggestion);
    }

    /// 获取已生成的建议
    pub fn suggestions(&self) -> &[CreationSuggestion] {
        &self.suggestions
    }

    /// 检查某个名称是否已经被建议过
    pub fn has_suggested(&self, name: &str) -> bool {
        self.suggestions.iter().any(|s| s.suggested_name == name)
    }

    /// 从建议创建模板
    pub fn suggestion_to_template(suggestion: &CreationSuggestion) -> BrainTemplate {
        BrainTemplate::new(&suggestion.suggested_name, &suggestion.reason)
            .with_prompt(&suggestion.suggested_prompt)
            .with_rule(FastThinkRule {
                keywords: suggestion.suggested_keywords.clone(),
                confidence: 0.8,
                summary_template: format!("检测到 {} 相关任务", suggestion.suggested_name),
            })
            .with_capabilities(
                suggestion
                    .suggested_capabilities
                    .iter()
                    .map(String::as_str)
                    .collect(),
            )
    }

    /// 获取所有观察到的模式
    pub fn patterns(&self) -> &[TaskPattern] {
        &self.patterns
    }

    /// 获取配置
    pub fn min_tasks(&self) -> u32 {
        self.min_tasks_before_suggest
    }

    pub fn confidence_threshold(&self) -> f64 {
        self.confidence_threshold
    }
}

impl Default for SuggestionEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_pattern_new() {
        let mut engine = SuggestionEngine::new();
        engine.record_pattern(vec!["代码审查".into()], 0.3, vec![BrainId::reasoning()]);
        assert_eq!(engine.patterns().len(), 1);
        assert_eq!(engine.patterns()[0].frequency, 1);
    }

    #[test]
    fn test_record_pattern_existing() {
        let mut engine = SuggestionEngine::new();
        engine.record_pattern(vec!["代码审查".into()], 0.3, vec![BrainId::reasoning()]);
        engine.record_pattern(vec!["代码审查".into()], 0.4, vec![BrainId::reasoning()]);
        assert_eq!(engine.patterns().len(), 1);
        assert_eq!(engine.patterns()[0].frequency, 2);
    }

    #[test]
    fn test_check_suggestions_below_threshold() {
        let mut engine = SuggestionEngine::new().with_thresholds(3, 0.5);

        // 记录 3 次，置信度很低
        for _ in 0..3 {
            engine.record_pattern(vec!["安全审计".into()], 0.2, vec![BrainId::reasoning()]);
        }

        let suggestions = engine.check_suggestions();
        assert_eq!(suggestions.len(), 1);
    }

    #[test]
    fn test_check_suggestions_above_confidence() {
        let mut engine = SuggestionEngine::new().with_thresholds(3, 0.5);

        // 记录 3 次，但置信度高
        for _ in 0..3 {
            engine.record_pattern(vec!["正常任务".into()], 0.9, vec![BrainId::reasoning()]);
        }

        let suggestions = engine.check_suggestions();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_generate_suggestion() {
        let engine = SuggestionEngine::new();
        let pattern = TaskPattern {
            keywords: vec!["性能优化".into()],
            frequency: 5,
            avg_confidence: 0.3,
            participating_brains: vec![BrainId::reasoning()],
        };

        let suggestion = engine.generate_rule_based_suggestion(&pattern);
        assert_eq!(suggestion.suggested_name, "auto-性能优化");
        assert!(!suggestion.reason.is_empty());
        assert!(!suggestion.suggested_keywords.is_empty());
    }

    #[test]
    fn test_suggestion_to_template() {
        let suggestion = CreationSuggestion {
            reason: "test".into(),
            suggested_name: "test-brain".into(),
            suggested_capabilities: vec!["test".into()],
            parent_brain: None,
            suggested_prompt: "test prompt".into(),
            suggested_keywords: vec!["test".into()],
            confidence: 0.8,
        };

        let template = SuggestionEngine::suggestion_to_template(&suggestion);
        assert_eq!(template.name, "test-brain");
        assert_eq!(template.prompt_template, "test prompt");
        assert_eq!(template.fast_think_rules.len(), 1);
    }

    #[test]
    fn test_has_suggested() {
        let mut engine = SuggestionEngine::new();
        engine.add_suggestion(CreationSuggestion {
            reason: "test".into(),
            suggested_name: "existing".into(),
            suggested_capabilities: vec![],
            parent_brain: None,
            suggested_prompt: String::new(),
            suggested_keywords: vec![],
            confidence: 0.5,
        });

        assert!(engine.has_suggested("existing"));
        assert!(!engine.has_suggested("nonexistent"));
    }
}
