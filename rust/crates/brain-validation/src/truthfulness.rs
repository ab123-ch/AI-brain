use brain_core::types::{KnowledgeSource, MemoryLayer, SourceAnalysis, TruthfulnessResult};

/// 来源可信度评分规则
///
/// 不同来源有不同的基础可信度：
/// - UserConfirmation: 0.95（用户确认过）
/// - WebSearch: 0.80（网络搜索结果）
/// - LlmReasoning: 0.60（LLM 推理）
/// - Memory (L0 TaskSummary): 0.90（深度巩固的任务总结）
/// - Memory (L1 EventIndex): 0.70（事件索引）
/// - Memory (L2 ShortTerm): 0.50（短期记忆）
/// - Memory (L3 Raw): 0.30（原始记忆，未加工）
/// - OtherBrain: 0.60（其他副脑产出）
const TRUTHFULNESS_WARNING_THRESHOLD: f64 = 0.60;

/// 来源可靠性评分
fn source_reliability(source: &KnowledgeSource) -> f64 {
    match source {
        KnowledgeSource::UserConfirmation => 0.95,
        KnowledgeSource::WebSearch { .. } => 0.80,
        KnowledgeSource::LlmReasoning { .. } | KnowledgeSource::OtherBrain { .. } => 0.60,
        KnowledgeSource::Memory { layer, .. } => match layer {
            MemoryLayer::TaskSummary => 0.90,
            MemoryLayer::EventIndex => 0.70,
            MemoryLayer::ShortTerm => 0.50,
            MemoryLayer::Raw => 0.30,
        },
    }
}

/// 真实性校验器
///
/// 追踪信息来源，评估可信度：
/// - 单一来源：直接用来源可靠性
/// - 多来源交叉验证：加权平均 + 交叉验证加成
/// - 可信度 < 0.60 → 标注警告
pub struct TruthfulnessChecker {
    /// 交叉验证加成（0.0-0.2）
    cross_verify_bonus: f64,
    /// 警告阈值
    warning_threshold: f64,
}

impl TruthfulnessChecker {
    pub fn new() -> Self {
        Self {
            cross_verify_bonus: 0.15,
            warning_threshold: TRUTHFULNESS_WARNING_THRESHOLD,
        }
    }

    /// 校验声明的真实性
    ///
    /// - `claim`: 待验证的声明内容
    /// - `sources`: 信息来源列表
    ///
    /// 返回:
    /// - confidence: 综合可信度 [0, 1]
    /// - source_analysis: 各来源分析
    /// - cross_verified: 是否经过交叉验证（>=2 个独立来源支持）
    /// - warning: 可信度低于阈值时的警告
    pub fn check(&self, claim: &str, sources: &[KnowledgeSource]) -> TruthfulnessResult {
        if sources.is_empty() {
            return TruthfulnessResult {
                confidence: 0.0,
                source_analysis: Vec::new(),
                cross_verified: false,
                warning: Some(format!("声明 \"{claim}\" 无任何来源支持")),
            };
        }

        // 1. 分析每个来源
        let source_analysis: Vec<SourceAnalysis> = sources
            .iter()
            .map(|s| {
                let reliability = source_reliability(s);
                // 简化：假设所有来源都支持该声明
                // 实际场景中应由 LLM 判断来源是否支持
                SourceAnalysis {
                    source: s.clone(),
                    reliability,
                    supports_claim: true,
                }
            })
            .collect();

        // 2. 计算支持声明的来源数量
        let supporting_count = source_analysis.iter().filter(|a| a.supports_claim).count();

        // 3. 综合可信度
        #[allow(clippy::cast_precision_loss)]
        let base_confidence = if supporting_count > 0 {
            // 加权平均
            let total_weight: f64 = source_analysis
                .iter()
                .filter(|a| a.supports_claim)
                .map(|a| a.reliability)
                .sum();
            total_weight / supporting_count as f64
        } else {
            0.0
        };

        // 4. 交叉验证加成
        let cross_verified = supporting_count >= 2;
        let confidence = if cross_verified {
            (base_confidence + self.cross_verify_bonus).min(1.0)
        } else {
            base_confidence
        };

        // 5. 低可信度警告
        let warning = if confidence < self.warning_threshold {
            Some(format!(
                "可信度 {:.0}% 低于安全阈值 {:.0}%，建议人工确认",
                confidence * 100.0,
                self.warning_threshold * 100.0,
            ))
        } else {
            None
        };

        TruthfulnessResult {
            confidence,
            source_analysis,
            cross_verified,
            warning,
        }
    }

