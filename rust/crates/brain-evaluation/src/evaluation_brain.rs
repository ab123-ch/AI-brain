use brain_core::agent::StatelessBrain;
use brain_core::types::{BrainId, BrainKind, ContextSnapshot, EvaluationResult};

use crate::context_health::{ContextHealthChecker, HealthThresholds};
use crate::error::Result;

/// 评估脑配置
#[derive(Debug, Clone)]
pub struct EvaluationConfig {
    /// 健康度阈值
    pub thresholds: HealthThresholds,
    /// 最大消息数（虚拟上限）
    pub max_messages: usize,
    /// 触发条件：空闲秒数
    pub idle_trigger_secs: u64,
    /// 触发条件：上下文使用率百分比
    pub usage_trigger_percent: f64,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            thresholds: HealthThresholds::default(),
            max_messages: 1000,
            idle_trigger_secs: 300, // 5 分钟
            usage_trigger_percent: 0.80,
        }
    }
}

/// 评估脑（脑干反射）
///
/// 无状态副脑：每次启动都是空白，只加载固定规则。
/// 执行评估后自毁（由调用方负责 drop）。
///
/// 职责：
/// - 接收其他副脑的上下文快照
/// - 按固定规则评估健康度（usage/redundancy/stale）
/// - 输出瘦身指令（Delete/Compress/Preserve）
///
/// 触发条件：
/// - 空闲超过 5 分钟
/// - 上下文使用率 > 80%
/// - 任务完成后
pub struct EvaluationBrain {
    id: BrainId,
    config: EvaluationConfig,
    checker: ContextHealthChecker,
}

impl EvaluationBrain {
    pub fn new(config: EvaluationConfig) -> Result<Self> {
        let checker = ContextHealthChecker::new(config.thresholds.clone(), config.max_messages);
        Ok(Self {
            id: BrainId::evaluation(),
            config,
            checker,
        })
    }

    pub fn with_defaults() -> Result<Self> {
        Self::new(EvaluationConfig::default())
    }

    /// 判断是否应触发评估
    ///
    /// 满足以下任一条件：
    /// 1. 空闲时间超过阈值
    /// 2. 任一副脑上下文使用率超过阈值
    /// 3. 任务完成（由调用方传入 task_completed=true）
    pub fn should_evaluate(
        &self,
        snapshots: &[ContextSnapshot],
        idle_secs: u64,
        task_completed: bool,
    ) -> bool {
        // 任务完成 → 立即触发
        if task_completed {
            return true;
        }

        // 空闲超时
        if idle_secs >= self.config.idle_trigger_secs {
            return true;
        }

        // 任一副脑上下文使用率超限
        let max_messages = self.config.max_messages;
        #[allow(clippy::cast_precision_loss)]
        let triggered = snapshots.iter().any(|s| {
            let usage = s.message_count as f64 / max_messages as f64;
            usage > self.config.usage_trigger_percent
        });
        triggered
    }

    /// 获取配置引用
    pub fn config(&self) -> &EvaluationConfig {
        &self.config
    }
}

impl StatelessBrain for EvaluationBrain {
    fn id(&self) -> &BrainId {
        &self.id
    }

    fn kind(&self) -> BrainKind {
        BrainKind::Evaluation
    }

    /// 执行评估 — 接收快照，输出评估结果
    ///
    /// 调用后建议立即 drop 此实例（无状态自毁语义）
    fn evaluate(&self, snapshots: Vec<ContextSnapshot>) -> EvaluationResult {
        tracing::info!("评估脑启动: 接收 {} 个副脑快照", snapshots.len());

        let result = self.checker.evaluate(&snapshots);

        tracing::info!(
            "评估完成: 整体健康 {:.1}%, 生成 {} 条瘦身指令",
            result.overall_health * 100.0,
            result.slim_instructions.len()
        );

        result
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

    fn make_brain() -> EvaluationBrain {
        EvaluationBrain::with_defaults().unwrap()
    }

    fn make_brain_with_config(
        max_messages: usize,
        idle_secs: u64,
        usage_pct: f64,
    ) -> EvaluationBrain {
        let config = EvaluationConfig {
            max_messages,
            idle_trigger_secs: idle_secs,
            usage_trigger_percent: usage_pct,
            ..EvaluationConfig::default()
        };
        EvaluationBrain::new(config).unwrap()
    }

    #[test]
    fn brain_id_and_kind() {
        let brain = make_brain();
        assert_eq!(brain.id(), &BrainId::evaluation());
        assert_eq!(brain.kind(), BrainKind::Evaluation);
    }

    #[test]
    fn evaluate_empty() {
        let brain = make_brain();
        let result = brain.evaluate(vec![]);
        assert!((result.overall_health - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn evaluate_healthy_system() {
        let brain = make_brain();
        let snapshots = vec![
            make_snapshot("reasoning", 50, 0.9),
            make_snapshot("memory", 30, 0.95),
        ];
        let result = brain.evaluate(snapshots);
        assert!(result.overall_health > 0.5);
        assert_eq!(result.brain_reports.len(), 2);
    }

    #[test]
    fn evaluate_overloaded_system() {
        let brain = make_brain_with_config(100, 300, 0.80);
        let snapshots = vec![
            make_snapshot("reasoning", 95, 0.2),
            make_snapshot("memory", 90, 0.1),
        ];
        let result = brain.evaluate(snapshots);
        // 整体健康应较低
        assert!(result.overall_health < 0.5);
        // 瘦身指令已禁用
        assert!(result.slim_instructions.is_empty());
    }

    #[test]
    fn should_evaluate_task_completed() {
        let brain = make_brain();
        let result = brain.should_evaluate(&[], 0, true);
        assert!(result);
    }

    #[test]
    fn should_evaluate_idle_timeout() {
        let brain = make_brain_with_config(100, 300, 0.80);
        // 空闲 301 秒，超过阈值 300
        let result = brain.should_evaluate(&[], 301, false);
        assert!(result);
    }

    #[test]
    fn should_not_evaluate_no_trigger() {
        let brain = make_brain_with_config(100, 300, 0.80);
        // 空闲 100 秒，未超阈值
        let result = brain.should_evaluate(&[], 100, false);
        assert!(!result);
    }

    #[test]
    fn should_evaluate_usage_exceeded() {
        let brain = make_brain_with_config(100, 300, 0.80);
        // 某副脑 85/100 = 85% > 80% 阈值
        let snapshots = vec![make_snapshot("memory", 85, 0.8)];
        let result = brain.should_evaluate(&snapshots, 10, false);
        assert!(result);
    }

    #[test]
    fn evaluate_generates_correct_instructions() {
        let brain = make_brain_with_config(100, 300, 0.80);
        let snapshots = vec![
            make_snapshot("reasoning", 10, 0.95), // 健康
            make_snapshot("memory", 98, 0.2),     // 过载
        ];
        let result = brain.evaluate(snapshots);

        // 瘦身指令已禁用，只验证健康度报告
        assert_eq!(result.brain_reports.len(), 2);
        assert!(result.slim_instructions.is_empty());
    }
}
