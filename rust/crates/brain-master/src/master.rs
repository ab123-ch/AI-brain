use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use brain_bus::{BrainBus, BroadcastReceiver, ResultReceiver};
use brain_core::config::BrainConfig;
use brain_core::types::{
    BrainId, BrainResponse, BrainResponsePayload, BroadcastMessage, CollaborationKind,
    CollaborationMessage, MasterOutput, MessagePriority, TaskContext, TaskPhase, TurnUsage, Weight,
};
use chrono::Utc;

use crate::error::MasterError;
use crate::weight_engine::WeightEngine;

/// 主脑 — 裁判 + 调度器
///
/// 不实现 BrainAgent trait。它是副脑的调度者，不是被调度者。
///
/// 职责:
///   1. 监听通道1（广播）— 记录任务上下文
///   2. 收集通道3（结果）— 权重排序
///   3. 调度慢思考 — 通过通道2发送调度指令
///   4. 输出最终响应
pub struct MasterBrain {
    id: BrainId,
    bus: Arc<BrainBus>,
    weights: WeightEngine,
    config: BrainConfig,
    current_task: Option<TaskContext>,
}

/// 快思考收集超时
const FAST_THINK_TIMEOUT: Duration = Duration::from_secs(5);
/// 慢思考收集超时（LLM 推理可能需要较长时间）
const SLOW_THINK_TIMEOUT: Duration = Duration::from_secs(60);

impl MasterBrain {
    /// 创建主脑
    pub fn new(config: BrainConfig, bus: Arc<BrainBus>) -> Result<Self, MasterError> {
        let weights = WeightEngine::new(config.brain.weights.to_map());
        Ok(Self {
            id: BrainId::master(),
            bus,
            weights,
            config,
            current_task: None,
        })
    }

    /// 获取 bus 引用（测试用）
    pub fn bus(&self) -> &Arc<BrainBus> {
        &self.bus
    }

    /// 主循环 — 处理一个完整任务
    ///
    /// 阻塞运行，直到一个任务完成。流程：
    ///   1. 等待广播消息（通道1）
    ///   2. 等待快思考结果（通道3，超时 5s）
    ///   3. 分析 need_slow_think → 调度慢思考（通道2）
    ///   4. 收集慢思考结果（通道3，超时 10s）
    ///   5. 权重排序 + 决策
    ///   6. 汇总输出 MasterOutput
    pub async fn run_once(
        &mut self,
        broadcast_rx: &mut BroadcastReceiver,
        result_rx: &mut ResultReceiver,
    ) -> Result<MasterOutput, MasterError> {
        let start = Utc::now();

        // 1. 等待广播
        let broadcast = broadcast_rx.recv().await?;
        self.current_task = Some(TaskContext {
            input: broadcast.clone(),
            start_at: start,
            phase: TaskPhase::WaitingFastThink,
            brain_responses: HashMap::new(),
        });

        // 2. 收集快思考结果
        let fast_responses = self.collect_fast_thinks(result_rx).await;

        // 3. 检查是否需要慢思考
        let slow_targets: Vec<BrainId> = fast_responses
            .iter()
            .filter(|r| r.need_slow_think)
            .map(|r| r.from.clone())
            .collect();

        let slow_responses = if slow_targets.is_empty() {
            Vec::new()
        } else {
            // 更新任务阶段
            if let Some(task) = &mut self.current_task {
                task.phase = TaskPhase::DispatchingSlowThink;
            }

            // 通过协作通道发送调度指令
            self.dispatch_slow_think(&slow_targets, &broadcast).await;

            // 收集慢思考结果
            if let Some(task) = &mut self.current_task {
                task.phase = TaskPhase::WaitingSlowThink;
            }
            self.collect_slow_thinks(result_rx, slow_targets.len())
                .await
        };

        // 4. 合并快+慢结果
        let all_responses = self.merge_responses(fast_responses, slow_responses);

        // 5. 权重排序
        let brain_ids: Vec<BrainId> = all_responses.iter().map(|r| r.from.clone()).collect();
        let sorted_ids = self.weights.sort_by_weight(&brain_ids);

        // 6. 更新权重（三层进化）
        let threshold = self.config.brain.thresholds.fast_think_confidence;

        // 从广播内容推断任务类型（简化版关键词匹配）
        let task_type = self.infer_task_type(&broadcast.content);

        let mut active_brains = Vec::new();
        for resp in &all_responses {
            let relevant = !matches!(resp.result, BrainResponsePayload::NotRelevant { .. });
            self.weights.record_performance_with_task_type(
                &resp.from,
                relevant,
                resp.confidence,
                threshold,
                &task_type,
            );
            if relevant {
                active_brains.push(resp.from.clone());
            }
        }

        // 赫布定律: 共激活的副脑权重联动
        self.weights.record_round_co_activation(&active_brains);

        // 7. 汇总输出
        let output = self.synthesize(&broadcast, &all_responses, &sorted_ids, start);

        // 清理任务上下文
        self.current_task = None;

        Ok(output)
    }