    /// 获取来源的基础可靠性（公开接口，供外部查询）
    pub fn reliability_of(source: &KnowledgeSource) -> f64 {
        source_reliability(source)
    }
}

impl Default for TruthfulnessChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_sources_zero_confidence() {
        let checker = TruthfulnessChecker::new();
        let result = checker.check("测试声明", &[]);
        assert!((result.confidence - 0.0).abs() < f64::EPSILON);
        assert!(!result.cross_verified);
        assert!(result.warning.is_some());
    }

    #[test]
    fn single_memory_source() {
        let checker = TruthfulnessChecker::new();
        let result = checker.check(
            "清明节4月4日",
            &[KnowledgeSource::Memory {
                memory_id: "mem_001".into(),
                layer: MemoryLayer::ShortTerm,
            }],
        );
        // 单一 L2 记忆：0.50
        assert!((result.confidence - 0.50).abs() < 0.01);
        assert!(!result.cross_verified);
        assert!(result.warning.is_some()); // 0.50 < 0.60
    }

    #[test]
    fn web_search_only() {
        let checker = TruthfulnessChecker::new();
        let result = checker.check(
            "2026年4月清明节",
            &[KnowledgeSource::WebSearch {
                url: "https://example.com".into(),
            }],
        );
        // 单一 WebSearch: 0.80
        assert!((result.confidence - 0.80).abs() < 0.01);
        assert!(!result.cross_verified);
        assert!(result.warning.is_none()); // 0.80 >= 0.60
    }

    #[test]
    fn cross_verified_web_and_memory() {
        let checker = TruthfulnessChecker::new();
        let result = checker.check(
            "清明节4月4-6日放假3天",
            &[
                KnowledgeSource::WebSearch {
                    url: "https://example.com".into(),
                },
                KnowledgeSource::Memory {
                    memory_id: "mem_001".into(),
                    layer: MemoryLayer::ShortTerm,
                },
            ],
        );
        // WebSearch(0.80) + Memory L2(0.50) → 平均 0.65 + 交叉验证加成 0.15 = 0.80
        assert!(result.confidence >= 0.75);
        assert!(result.cross_verified);
        assert!(result.warning.is_none());
    }

    #[test]
    fn high_confidence_triple_source() {
        let checker = TruthfulnessChecker::new();
        let result = checker.check(
            "确认信息",
            &[
                KnowledgeSource::UserConfirmation,
                KnowledgeSource::WebSearch {
                    url: "https://example.com".into(),
                },
                KnowledgeSource::Memory {
                    memory_id: "mem_001".into(),
                    layer: MemoryLayer::TaskSummary,
                },
            ],
        );
        // 三源交叉验证，高可信度
        assert!(result.confidence >= 0.85);
        assert!(result.cross_verified);
        assert!(result.warning.is_none());
    }

    #[test]
    fn user_confirmation_highest_reliability() {
        let checker = TruthfulnessChecker::new();
        let result = checker.check("用户确认", &[KnowledgeSource::UserConfirmation]);
        assert!((result.confidence - 0.95).abs() < 0.01);
    }

    #[test]
    fn source_reliability_mapping() {
        assert!(
            (TruthfulnessChecker::reliability_of(&KnowledgeSource::UserConfirmation) - 0.95).abs()
                < f64::EPSILON
        );
        assert!(
            (TruthfulnessChecker::reliability_of(&KnowledgeSource::WebSearch {
                url: String::new()
            }) - 0.80)
                .abs()
                < f64::EPSILON
        );
        assert!(
            (TruthfulnessChecker::reliability_of(&KnowledgeSource::LlmReasoning {
                model: "test".into()
            }) - 0.60)
                .abs()
                < f64::EPSILON
        );
        assert!(
            (TruthfulnessChecker::reliability_of(&KnowledgeSource::Memory {
                memory_id: String::new(),
                layer: MemoryLayer::TaskSummary
            }) - 0.90)
                .abs()
                < f64::EPSILON
        );
        assert!(
            (TruthfulnessChecker::reliability_of(&KnowledgeSource::Memory {
                memory_id: String::new(),
                layer: MemoryLayer::Raw
            }) - 0.30)
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn llm_reasoning_moderate_confidence() {
        let checker = TruthfulnessChecker::new();
        let result = checker.check(
            "LLM 推理结论",
            &[KnowledgeSource::LlmReasoning {
                model: "claude-3".into(),
            }],
        );
        assert!((result.confidence - 0.60).abs() < 0.01);
        assert!(result.warning.is_none()); // 恰好等于阈值
    }
}
