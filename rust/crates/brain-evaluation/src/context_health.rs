use brain_core::types::{
    BrainHealthReport, ContextSnapshot, EvaluationResult, SlimInstruction,
};

/// 上下文健康度阈值配置
#[derive(Debug, Clone)]
pub struct HealthThresholds {
    /// 上下文使用率警告阈值（默认 0.80 = 80%）
    pub usage_warning: f64,
    /// 上下文使用率危险阈值（默认 0.95 = 95%）
    pub usage_critical: f64,
    /// 冗余度阈值（默认 0.70）
    pub redundancy_threshold: f64,
    /// 过时消息阈值（默认 0.60）
    pub stale_threshold: f64,
    /// 整体健康最低可接受分数（默认 0.40）
    pub min_acceptable_health: f64,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            usage_warning: 0.80,
            usage_critical: 0.95,
            redundancy_threshold: 0.70,
            stale_threshold: 0.60,
            min_acceptable_health: 0.40,
        }
    }
}

/// 上下文健康度评估器
///
/// 纯规则引擎，不依赖 LLM。按固定规则评估各副脑上下文健康度，
/// 生成瘦身指令（删除/压缩/保留）。
///
/// 评估维度：
/// 1. **usage** — 上下文使用率（消息数占比）
/// 2. **redundancy** — 信息冗余度（重复内容比例）
/// 3. **stale** — 信息过时度（陈旧未引用的比例）
pub struct ContextHealthChecker {
    thresholds: HealthThresholds,
    /// 最大允许消息数（虚拟上限）
    max_messages: usize,
}