    /// 收集快思考结果（带超时）
    async fn collect_fast_thinks(&mut self, result_rx: &mut ResultReceiver) -> Vec<BrainResponse> {
        let mut responses = Vec::new();
        let deadline = tokio::time::Instant::now() + FAST_THINK_TIMEOUT;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            match result_rx.recv_timeout(remaining).await {
                Ok(resp) => {
                    tracing::debug!("received response from {}", resp.from);
                    if let Some(task) = &mut self.current_task {
                        task.brain_responses.insert(resp.from.clone(), resp.clone());
                    }
                    responses.push(resp);
                }
                Err(_) => break, // 超时或通道关闭
            }
        }

        responses
    }

    /// 通过协作通道调度慢思考（两阶段：先记忆脑召回，再推理脑深度思考）
    ///
    /// Phase 1: 向记忆脑发送 Dispatch → 记忆脑召回相关记忆 → 通过通道3返回结果
    /// Phase 2: 向其他慢思考目标（推理脑等）发送 Dispatch
    async fn dispatch_slow_think(&self, targets: &[BrainId], broadcast: &BroadcastMessage) {
        let memory_id = BrainId::memory();
        let memory_targets: Vec<&BrainId> = targets.iter().filter(|id| **id == memory_id).collect();
        let other_targets: Vec<&BrainId> = targets.iter().filter(|id| **id != memory_id).collect();

        // Phase 1: 记忆脑优先调度
        for target in &memory_targets {
            let msg = CollaborationMessage {
                id: format!("slow-mem-{}-{}", Utc::now().timestamp_millis(), target),
                from: self.id.clone(),
                to: vec![(*target).clone()],
                correlation_id: None,
                hop_count: 0,
                priority: MessagePriority::High,
                content: broadcast.content.clone(),
                kind: CollaborationKind::Dispatch,
            };
            if let Err(e) = self.bus.send_collaboration(msg).await {
                tracing::warn!("慢思考调度失败 (memory → {}): {e}", target);
            } else {
                tracing::info!("慢思考 Phase 1: 记忆脑调度 → {target}");
            }
        }

        // 给记忆脑一个短窗口完成召回（结果通过通道3返回）
        if !memory_targets.is_empty() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Phase 2: 推理脑等其他目标调度
        for target in &other_targets {
            let msg = CollaborationMessage {
                id: format!("slow-reas-{}-{}", Utc::now().timestamp_millis(), target),
                from: self.id.clone(),
                to: vec![(*target).clone()],
                correlation_id: Some(format!("slow-{}", Utc::now().timestamp_millis())),
                hop_count: 0,
                priority: MessagePriority::High,
                content: broadcast.content.clone(),
                kind: CollaborationKind::Dispatch,
            };
            if let Err(e) = self.bus.send_collaboration(msg).await {
                tracing::warn!("慢思考调度失败 (→ {}): {e}", target);
            } else {
                tracing::info!("慢思考 Phase 2: 推理调度 → {target}");
            }
        }
    }

    /// 收集慢思考结果（带超时，收齐即返回）
    async fn collect_slow_thinks(
        &mut self,
        result_rx: &mut ResultReceiver,
        expected_count: usize,
    ) -> Vec<BrainResponse> {
        let mut responses = Vec::new();
        let mut completed_count = 0;
        let deadline = tokio::time::Instant::now() + SLOW_THINK_TIMEOUT;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!(
                    "慢思考超时: 已收 {}/{} 个结果",
                    completed_count,
                    expected_count
                );
                break;
            }

            match result_rx.recv_timeout(remaining).await {
                Ok(resp) => {
                    if matches!(resp.result, BrainResponsePayload::Processing) {
                        tracing::info!("收到 {} 的 Processing ack", resp.from);
                    } else {
                        tracing::info!(
                            "收到 {} 的慢思考结果 (confidence={:.2})",
                            resp.from,
                            resp.confidence
                        );
                        if let Some(task) = &mut self.current_task {
                            task.brain_responses.insert(resp.from.clone(), resp.clone());
                        }
                        responses.push(resp);
                        completed_count += 1;
                    }

                    // 收齐了，立刻返回
                    if completed_count >= expected_count {
                        tracing::info!("慢思考全部收齐: {} 个结果，无需等待超时", completed_count);
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        tracing::info!(
            "慢思考收集完成: {}/{} 个结果",
            completed_count,
            expected_count
        );
        responses
    }

    /// 合并快+慢思考结果（慢思考覆盖同副脑的快思考）
    fn merge_responses(
        &self,
        fast: Vec<BrainResponse>,
        slow: Vec<BrainResponse>,
    ) -> Vec<BrainResponse> {
        let mut merged: HashMap<BrainId, BrainResponse> = HashMap::new();

        // 先插入快思考
        for resp in fast {
            merged.insert(resp.from.clone(), resp);
        }

        // 慢思考覆盖快思考（同副脑取最新）
        for resp in slow {
            merged.insert(resp.from.clone(), resp);
        }

        merged.into_values().collect()
    }

    /// 汇总各副脑响应为最终输出
    fn synthesize(
        &self,
        broadcast: &BroadcastMessage,
        responses: &[BrainResponse],
        sorted_ids: &[BrainId],
        start: chrono::DateTime<Utc>,
    ) -> MasterOutput {
        let mut answer_parts = Vec::new();
        let mut sources = Vec::new();
        let mut participating = Vec::new();
        let mut total_confidence = 0.0;
        let mut confidence_count = 0;

        for id in sorted_ids {
            if let Some(resp) = responses.iter().find(|r| r.from == *id) {
                if matches!(resp.result, BrainResponsePayload::NotRelevant { .. })
                    || matches!(resp.result, BrainResponsePayload::Processing)
                {
                    continue;
                }

                participating.push(id.clone());
                total_confidence += resp.confidence;
                confidence_count += 1;

                match &resp.result {
                    BrainResponsePayload::FastThink(ft) => {
                        if let Some(summary) = &ft.summary {
                            answer_parts.push(summary.clone());
                        }
                    }
                    BrainResponsePayload::SlowThink(st) => {
                        answer_parts.push(st.conclusion.clone());
                        sources.extend(st.sources.clone());
                    }
                    BrainResponsePayload::MemoryRecall(entries) => {
                        for entry in entries {
                            answer_parts.push(entry.content.clone());
                        }
                    }
                    BrainResponsePayload::ToolResult(tool_result) => {
                        answer_parts.push(tool_result.output.clone());
                    }
                    BrainResponsePayload::SafetyCheck(_)
                    | BrainResponsePayload::TruthfulnessCheck(_)
                    | BrainResponsePayload::Evaluation(_)
                    | BrainResponsePayload::Processing => {
                        // 校验/评估/Processing ack 不直接拼到回答中
                    }
                    BrainResponsePayload::NotRelevant { .. } => unreachable!(),
                }
            }
        }

        let answer = if answer_parts.is_empty() {
            format!(
                "收到输入「{}」，但没有副脑给出相关响应。",
                broadcast.raw_input
            )
        } else {
            answer_parts.join("\n")
        };

        let confidence = if confidence_count > 0 {
            total_confidence / confidence_count as f64
        } else {
            0.0
        };

        MasterOutput {
            answer,
            confidence,
            sources,
            participating_brains: participating,
            usage: TurnUsage {
                total_tokens: 0,
                llm_calls: 0,
                duration_ms: (Utc::now() - start).num_milliseconds().max(0) as u64,
                prompt_tokens: 0,
                completion_tokens: 0,
            },
        }
    }

    /// 获取当前任务上下文
    pub fn current_task(&self) -> Option<&TaskContext> {
        self.current_task.as_ref()
    }

    /// 获取所有副脑权重
    pub fn get_weights(&self) -> &HashMap<BrainId, Weight> {
        self.weights.all_weights()
    }

    pub fn id(&self) -> &BrainId {
        &self.id
    }

    /// 从内容推断任务类型（简化版关键词匹配）
    fn infer_task_type(&self, content: &str) -> String {
        let content_lower = content.to_lowercase();
        if content_lower.contains("代码")
            || content_lower.contains("调试")
            || content_lower.contains("bug")
        {
            "code_debugging".into()
        } else if content_lower.contains("搜索")
            || content_lower.contains("查找")
            || content_lower.contains("文件")
        {
            "file_search".into()
        } else if content_lower.contains("修改")
            || content_lower.contains("编辑")
            || content_lower.contains("创建")
        {
            "code_editing".into()
        } else if content_lower.contains("运行")
            || content_lower.contains("执行")
            || content_lower.contains("命令")
        {
            "command_execution".into()
        } else {
            "general".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::{BrainContext, FastThinkResult};

    fn make_config() -> BrainConfig {
        BrainConfig::default()
    }

    #[tokio::test]
    async fn test_master_run_once() {
        let bus = Arc::new(BrainBus::new(16, 16, 16));
        let config = make_config();
        let mut master = MasterBrain::new(config, bus.clone()).unwrap();

        let mut broadcast_rx = bus.subscribe_broadcast();
        let mut result_rx = bus.take_result_receiver().await.unwrap();

        // 发送广播
        let msg = BroadcastMessage {
            content: "用户查询节假日".into(),
            raw_input: "这个月有节假日吗？".into(),
            context: BrainContext {
                current_date: "2026-04-04".into(),
                cwd: "/test".into(),
                git_branch: None,
                platform: "darwin".into(),
            },
            timestamp: Utc::now(),
        };
        bus.broadcast(msg.clone()).unwrap();

        // 模拟副脑提交快思考结果
        let resp = BrainResponse {
            from: BrainId::reasoning(),
            relevance: 0.9,
            confidence: 0.85,
            result: BrainResponsePayload::FastThink(FastThinkResult {
                relevant: true,
                confidence: 0.85,
                summary: Some("根据记忆，4月有清明节(4月4-6日)".into()),
                suggested_tools: vec![],
                matched_experience: None,
            }),
            need_slow_think: false,
            timestamp: Utc::now(),
        };
        bus.submit_result(resp).await.unwrap();

        // 主脑运行
        let output = master
            .run_once(&mut broadcast_rx, &mut result_rx)
            .await
            .unwrap();
        assert!(output.answer.contains("清明节"));
        assert!(output.participating_brains.contains(&BrainId::reasoning()));
        assert!(output.confidence > 0.0);
    }

    #[tokio::test]
    async fn test_master_no_relevant_responses() {
        let bus = Arc::new(BrainBus::new(16, 16, 16));
        let config = make_config();
        let mut master = MasterBrain::new(config, bus.clone()).unwrap();

        let mut broadcast_rx = bus.subscribe_broadcast();
        let mut result_rx = bus.take_result_receiver().await.unwrap();

        let msg = BroadcastMessage {
            content: "测试".into(),
            raw_input: "测试输入".into(),
            context: BrainContext {
                current_date: "2026-04-04".into(),
                cwd: "/test".into(),
                git_branch: None,
                platform: "darwin".into(),
            },
            timestamp: Utc::now(),
        };
        bus.broadcast(msg).unwrap();

        // 提交一个 NotRelevant
        let resp = BrainResponse {
            from: BrainId::motor(),
            relevance: 0.1,
            confidence: 0.1,
            result: BrainResponsePayload::NotRelevant {
                reason: "not related to tool execution".into(),
            },
            need_slow_think: false,
            timestamp: Utc::now(),
        };
        bus.submit_result(resp).await.unwrap();

        let output = master
            .run_once(&mut broadcast_rx, &mut result_rx)
            .await
            .unwrap();
        assert!(output.answer.contains("没有副脑给出相关响应"));
        assert!(output.participating_brains.is_empty());
    }
}
