use std::collections::HashMap;

use brain_core::types::{BrainId, Weight};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 单次表现记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceRecord {
    pub brain_id: BrainId,
    pub task_type: String,
    pub relevant: bool,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
}

/// 任务类型统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskTypeStats {
    pub total_tasks: u32,
    pub relevant_count: u32,
    pub avg_confidence: f64,
}

/// 赫布定律 — 共激活连接强度
///
/// 记录两个副脑在同一任务中同时相关（co-activated）的次数。
/// 赫布定律："一起激发的神经元连接更强"。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoActivationMatrix {
    /// (brain_a, brain_b) → 共激活次数
    /// key 以有序对存储（较小的 id 在前）
    matrix: HashMap<(String, String), u32>,
}

impl CoActivationMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    /// 生成有序 key
    fn make_key(a: &BrainId, b: &BrainId) -> (String, String) {
        if a.0 <= b.0 {
            (a.0.clone(), b.0.clone())
        } else {
            (b.0.clone(), a.0.clone())
        }
    }

    /// 记录一组副脑在同一任务中共激活
    ///
    /// 所有 relevant=true 的副脑两两之间增加计数
    pub fn record_co_activation(&mut self, active_brains: &[BrainId]) {
        for i in 0..active_brains.len() {
            for j in (i + 1)..active_brains.len() {
                let key = Self::make_key(&active_brains[i], &active_brains[j]);
                *self.matrix.entry(key).or_insert(0) += 1;
            }
        }
    }

    /// 获取两个副脑的共激活次数
    pub fn get_co_activation(&self, a: &BrainId, b: &BrainId) -> u32 {
        let key = Self::make_key(a, b);
        self.matrix.get(&key).copied().unwrap_or(0)
    }

    /// 获取与某个副脑共激活最强的 top N 副脑
    pub fn top_partners(&self, brain_id: &BrainId, n: usize) -> Vec<(BrainId, u32)> {
        let mut pairs: Vec<(BrainId, u32)> = self
            .matrix
            .iter()
            .filter_map(|((a, b), count)| {
                if a == &brain_id.0 {
                    Some((BrainId(b.clone()), *count))
                } else if b == &brain_id.0 {
                    Some((BrainId(a.clone()), *count))
                } else {
                    None
                }
            })
            .collect();

        pairs.sort_by(|a, b| b.1.cmp(&a.1));
        pairs.truncate(n);
        pairs
    }
}

/// 权重进化引擎
///
/// 三层进化机制：
/// 1. **基础进化**：相关且质量高 +0.1 / 不相关 -0.05
/// 2. **任务类型感知**：按任务类型统计表现，对特定类型表现好的副脑加权
/// 3. **赫布定律**：一起被激活的副脑连接更强，权重联动调整
///
/// 权重范围 [0.1, 1.0]
pub struct WeightEngine {
    weights: HashMap<BrainId, Weight>,
    /// 表现历史（最近 N 条）
    history: Vec<PerformanceRecord>,
    /// 最大历史记录数
    max_history: usize,
    /// 任务类型统计: task_type → brain_id → stats
    task_stats: HashMap<String, HashMap<BrainId, TaskTypeStats>>,
    /// 赫布共激活矩阵
    co_activation: CoActivationMatrix,
}

/// 赫布定律增强因子
const HEBBIAN_FACTOR: f64 = 0.02;
/// 任务类型感知增强因子
const TASK_TYPE_FACTOR: f64 = 0.05;

impl WeightEngine {
    pub fn new(initial: HashMap<BrainId, Weight>) -> Self {
        Self {
            weights: initial,
            history: Vec::new(),
            max_history: 1000,
            task_stats: HashMap::new(),
            co_activation: CoActivationMatrix::new(),
        }
    }

    /// 获取副脑权重
    pub fn get(&self, id: &BrainId) -> Weight {
        self.weights
            .get(id)
            .copied()
            .unwrap_or_else(Weight::default_value)
    }