impl ContextHealthChecker {
    pub fn new(thresholds: HealthThresholds, max_messages: usize) -> Self {
        Self {
            thresholds,
            max_messages,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(HealthThresholds::default(), 1000)
    }

    /// 评估一组副脑的上下文快照，生成评估结果
    pub fn evaluate(&self, snapshots: &[ContextSnapshot]) -> EvaluationResult {
        if snapshots.is_empty() {
            return EvaluationResult {
                overall_health: 1.0,
                brain_reports: Vec::new(),
                slim_instructions: Vec::new(),
            };
        }

        let mut brain_reports = Vec::with_capacity(snapshots.len());
        let mut slim_instructions = Vec::new();

        for snapshot in snapshots {
            let report = self.evaluate_single(snapshot);
            let instructions = self.generate_slim_instructions(snapshot, &report);
            slim_instructions.extend(instructions);
            brain_reports.push(report);
        }

        #[allow(clippy::cast_precision_loss)]
        let overall_health = if brain_reports.is_empty() {
            1.0
        } else {
            brain_reports
                .iter()
                .map(|r| r.health_score)
                .sum::<f64>()
                / brain_reports.len() as f64
        };

        EvaluationResult {
            overall_health,
            brain_reports,
            slim_instructions,
        }
    }

    /// 评估单个副脑上下文
    fn evaluate_single(&self, snapshot: &ContextSnapshot) -> BrainHealthReport {
        let usage_percent = self.calc_usage(snapshot);
        let redundancy_score = self.calc_redundancy(snapshot);
        let stale_score = self.calc_stale(snapshot);

        // 权重：usage 最重要（0.5），redundancy 次之（0.3），stale（0.2）
        let penalty = usage_percent * 0.5
            + redundancy_score * self.thresholds.redundancy_threshold * 0.3
            + stale_score * self.thresholds.stale_threshold * 0.2;

        let health_score = (1.0 - penalty).clamp(0.0, 1.0);

        BrainHealthReport {
            brain_id: snapshot.brain_id.clone(),
            health_score,
            usage_percent,
            redundancy_score,
            stale_score,
        }
    }

    /// 计算上下文使用率
    #[allow(clippy::cast_precision_loss)]
    fn calc_usage(&self, snapshot: &ContextSnapshot) -> f64 {
        if self.max_messages == 0 {
            return 0.0;
        }
        (snapshot.message_count as f64 / self.max_messages as f64).clamp(0.0, 1.0)
    }

    /// 计算信息冗余度
    fn calc_redundancy(&self, snapshot: &ContextSnapshot) -> f64 {
        (1.0 - snapshot.health_score).clamp(0.0, 1.0)
    }

    /// 计算信息过时度
    fn calc_stale(&self, snapshot: &ContextSnapshot) -> f64 {
        let usage = self.calc_usage(snapshot);
        let health = snapshot.health_score;
        (usage * (1.0 - health)).clamp(0.0, 1.0)
    }

    /// 根据评估报告生成瘦身指令
    fn generate_slim_instructions(
        &self,
        snapshot: &ContextSnapshot,
        report: &BrainHealthReport,
    ) -> Vec<SlimInstruction> {
        let mut instructions = Vec::new();
        let brain_id_str = snapshot.brain_id.to_string();

        // 使用率 > 危险阈值 → 压缩旧消息
        if report.usage_percent > self.thresholds.usage_critical {
            instructions.push(SlimInstruction::Compress {
                message_ids: vec![format!("{}_old_batch", brain_id_str)],
                summary: format!(
                    "上下文使用率 {:.0}% 超过危险阈值 {:.0}%，压缩旧消息",
                    report.usage_percent * 100.0,
                    self.thresholds.usage_critical * 100.0,
                ),
            });
        }
        // 使用率 > 警告阈值但 < 危险阈值 → 压缩部分
        else if report.usage_percent > self.thresholds.usage_warning {
            instructions.push(SlimInstruction::Compress {
                message_ids: vec![format!("{}_stale_batch", brain_id_str)],
                summary: format!(
                    "上下文使用率 {:.0}% 接近上限，压缩过时消息",
                    report.usage_percent * 100.0,
                ),
            });
        }

        // 高冗余 → 删除重复
        if report.redundancy_score > self.thresholds.redundancy_threshold {
            instructions.push(SlimInstruction::Delete {
                message_ids: vec![format!("{}_redundant", brain_id_str)],
                reason: format!(
                    "冗余度 {:.0}% 超过阈值，删除重复信息",
                    report.redundancy_score * 100.0,
                ),
            });
        }

        // 高过时 → 删除过时
        if report.stale_score > self.thresholds.stale_threshold {
            instructions.push(SlimInstruction::Delete {
                message_ids: vec![format!("{}_stale", brain_id_str)],
                reason: format!(
                    "过时度 {:.0}% 超过阈值，清理过时信息",
                    report.stale_score * 100.0,
                ),
            });
        }

        // 健康分很高 → 保持
        if report.health_score > self.thresholds.min_acceptable_health && instructions.is_empty() {
            instructions.push(SlimInstruction::Preserve {
                message_ids: vec![format!("{}_all", brain_id_str)],
                reason: format!(
                    "健康分 {:.0}% 良好，保持上下文不变",
                    report.health_score * 100.0,
                ),
            });
        }

        instructions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::BrainId;

    fn make_snapshot(brain_id: &str, message_count: usize, health_score: f64) -> ContextSnapshot {
        ContextSnapshot {
            brain_id: BrainId(brain_id.into()),
            message_count,
            health_score,
        }
    }

    #[test]
    fn evaluate_empty_snapshots() {
        let checker = ContextHealthChecker::with_defaults();
        let result = checker.evaluate(&[]);
        assert!((result.overall_health - 1.0).abs() < f64::EPSILON);
        assert!(result.brain_reports.is_empty());
        assert!(result.slim_instructions.is_empty());
    }

    #[test]
    fn evaluate_healthy_brain() {
        let checker = ContextHealthChecker::with_defaults();
        let snapshot = make_snapshot("reasoning", 50, 0.9);
        let result = checker.evaluate(&[snapshot]);
        assert!(result.overall_health > 0.5);
        assert!(result
            .slim_instructions
            .iter()
            .any(|i| matches!(i, SlimInstruction::Preserve { .. })));
    }

    #[test]
    fn evaluate_overloaded_brain() {
        let checker = ContextHealthChecker::new(HealthThresholds::default(), 100);
        let snapshot = make_snapshot("memory", 95, 0.3);
        let result = checker.evaluate(&[snapshot]);

        let report = &result.brain_reports[0];
        assert!(report.usage_percent > 0.9);
        assert!(report.health_score < 0.5);

        let has_compress = result
            .slim_instructions
            .iter()
            .any(|i| matches!(i, SlimInstruction::Compress { .. }));
        let has_delete = result
            .slim_instructions
            .iter()
            .any(|i| matches!(i, SlimInstruction::Delete { .. }));
        assert!(has_compress || has_delete);
    }

    #[test]
    fn evaluate_multiple_brains() {
        let checker = ContextHealthChecker::new(HealthThresholds::default(), 100);
        let snapshots = vec![
            make_snapshot("reasoning", 30, 0.9),
            make_snapshot("memory", 80, 0.5),
        ];
        let result = checker.evaluate(&snapshots);
        assert_eq!(result.brain_reports.len(), 2);
        assert!(result.brain_reports[0].health_score > result.brain_reports[1].health_score);
    }

    #[test]
    fn usage_calculation() {
        let checker = ContextHealthChecker::new(HealthThresholds::default(), 200);
        let snapshot = make_snapshot("test", 100, 0.8);
        let usage = checker.calc_usage(&snapshot);
        assert!((usage - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn stale_calculation_high_usage_low_health() {
        let checker = ContextHealthChecker::new(HealthThresholds::default(), 100);
        let snapshot = make_snapshot("test", 90, 0.1);
        let stale = checker.calc_stale(&snapshot);
        assert!(stale > 0.7);
    }

    #[test]
    fn custom_thresholds() {
        let thresholds = HealthThresholds {
            usage_warning: 0.5,
            usage_critical: 0.7,
            redundancy_threshold: 0.5,
            stale_threshold: 0.5,
            min_acceptable_health: 0.6,
        };
        let checker = ContextHealthChecker::new(thresholds, 100);
        let snapshot = make_snapshot("test", 60, 0.8);
        let result = checker.evaluate(&[snapshot]);
        let report = &result.brain_reports[0];
        assert!(report.usage_percent > 0.5);
    }
}
