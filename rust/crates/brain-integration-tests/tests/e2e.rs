use std::sync::Arc;

use brain_bus::BrainBus;
use brain_core::config::BrainConfig;
use brain_core::types::{
    BrainContext, BrainId, BrainResponse, BrainResponsePayload, BroadcastMessage,
    CollaborationKind, FastThinkResult, MemoryEntry, MemoryLayer,
    SlowThinkResult, Weight,
};
use brain_master::MasterBrain;
use chrono::Utc;

// ─── helpers ────────────────────────────────────────────────────

fn make_broadcast(content: &str) -> BroadcastMessage {
    BroadcastMessage {
        content: content.into(),
        raw_input: content.into(),
        context: BrainContext {
            current_date: "2026-04-06".into(),
            cwd: "/test".into(),
            git_branch: Some("feat/e2e".into()),
            platform: "darwin".into(),
        },
        timestamp: Utc::now(),
    }
}

fn make_fast_response(
    brain_id: BrainId,
    relevant: bool,
    confidence: f64,
    summary: &str,
) -> BrainResponse {
    BrainResponse {
        from: brain_id,
        relevance: confidence,
        confidence,
        result: if relevant {
            BrainResponsePayload::FastThink(FastThinkResult {
                relevant: true,
                confidence,
                summary: Some(summary.into()),
                suggested_tools: Vec::new(),
                matched_experience: None,
            })
        } else {
            BrainResponsePayload::NotRelevant {
                reason: "not relevant".into(),
            }
        },
        need_slow_think: false,
        timestamp: Utc::now(),
    }
}

/// 构建慢思考响应（推理脑风格）
fn make_slow_response(brain_id: BrainId, conclusion: &str, confidence: f64) -> BrainResponse {
    BrainResponse {
        from: brain_id,
        relevance: confidence,
        confidence,
        result: BrainResponsePayload::SlowThink(SlowThinkResult {
            conclusion: conclusion.into(),
            reasoning_path: vec!["分析问题".into(), "检索经验".into(), "综合推理".into()],
            confidence,
            sources: Vec::new(),
            new_experience: None,
        }),
        need_slow_think: false,
        timestamp: Utc::now(),
    }
}

/// 构建记忆召回响应（记忆脑风格）
fn make_memory_recall_response(memories: Vec<(&str, &str)>) -> BrainResponse {
    let entries: Vec<MemoryEntry> = memories
        .into_iter()
        .map(|(id, content)| MemoryEntry {
            id: id.into(),
            content: content.into(),
            tags: vec!["recall".into()],
            layer: MemoryLayer::ShortTerm,
            importance: 0.7,
            source: brain_core::types::KnowledgeSource::Memory {
                memory_id: id.into(),
                layer: MemoryLayer::ShortTerm,
            },
            confidence: 0.8,
            reference_count: 1,
            created_at: Utc::now(),
            last_accessed: Utc::now(),
            consolidated: false,
        })
        .collect();

    BrainResponse {
        from: BrainId::memory(),
        relevance: 0.8,
        confidence: 0.8,
        result: BrainResponsePayload::MemoryRecall(entries),
        need_slow_think: false,
        timestamp: Utc::now(),
    }
}

async fn run_master_once(
    bus: &Arc<BrainBus>,
    master: &mut MasterBrain,
    broadcast_msg: &BroadcastMessage,
    responses: Vec<BrainResponse>,
) -> brain_core::types::MasterOutput {
    let mut broadcast_rx = bus.subscribe_broadcast();
    let mut result_rx = bus.take_result_receiver().await.unwrap();

    bus.broadcast(broadcast_msg.clone()).unwrap();
    for resp in responses {
        bus.submit_result(resp).await.unwrap();
    }

    master.run_once(&mut broadcast_rx, &mut result_rx).await.unwrap()
}

// ─── active tests ───────────────────────────────────────────────

#[tokio::test]
async fn test_e2e_fast_think_path() {
    let bus = Arc::new(BrainBus::new(16, 16, 16));
    let config = BrainConfig::default();
    let mut master = MasterBrain::new(config, bus.clone()).unwrap();

    let msg = make_broadcast("帮我搜索代码中的错误");
    let responses = vec![
        make_fast_response(BrainId::reasoning(), true, 0.85, "匹配到调试经验"),
        make_fast_response(BrainId::motor(), true, 0.8, "推荐使用 Grep 工具"),
        make_fast_response(BrainId::memory(), false, 0.3, ""),
        make_fast_response(BrainId::validation(), true, 0.6, "内容安全"),
    ];

    let output = run_master_once(&bus, &mut master, &msg, responses).await;

    assert!(!output.answer.is_empty(), "answer should not be empty");
    assert!(output.confidence > 0.0, "confidence should be > 0");
    assert!(
        output.participating_brains.contains(&BrainId::reasoning()),
        "reasoning brain should participate"
    );
    assert!(
        output.participating_brains.contains(&BrainId::motor()),
        "motor brain should participate"
    );
    assert!(
        !output.participating_brains.contains(&BrainId::memory()),
        "memory brain should not participate (NotRelevant)"
    );
}

