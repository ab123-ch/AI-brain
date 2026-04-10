//! 最小闭环集成测试
//!
//! 验证完整流程：
//!   输入 -> 感知脑 -> 通道1广播 -> StubBrain快思考 -> 通道3结果 -> 主脑汇总 -> 输出

use std::sync::Arc;

use brain_bus::{BrainBus, BroadcastReceiver};
use brain_core::config::BrainConfig;
use brain_core::types::{BrainId, BrainResponse, BrainResponsePayload, FastThinkResult};
use brain_master::MasterBrain;
use brain_sensory::llm::StubLlmProvider;
use brain_sensory::SensoryBrain;
use chrono::Utc;

/// 模拟副脑：接收广播 -> 快思考 -> 提交结果
///
/// 接收一个已经 subscribe 好的 rx，避免 race condition。
async fn stub_brain_respond(
    bus: Arc<BrainBus>,
    mut rx: BroadcastReceiver,
    brain_id: BrainId,
    summary: &str,
    relevant: bool,
) {
    let _msg = rx.recv().await.unwrap();

    let response = BrainResponse {
        from: brain_id.clone(),
        relevance: if relevant { 0.9 } else { 0.1 },
        confidence: if relevant { 0.85 } else { 0.1 },
        result: if relevant {
            BrainResponsePayload::FastThink(FastThinkResult {
                relevant: true,
                confidence: 0.85,
                summary: Some(summary.into()),
                suggested_tools: vec![],
                matched_experience: None,
            })
        } else {
            BrainResponsePayload::NotRelevant {
                reason: format!("{brain_id}: not my domain"),
            }
        },
        need_slow_think: false,
        timestamp: Utc::now(),
    };

    bus.submit_result(response).await.unwrap();
}

/// 测试1: 单脑闭环 — 感知脑 -> 推理脑快思考 -> 主脑汇总
#[tokio::test]
async fn single_brain_loop() {
    let bus = Arc::new(BrainBus::new(64, 64, 64));
    let sensory = SensoryBrain::new("haiku", bus.clone(), Box::new(StubLlmProvider::default()));
    let mut master = MasterBrain::new(BrainConfig::default(), bus.clone()).unwrap();

    let mut broadcast_rx = bus.subscribe_broadcast();
    let mut result_rx = bus.take_result_receiver().await.unwrap();

    // 先 subscribe，再 spawn，确保不会错过广播
    let reasoning_rx = bus.subscribe_broadcast();
    let reasoning_handle = tokio::spawn(stub_brain_respond(
        bus.clone(),
        reasoning_rx,
        BrainId::reasoning(),
        "根据记忆，4月清明节4月4-6日放假3天",
        true,
    ));

    // 感知脑投递广播
    let result = sensory.process_input("这个月有节假日吗？").await.unwrap();
    assert!(result.contains("这个月有节假日吗？"));

    // 等待推理脑完成
    reasoning_handle.await.unwrap();

    // 主脑汇总
    let output = master
        .run_once(&mut broadcast_rx, &mut result_rx)
        .await
        .unwrap();

    assert!(
        output.answer.contains("清明节"),
        "expected 清明节 in answer, got: {}",
        output.answer
    );
    assert!(output.participating_brains.contains(&BrainId::reasoning()));
    assert!(output.confidence > 0.0);
}

/// 测试2: 多脑闭环 — 推理脑 + 记忆脑相关，执行脑不相关
#[tokio::test]
async fn multi_brain_loop() {
    let bus = Arc::new(BrainBus::new(64, 64, 64));
    let sensory = SensoryBrain::new("haiku", bus.clone(), Box::new(StubLlmProvider::default()));
    let mut master = MasterBrain::new(BrainConfig::default(), bus.clone()).unwrap();

    let mut broadcast_rx = bus.subscribe_broadcast();
    let mut result_rx = bus.take_result_receiver().await.unwrap();

    // 先 subscribe 所有 stub brain
    let reasoning_rx = bus.subscribe_broadcast();
    let memory_rx = bus.subscribe_broadcast();
    let motor_rx = bus.subscribe_broadcast();

    // 推理脑（相关）
    let reasoning = tokio::spawn(stub_brain_respond(
        bus.clone(),
        reasoning_rx,
        BrainId::reasoning(),
        "推理结论：4月有清明节放假",
        true,
    ));

    // 记忆脑（相关）
    let memory = tokio::spawn(stub_brain_respond(
        bus.clone(),
        memory_rx,
        BrainId::memory(),
        "记忆召回：去年4月也有清明假期",
        true,
    ));

    // 执行脑（不相关）
    let motor = tokio::spawn(stub_brain_respond(
        bus.clone(),
        motor_rx,
        BrainId::motor(),
        "",
        false,
    ));

    sensory.process_input("四月有什么节假日？").await.unwrap();

    reasoning.await.unwrap();
    memory.await.unwrap();
    motor.await.unwrap();

    let output = master
        .run_once(&mut broadcast_rx, &mut result_rx)
        .await
        .unwrap();

    assert!(output.participating_brains.contains(&BrainId::reasoning()));
    assert!(output.participating_brains.contains(&BrainId::memory()));
    assert!(!output.participating_brains.contains(&BrainId::motor()));
}

/// 测试3: LLM 降级 — LLM 失败时感知脑优雅降级
#[tokio::test]
async fn llm_degradation() {
    use brain_sensory::llm::FailingLlmProvider;

    let bus = Arc::new(BrainBus::new(64, 64, 64));
    let sensory = SensoryBrain::new("haiku", bus.clone(), Box::new(FailingLlmProvider));

    let mut broadcast_rx = bus.subscribe_broadcast();

    // LLM 失败，应降级为原始输入
    let result = sensory.process_input("测试降级输入").await.unwrap();
    assert!(
        result.contains("[未解析]"),
        "expected degradation marker, got: {result}"
    );

    // 广播仍应成功投递
    let msg = broadcast_rx.recv().await.unwrap();
    assert_eq!(msg.raw_input, "测试降级输入");
    assert_eq!(msg.content, "[未解析] 测试降级输入");
}
