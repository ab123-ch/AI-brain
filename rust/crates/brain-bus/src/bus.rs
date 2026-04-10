use brain_core::types::{BrainId, BrainResponse, BroadcastMessage, CollaborationMessage};
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::error::BusError;
use crate::router::CollaborationRouter;

/// 三通道消息总线
///
/// 通道1: broadcast  — 感知脑 -> 所有副脑 (`tokio::broadcast`)
/// 通道2: collaboration — 副脑间点对点 (mpsc + router)
/// 通道3: results    — 副脑 → 主脑 (mpsc)
pub struct BrainBus {
    // 通道1
    broadcast_tx: broadcast::Sender<BroadcastMessage>,

    // 通道2（路由器管理分发）
    collab_router: Mutex<CollaborationRouter>,

    // 通道3
    result_tx: mpsc::Sender<BrainResponse>,
    result_rx: Mutex<Option<mpsc::Receiver<BrainResponse>>>,

    // 配置
    collab_capacity: usize,
}

impl BrainBus {
    #[must_use]
    pub fn new(broadcast_capacity: usize, collab_capacity: usize, result_capacity: usize) -> Self {
        let (broadcast_tx, _) = broadcast::channel(broadcast_capacity);
        let (result_tx, result_rx) = mpsc::channel(result_capacity);

        Self {
            broadcast_tx,
            collab_router: Mutex::new(CollaborationRouter::new()),
            result_tx,
            result_rx: Mutex::new(Some(result_rx)),
            collab_capacity,
        }
    }

    // ─── 通道1: 广播 ─────────────────────────────────────────

    /// 感知脑投递广播
    pub fn broadcast(&self, msg: BroadcastMessage) -> Result<(), BusError> {
        self.broadcast_tx
            .send(msg)
            .map(|_| ())
            .map_err(|_| BusError::NoReceivers)
    }

    /// 订阅广播通道
    pub fn subscribe_broadcast(&self) -> BroadcastReceiver {
        BroadcastReceiver {
            inner: self.broadcast_tx.subscribe(),
        }
    }

    // ─── 通道2: 协作 ─────────────────────────────────────────

    /// 注册副脑到协作通道
    pub async fn subscribe_collaboration(&self, brain_id: BrainId) -> CollaborationReceiver {
        let (tx, rx) = mpsc::channel(self.collab_capacity);
        self.collab_router.lock().await.subscribe(brain_id, tx);
        CollaborationReceiver { inner: rx }
    }

    /// 发送协作消息（路由器分发）
    pub async fn send_collaboration(&self, msg: CollaborationMessage) -> Result<(), BusError> {
        let router = self.collab_router.lock().await;
        router.route(msg).await
    }

    // ─── 通道3: 结果 ─────────────────────────────────────────

    /// 副脑提交结果
    pub async fn submit_result(&self, response: BrainResponse) -> Result<(), BusError> {
        self.result_tx
            .send(response)
            .await
            .map_err(|_| BusError::ChannelClosed)
    }

    /// 主脑取走结果接收端（只能调一次）
    pub async fn take_result_receiver(&self) -> Option<ResultReceiver> {
        let mut guard = self.result_rx.lock().await;
        guard.take().map(|rx| ResultReceiver { inner: rx })
    }

    // ─── 状态查询 ─────────────────────────────────────────────

    pub fn broadcast_receiver_count(&self) -> usize {
        self.broadcast_tx.receiver_count()
    }

    pub async fn collaboration_subscriber_count(&self) -> usize {
        self.collab_router.lock().await.subscriber_count()
    }
}

// ─── 接收端包装 ──────────────────────────────────────────────────

/// 广播接收端(通道1)
///
/// 脱注 backticks around `tokio::broadcast`)
pub struct BroadcastReceiver {
    inner: broadcast::Receiver<BroadcastMessage>,
}

impl BroadcastReceiver {
    pub async fn recv(&mut self) -> Result<BroadcastMessage, BusError> {
        match self.inner.recv().await {
            Ok(msg) => Ok(msg),
            Err(broadcast::error::RecvError::Closed) => Err(BusError::ChannelClosed),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("broadcast receiver lagged by {n} messages");
                // lagged 后继续尝试接收
                self.inner.recv().await.map_err(|_| BusError::ChannelClosed)
            }
        }
    }
}