    /// 按权重对响应排序（权重高的排前面）
    pub fn sort_by_weight(&self, ids: &[BrainId]) -> Vec<BrainId> {
        let mut sorted: Vec<BrainId> = ids.to_vec();
        sorted.sort_by(|a, b| {
            let wa = self.get(a).value();
            let wb = self.get(b).value();
            wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted
    }

    /// 记录表现并更新权重（增强版）
    ///
    /// 三层进化：
    /// 1. 基础: relevant + high_confidence → +0.1, irrelevant → -0.05
    /// 2. 任务类型: 在该类型中表现好的 → 额外 +TASK_TYPE_FACTOR
    /// 3. 赫布: 共激活的伙伴权重高 → 额外 +HEBBIAN_FACTOR
    pub fn record_performance(
        &mut self,
        brain_id: &BrainId,
        relevant: bool,
        confidence: f64,
        threshold: f64,
    ) {
        self.record_performance_with_task_type(brain_id, relevant, confidence, threshold, "default");
    }

    /// 带任务类型的表现记录
    pub fn record_performance_with_task_type(
        &mut self,
        brain_id: &BrainId,
        relevant: bool,
        confidence: f64,
        threshold: f64,
        task_type: &str,
    ) {
        // 1. 记录历史
        let record = PerformanceRecord {
            brain_id: brain_id.clone(),
            task_type: task_type.into(),
            relevant,
            confidence,
            timestamp: Utc::now(),
        };
        self.history.push(record);
        if self.history.len() > self.max_history {
            self.history.drain(..self.history.len() - self.max_history);
        }

        // 2. 更新任务类型统计
        let stats = self
            .task_stats
            .entry(task_type.into())
            .or_default()
            .entry(brain_id.clone())
            .or_default();
        stats.total_tasks += 1;
        if relevant {
            stats.relevant_count += 1;
        }
        // 更新平均置信度（指数移动平均）
        stats.avg_confidence = if stats.total_tasks == 1 {
            confidence
        } else {
            stats.avg_confidence * 0.7 + confidence * 0.3
        };

        // 3. 基础权重调整
        if let Some(w) = self.weights.get_mut(brain_id) {
            if relevant && confidence > threshold {
                w.strengthen(0.1);
            } else if !relevant {
                w.weaken(0.05);
            }
        }

        // 4. 任务类型感知增强
        if relevant && confidence > threshold {
            if let Some(w) = self.weights.get_mut(brain_id) {
                w.strengthen(TASK_TYPE_FACTOR);
            }
        }
    }

    /// 记录一轮任务中所有激活的副脑（赫布定律）
    ///
    /// 在一轮快思考完成后调用，传入所有 relevant=true 的副脑列表。
    /// 共激活的副脑对彼此的权重有联动增强。
    pub fn record_round_co_activation(&mut self, active_brains: &[BrainId]) {
        self.co_activation.record_co_activation(active_brains);

        // 赫布定律：如果两个副脑经常一起激活，且伙伴权重高，则增强当前副脑
        for brain_id in active_brains {
            let partners = self.co_activation.top_partners(brain_id, 3);
            for (_, co_count) in &partners {
                if *co_count >= 3 {
                    // 共激活 3 次以上，触发赫布增强
                    if let Some(w) = self.weights.get_mut(brain_id) {
                        w.strengthen(HEBBIAN_FACTOR);
                    }
                }
            }
        }
    }

    /// 获取某个副脑在特定任务类型的表现统计
    pub fn get_task_stats(
        &self,
        task_type: &str,
        brain_id: &BrainId,
    ) -> Option<&TaskTypeStats> {
        self.task_stats.get(task_type).and_then(|m| m.get(brain_id))
    }

    /// 获取两个副脑的共激活次数
    pub fn get_co_activation(&self, a: &BrainId, b: &BrainId) -> u32 {
        self.co_activation.get_co_activation(a, b)
    }

    /// 所有权重快照
    pub fn all_weights(&self) -> &HashMap<BrainId, Weight> {
        &self.weights
    }

    /// 获取表现历史
    pub fn history(&self) -> &[PerformanceRecord] {
        &self.history
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weight_engine_new() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.5));
        init.insert(BrainId::memory(), Weight(0.8));

        let engine = WeightEngine::new(init);
        assert!((engine.get(&BrainId::reasoning()).value() - 0.5).abs() < f64::EPSILON);
        assert!((engine.get(&BrainId::memory()).value() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_sort_by_weight() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.3));
        init.insert(BrainId::memory(), Weight(0.9));
        init.insert(BrainId::motor(), Weight(0.5));

        let engine = WeightEngine::new(init);
        let sorted =
            engine.sort_by_weight(&[BrainId::reasoning(), BrainId::motor(), BrainId::memory()]);

        assert_eq!(sorted[0], BrainId::memory());
        assert_eq!(sorted[1], BrainId::motor());
        assert_eq!(sorted[2], BrainId::reasoning());
    }