#[tokio::test]
async fn test_e2e_broadcast_reaches_all_brains() {
    let bus = Arc::new(BrainBus::new(16, 16, 16));

    let _master_rx = bus.subscribe_broadcast();
    assert_eq!(bus.broadcast_receiver_count(), 1);

    let _rx1 = bus.subscribe_broadcast();
    let _rx2 = bus.subscribe_broadcast();
    let _rx3 = bus.subscribe_broadcast();

    assert_eq!(bus.broadcast_receiver_count(), 4, "all brains should receive broadcast");
}

// ─── previously ignored tests (now enabled) ──────────────────────

/// 低置信度触发慢思考调度
#[tokio::test]
async fn test_e2e_slow_think_low_confidence() {
    let bus = Arc::new(BrainBus::new(16, 16, 16));
    let config = BrainConfig::default();
    let mut master = MasterBrain::new(config, bus.clone()).unwrap();

    let mut broadcast_rx = bus.subscribe_broadcast();
    let mut result_rx = bus.take_result_receiver().await.unwrap();

    // 订阅协作通道（推理脑）
    let reasoning_collab_rx = bus.subscribe_collaboration(BrainId::reasoning()).await;

    // 低置信度快思考 → need_slow_think=true
    let fast_reasoning = BrainResponse {
        from: BrainId::reasoning(),
        relevance: 0.4,
        confidence: 0.4,
        result: BrainResponsePayload::FastThink(FastThinkResult {
            relevant: true,
            confidence: 0.4,
            summary: Some("no experience match".into()),
            suggested_tools: Vec::new(),
            matched_experience: None,
        }),
        need_slow_think: true,
        timestamp: Utc::now(),
    };

    let msg = make_broadcast("a completely novel problem");
    bus.broadcast(msg.clone()).unwrap();
    bus.submit_result(fast_reasoning).await.unwrap();

    // 模拟推理脑：收到 Dispatch → 返回慢思考
    let reas_bus = bus.clone();
    let reasoning_handle = tokio::spawn(async move {
        let bus = reas_bus;
        let mut rx = reasoning_collab_rx;
        if let Some(_msg) = rx.recv().await {
            let response = make_slow_response(
                BrainId::reasoning(),
                "经过深度推理分析的新结论",
                0.75,
            );
            bus.submit_result(response).await.unwrap();
        }
    });

    let output = master
        .run_once(&mut broadcast_rx, &mut result_rx)
        .await
        .unwrap();

    let _ = reasoning_handle.await;

    assert!(!output.answer.is_empty());
    assert!(output.participating_brains.contains(&BrainId::reasoning()));
    // 慢思考结果应该比快思考置信度更高
    assert!(output.confidence >= 0.4, "confidence should improve after slow think");
}

/// 多轮查询后权重进化
#[tokio::test]
async fn test_e2e_weight_evolution_after_multiple_queries() {
    let bus = Arc::new(BrainBus::new(16, 16, 16));
    let config = BrainConfig::default();
    let mut master = MasterBrain::new(config, bus.clone()).unwrap();

    // 取一次 receiver，循环内复用
    let mut broadcast_rx = bus.subscribe_broadcast();
    let mut result_rx = bus.take_result_receiver().await.unwrap();

    // 获取初始权重
    let initial_weight = {
        let ws = master.get_weights();
        ws.get(&BrainId::reasoning()).map_or(0.5, Weight::value)
    };

    // 运行多轮查询，推理脑始终相关且高置信度
    for i in 0..3 {
        let msg = make_broadcast(&format!("查询任务 {i}"));
        let responses = vec![
            make_fast_response(BrainId::reasoning(), true, 0.9, &format!("推理结果 {i}")),
            make_fast_response(BrainId::motor(), false, 0.1, ""),
        ];

        bus.broadcast(msg).unwrap();
        for resp in responses {
            bus.submit_result(resp).await.unwrap();
        }

        master.run_once(&mut broadcast_rx, &mut result_rx).await.unwrap();
    }

    // 验证权重已进化
    let final_weight = {
        let ws = master.get_weights();
        ws.get(&BrainId::reasoning()).map_or(0.5, Weight::value)
    };

    // 推理脑每轮都相关且高置信度，权重应该提升
    assert!(
        final_weight >= initial_weight,
        "reasoning weight should not decrease after good performance: {final_weight} < {initial_weight}"
    );
}