/// 协作接收端（通道2，按 `BrainId` 已过滤）
pub struct CollaborationReceiver {
    inner: mpsc::Receiver<CollaborationMessage>,
}

impl CollaborationReceiver {
    pub async fn recv(&mut self) -> Option<CollaborationMessage> {
        self.inner.recv().await
    }
}

/// 结果接收端（通道3）
pub struct ResultReceiver {
    inner: mpsc::Receiver<BrainResponse>,
}

impl ResultReceiver {
    pub async fn recv(&mut self) -> Option<BrainResponse> {
        self.inner.recv().await
    }

    pub async fn recv_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<BrainResponse, BusError> {
        tokio::time::timeout(timeout, self.inner.recv())
            .await
            .map_err(|_| BusError::ChannelClosed)?
            .ok_or(BusError::ChannelClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::{BrainContext, CollaborationKind, MessagePriority};

    fn make_broadcast(content: &str) -> BroadcastMessage {
        BroadcastMessage {
            content: content.into(),
            raw_input: content.into(),
            context: BrainContext {
                current_date: "2026-04-03".into(),
                cwd: "/test".into(),
                git_branch: None,
                platform: "darwin".into(),
            },
            timestamp: chrono::Utc::now(),
        }
    }

    fn make_collab(to: Vec<BrainId>, hop: u32) -> CollaborationMessage {
        CollaborationMessage {
            id: "test_msg".into(),
            from: BrainId::master(),
            to,
            correlation_id: None,
            hop_count: hop,
            priority: MessagePriority::Normal,
            content: "测试协作".into(),
            kind: CollaborationKind::Request,
        }
    }

    #[tokio::test]
    async fn test_broadcast_all_subscribers() {
        let bus = BrainBus::new(16, 16, 16);
        let mut rx1 = bus.subscribe_broadcast();
        let mut rx2 = bus.subscribe_broadcast();

        bus.broadcast(make_broadcast("hello")).unwrap();

        let msg1 = rx1.recv().await.unwrap();
        let msg2 = rx2.recv().await.unwrap();
        assert_eq!(msg1.content, "hello");
        assert_eq!(msg2.content, "hello");
    }

    #[tokio::test]
    async fn test_collaboration_point_to_point() {
        let bus = BrainBus::new(16, 16, 16);
        let mut rx_r = bus.subscribe_collaboration(BrainId::reasoning()).await;
        let _rx_m = bus.subscribe_collaboration(BrainId::memory()).await;

        bus.send_collaboration(make_collab(vec![BrainId::reasoning()], 0))
            .await
            .unwrap();

        let received = rx_r.recv().await.unwrap();
        assert_eq!(received.content, "测试协作");
        assert_eq!(received.hop_count, 1);
    }

    #[tokio::test]
    async fn test_result_channel() {
        let bus = BrainBus::new(16, 16, 16);
        let mut result_rx = bus.take_result_receiver().await.unwrap();

        // 第二次取应该返回 None
        assert!(bus.take_result_receiver().await.is_none());

        let response = BrainResponse {
            from: BrainId::reasoning(),
            relevance: 0.9,
            confidence: 0.85,
            result: brain_core::types::BrainResponsePayload::NotRelevant {
                reason: "test".into(),
            },
            need_slow_think: false,
            timestamp: chrono::Utc::now(),
        };

        bus.submit_result(response).await.unwrap();
        let received = result_rx.recv().await.unwrap();
        assert_eq!(received.from, BrainId::reasoning());
    }

    #[tokio::test]
    async fn test_result_timeout() {
        let bus = BrainBus::new(16, 16, 16);
        let mut result_rx = bus.take_result_receiver().await.unwrap();

        let result = result_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_hop_count_prevention() {
        let bus = BrainBus::new(16, 16, 16);
        let _rx = bus.subscribe_collaboration(BrainId::memory()).await;

        let result = bus
            .send_collaboration(make_collab(vec![BrainId::memory()], 4))
            .await;
        assert!(result.is_err());
    }
}