    #[test]
    fn test_record_performance_strengthen() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.5));

        let mut engine = WeightEngine::new(init);
        engine.record_performance(&BrainId::reasoning(), true, 0.9, 0.7);

        let w = engine.get(&BrainId::reasoning()).value();
        // 基础 +0.1 + 任务类型 +0.05 = +0.15
        assert!((w - 0.65).abs() < f64::EPSILON);
    }

    #[test]
    fn test_record_performance_weaken() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.5));

        let mut engine = WeightEngine::new(init);
        engine.record_performance(&BrainId::reasoning(), false, 0.3, 0.7);

        let w = engine.get(&BrainId::reasoning()).value();
        assert!((w - 0.45).abs() < f64::EPSILON);
    }

    #[test]
    fn test_task_type_stats() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.5));

        let mut engine = WeightEngine::new(init);
        engine.record_performance_with_task_type(
            &BrainId::reasoning(), true, 0.9, 0.7, "code_generation",
        );
        engine.record_performance_with_task_type(
            &BrainId::reasoning(), true, 0.8, 0.7, "code_generation",
        );
        engine.record_performance_with_task_type(
            &BrainId::reasoning(), false, 0.3, 0.7, "code_generation",
        );

        let stats = engine.get_task_stats("code_generation", &BrainId::reasoning()).unwrap();
        assert_eq!(stats.total_tasks, 3);
        assert_eq!(stats.relevant_count, 2);
    }

    #[test]
    fn test_co_activation_basic() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.5));
        init.insert(BrainId::memory(), Weight(0.5));
        init.insert(BrainId::motor(), Weight(0.5));

        let mut engine = WeightEngine::new(init);

        // 模拟多轮共激活
        let active = vec![BrainId::reasoning(), BrainId::memory()];
        for _ in 0..3 {
            engine.record_round_co_activation(&active);
        }

        let co = engine.get_co_activation(&BrainId::reasoning(), &BrainId::memory());
        assert!(co >= 3);
    }

    #[test]
    fn test_hebbian_weight_enhancement() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.5));
        init.insert(BrainId::memory(), Weight(0.5));

        let mut engine = WeightEngine::new(init);

        // 共激活 4 次（超过阈值 3）
        let active = vec![BrainId::reasoning(), BrainId::memory()];
        for _ in 0..4 {
            engine.record_round_co_activation(&active);
        }

        // 两个副脑权重都应被增强
        let r = engine.get(&BrainId::reasoning()).value();
        let m = engine.get(&BrainId::memory()).value();
        assert!(r > 0.5);
        assert!(m > 0.5);
    }

    #[test]
    fn test_history_trimming() {
        let mut init = HashMap::new();
        init.insert(BrainId::reasoning(), Weight(0.5));

        let mut engine = WeightEngine::new(init);
        engine.max_history = 10;

        for _ in 0..20 {
            engine.record_performance(&BrainId::reasoning(), true, 0.8, 0.7);
            // 检查历史不超过上限
            assert!(engine.history().len() <= 10);
        }
    }

    #[test]
    fn test_co_activation_matrix_top_partners() {
        let mut matrix = CoActivationMatrix::new();
        let active = vec![BrainId::reasoning(), BrainId::memory(), BrainId::motor()];
        for _ in 0..5 {
            matrix.record_co_activation(&active);
        }

        let partners = matrix.top_partners(&BrainId::reasoning(), 3);
        assert_eq!(partners.len(), 2);
        // 所有伙伴的共激活次数应相同（都参与了相同轮次）
        assert!(partners.iter().all(|(_, c)| *c == 5));
    }
}