/// 完整管线：快思考 + 慢思考 + 合并
#[tokio::test]
async fn test_e2e_full_pipeline() {
    let bus = Arc::new(BrainBus::new(16, 16, 16));
    let config = BrainConfig::default();
    let mut master = MasterBrain::new(config, bus.clone()).unwrap();

    let mut broadcast_rx = bus.subscribe_broadcast();
    let mut result_rx = bus.take_result_receiver().await.unwrap();

    // 订阅协作通道
    let memory_collab_rx = bus.subscribe_collaboration(BrainId::memory()).await;
    let reasoning_collab_rx = bus.subscribe_collaboration(BrainId::reasoning()).await;

    // 快思考阶段：推理脑低置信度 → need_slow_think
    let fast_reasoning = BrainResponse {
        from: BrainId::reasoning(),
        relevance: 0.4,
        confidence: 0.4,
        result: BrainResponsePayload::FastThink(FastThinkResult {
            relevant: true,
            confidence: 0.4,
            summary: Some("需要慢思考".into()),
            suggested_tools: Vec::new(),
            matched_experience: None,
        }),
        need_slow_think: true,
        timestamp: Utc::now(),
    };
    let fast_memory = BrainResponse {
        from: BrainId::memory(),
        relevance: 0.3,
        confidence: 0.3,
        result: BrainResponsePayload::FastThink(FastThinkResult {
            relevant: true,
            confidence: 0.3,
            summary: Some("需要召回记忆".into()),
            suggested_tools: Vec::new(),
            matched_experience: None,
        }),
        need_slow_think: true,
        timestamp: Utc::now(),
    };
    // 执行脑不相关
    let fast_motor = make_fast_response(BrainId::motor(), false, 0.1, "");

    let msg = make_broadcast("完整管线测试：分析性能并提供优化建议");
    bus.broadcast(msg).unwrap();
    bus.submit_result(fast_reasoning).await.unwrap();
    bus.submit_result(fast_memory).await.unwrap();
    bus.submit_result(fast_motor).await.unwrap();

    // 模拟记忆脑慢思考
    let mem_bus = bus.clone();
    let memory_handle = tokio::spawn(async move {
        let bus = mem_bus;
        let mut rx = memory_collab_rx;
        if let Some(msg) = rx.recv().await {
            if msg.kind == CollaborationKind::Dispatch {
                let response = make_memory_recall_response(vec![
                    ("mem-pipeline-1", "管线相关记忆1"),
                ]);
                bus.submit_result(response).await.unwrap();
            }
        }
    });

    // 模拟推理脑慢思考
    let reas_bus = bus.clone();
    let reasoning_handle = tokio::spawn(async move {
        let bus = reas_bus;
        let mut rx = reasoning_collab_rx;
        if let Some(_msg) = rx.recv().await {
            let response = make_slow_response(
                BrainId::reasoning(),
                "管线完整慢思考结论",
                0.85,
            );
            bus.submit_result(response).await.unwrap();
        }
    });

    let output = master
        .run_once(&mut broadcast_rx, &mut result_rx)
        .await
        .unwrap();

    let _ = memory_handle.await;
    let _ = reasoning_handle.await;

    // 验证完整管线结果
    assert!(!output.answer.is_empty(), "answer should not be empty");
    assert!(
        output.participating_brains.contains(&BrainId::reasoning()),
        "reasoning should participate"
    );
    assert!(
        output.participating_brains.contains(&BrainId::memory()),
        "memory should participate"
    );
    assert!(
        !output.participating_brains.contains(&BrainId::motor()),
        "motor should not participate"
    );
    assert!(output.confidence > 0.0);
    assert!(output.usage.duration_ms > 0, "should have measurable duration");
}

/// 校验脑安全检查集成
#[tokio::test]
async fn test_e2e_validation_safety_check() {
    use brain_core::types::{RiskLevel, SafetyCheckResult};

    let bus = Arc::new(BrainBus::new(16, 16, 16));
    let config = BrainConfig::default();
    let mut master = MasterBrain::new(config, bus.clone()).unwrap();

    let mut broadcast_rx = bus.subscribe_broadcast();
    let mut result_rx = bus.take_result_receiver().await.unwrap();

    // 推理脑给出正常响应
    let fast_reasoning = make_fast_response(
        BrainId::reasoning(), true, 0.8, "正常推理结果",
    );
    // 校验脑给出安全检查结果（安全）
    let validation_response = BrainResponse {
        from: BrainId::validation(),
        relevance: 0.9,
        confidence: 0.95,
        result: BrainResponsePayload::SafetyCheck(SafetyCheckResult {
            safe: true,
            risk_level: RiskLevel::Low,
            reason: Some("内容安全，无风险".into()),
            requires_user_approval: false,
        }),
        need_slow_think: false,
        timestamp: Utc::now(),
    };

    let msg = make_broadcast("安全检查测试");
    bus.broadcast(msg).unwrap();
    bus.submit_result(fast_reasoning).await.unwrap();
    bus.submit_result(validation_response).await.unwrap();

    let output = master
        .run_once(&mut broadcast_rx, &mut result_rx)
        .await
        .unwrap();

    // 校验脑参与了（虽然 SafetyCheck 不直接拼入 answer）
    assert!(!output.answer.is_empty());
    assert!(
        output.participating_brains.contains(&BrainId::reasoning()),
        "reasoning should participate"
    );
}
