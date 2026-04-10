//! 最小闭环集成测试
//!
//! 验证三通道消息流转的完整闭环

use brain_bus::BrainBus;
use brain_core::types::{
    BrainContext, BrainId, BrainResponse, BrainResponsePayload, BroadcastMessage,
    CollaborationKind, CollaborationMessage, FastThinkResult, MessagePriority,
};

/// 构造一条广播消息
fn make_broadcast(content: &str, raw: &str) -> BroadcastMessage {
    BroadcastMessage {
        content: content.into(),
        raw_input: raw.into(),
        context: BrainContext {
            current_date: "2026-04-03".into(),
            cwd: "/test".into(),
            git_branch: None,
            platform: "darwin".into(),
        },
        timestamp: chrono::Utc::now(),
    }
}

/// 模拟副脑快思考 → 返回 BrainResponse
fn fast_think_response(brain_id: &BrainId, msg: &BroadcastMessage, keyword: &str) -> BrainResponse {
    let relevant = msg.content.contains(keyword) || msg.raw_input.contains(keyword);
    let confidence = if relevant { 0.85 } else { 0.1 };

    let result = if relevant {
        BrainResponsePayload::FastThink(FastThinkResult {
            relevant: true,
            confidence,
            summary: Some(format!("{brain_id} 认为相关")),
            suggested_tools: vec!["WebSearch".into()],
            matched_experience: None,
        })
    } else {
        BrainResponsePayload::NotRelevant {
            reason: format!("{brain_id} 认为不相关"),
        }
    };

    BrainResponse {
        from: brain_id.clone(),
        relevance: confidence,
        confidence,
        result,
        need_slow_think: relevant && confidence < 0.7,
        timestamp: chrono::Utc::now(),
    }
}

// ─── 测试1: 广播 → 快思考 → 结果汇总 ─────────────────────────

#[tokio::test]
async fn test_broadcast_to_result_full_loop() {
    let bus = BrainBus::new(16, 16, 16);

    // 1. 各副脑订阅广播（必须在 broadcast 之前）
    let mut rx_reasoning = bus.subscribe_broadcast();
    let mut rx_memory = bus.subscribe_broadcast();
    let mut rx_motor = bus.subscribe_broadcast();
    assert_eq!(bus.broadcast_receiver_count(), 3);

    // 2. 感知脑投递广播
    let msg = make_broadcast("用户查询2026年4月节假日信息", "这个月有节假日吗？");
    bus.broadcast(msg).unwrap();

    // 3. 各副脑接收广播 → 快思考
    let msg_r = rx_reasoning.recv().await.unwrap();
    let msg_m = rx_memory.recv().await.unwrap();
    let msg_mo = rx_motor.recv().await.unwrap();

    let resp_r = fast_think_response(&BrainId::reasoning(), &msg_r, "节假日");
    let resp_m = fast_think_response(&BrainId::memory(), &msg_m, "节假日");
    let resp_mo = fast_think_response(&BrainId::motor(), &msg_mo, "代码");

    // 4. 提交结果到通道3
    bus.submit_result(resp_r).await.unwrap();
    bus.submit_result(resp_m).await.unwrap();
    bus.submit_result(resp_mo).await.unwrap();

    // 5. 主脑收集结果
    let mut result_rx = bus.take_result_receiver().await.unwrap();
    let mut responses = Vec::new();
    for _ in 0..3 {
        let resp = result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .await
            .unwrap();
        responses.push(resp);
    }
    assert_eq!(responses.len(), 3);

    // 6. 验证：推理脑和记忆脑相关，执行脑不相关
    let r = responses
        .iter()
        .find(|r| r.from == BrainId::reasoning())
        .unwrap();
    assert!(matches!(&r.result, BrainResponsePayload::FastThink(f) if f.relevant));

    let m = responses
        .iter()
        .find(|r| r.from == BrainId::memory())
        .unwrap();
    assert!(matches!(&m.result, BrainResponsePayload::FastThink(f) if f.relevant));

    let mo = responses
        .iter()
        .find(|r| r.from == BrainId::motor())
        .unwrap();
    assert!(matches!(
        &mo.result,
        BrainResponsePayload::NotRelevant { .. }
    ));
}

// ─── 测试2: 副脑间协作（推理脑 ↔ 记忆脑）─────────────────────

#[tokio::test]
async fn test_collaboration_between_brains() {
    let bus = BrainBus::new(16, 16, 16);

    // 注册协作通道
    let mut rx_reasoning = bus.subscribe_collaboration(BrainId::reasoning()).await;
    let mut rx_memory = bus.subscribe_collaboration(BrainId::memory()).await;

    // 推理脑 → 记忆脑：请求召回
    let request = CollaborationMessage {
        id: "collab_001".into(),
        from: BrainId::reasoning(),
        to: vec![BrainId::memory()],
        correlation_id: None,
        hop_count: 0,
        priority: MessagePriority::High,
        content: "请召回订舱接口开发的相关记忆".into(),
        kind: CollaborationKind::Request,
    };

    bus.send_collaboration(request).await.unwrap();

    // 记忆脑收到
    let received = rx_memory.recv().await.unwrap();
    assert_eq!(received.content, "请召回订舱接口开发的相关记忆");
    assert_eq!(received.from, BrainId::reasoning());
    assert_eq!(received.hop_count, 1);

    // 记忆脑 → 推理脑：回复
    let response = CollaborationMessage {
        id: "collab_002".into(),
        from: BrainId::memory(),
        to: vec![BrainId::reasoning()],
        correlation_id: Some("collab_001".into()),
        hop_count: 0,
        priority: MessagePriority::Normal,
        content: "找到3条相关记忆：HttpParseService模式...".into(),
        kind: CollaborationKind::Response,
    };

    bus.send_collaboration(response).await.unwrap();

    // 推理脑收到回复
    let reply = rx_reasoning.recv().await.unwrap();
    assert_eq!(reply.kind, CollaborationKind::Response);
    assert_eq!(reply.correlation_id, Some("collab_001".into()));
}

// ─── 测试3: 防循环（hop_count > 3 丢弃）─────────────────────

#[tokio::test]
async fn test_hop_count_prevents_loop() {
    let bus = BrainBus::new(16, 16, 16);
    let _rx = bus.subscribe_collaboration(BrainId::memory()).await;

    let loop_msg = CollaborationMessage {
        id: "loop".into(),
        from: BrainId::reasoning(),
        to: vec![BrainId::memory()],
        correlation_id: None,
        hop_count: 4, // > 3
        priority: MessagePriority::High,
        content: "循环消息".into(),
        kind: CollaborationKind::Request,
    };

    let result = bus.send_collaboration(loop_msg).await;
    assert!(result.is_err());
}
