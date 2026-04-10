use brain_core::types::{BrainId, CollaborationMessage};

/// 协作消息路由器
///
/// 根据 CollaborationMessage.to 字段路由到对应副脑。
/// 防循环机制：`hop_count` > 3 的消息被丢弃。
pub struct CollaborationRouter {
    subscribers: Vec<(BrainId, tokio::sync::mpsc::Sender<CollaborationMessage>)>,
}

impl CollaborationRouter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscribers: Vec::new(),
        }
    }

    /// 注册副脑
    pub fn subscribe(
        &mut self,
        brain_id: BrainId,
        tx: tokio::sync::mpsc::Sender<CollaborationMessage>,
    ) {
        if self.subscribers.iter().any(|(id, _)| *id == brain_id) {
            return;
        }
        self.subscribers.push((brain_id, tx));
    }

    /// 同步路由：找到目标 sender 并 clone 出来，稍后异步发送
    ///
    /// 返回需要发送的 (sender, message) 对列表，由调用者在释放锁后异步发送。
    fn resolve_targets(
        &self,
        msg: &CollaborationMessage,
    ) -> Result<
        Vec<(
            tokio::sync::mpsc::Sender<CollaborationMessage>,
            CollaborationMessage,
        )>,
        crate::BusError,
    > {
        if msg.hop_count > 3 {
            return Err(crate::BusError::HopCountExceeded {
                hop_count: msg.hop_count,
            });
        }

        let mut targets = Vec::new();

        if msg.to.is_empty() {
            // 广播
            for (_, tx) in &self.subscribers {
                let mut cloned = msg.clone();
                cloned.hop_count += 1;
                targets.push((tx.clone(), cloned));
            }
        } else {
            // 点对点/点对多
            for target in &msg.to {
                if let Some((_, tx)) = self.subscribers.iter().find(|(id, _)| *id == *target) {
                    let mut cloned = msg.clone();
                    cloned.hop_count += 1;
                    targets.push((tx.clone(), cloned));
                } else {
                    tracing::warn!("no subscription for brain: {}", target);
                }
            }
        }

        Ok(targets)
    }

    /// 异步路由（先同步解析目标，再异步发送）
    pub async fn route(&self, msg: CollaborationMessage) -> Result<(), crate::BusError> {
        let targets = self.resolve_targets(&msg)?;
        for (tx, m) in targets {
            if tx.send(m).await.is_err() {
                tracing::warn!("collaboration receiver dropped");
            }
        }
        Ok(())
    }

    /// 订阅者数量
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }
}

impl Default for CollaborationRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::{CollaborationKind, MessagePriority};

    #[tokio::test]
    async fn test_route_point_to_point() {
        let mut router = CollaborationRouter::new();
        let (tx_a, mut rx_a) = tokio::sync::mpsc::channel::<CollaborationMessage>(16);
        let (tx_b, mut rx_b) = tokio::sync::mpsc::channel::<CollaborationMessage>(16);

        router.subscribe(BrainId::reasoning(), tx_a);
        router.subscribe(BrainId::memory(), tx_b);

        let msg = CollaborationMessage {
            id: "msg_1".into(),
            from: BrainId::master(),
            to: vec![BrainId::reasoning()],
            correlation_id: None,
            hop_count: 0,
            priority: MessagePriority::Normal,
            content: "请召回订舱相关记忆".into(),
            kind: CollaborationKind::Request,
        };

        router.route(msg).await.unwrap();

        assert!(rx_a.try_recv().is_ok());
        assert!(rx_b.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_route_broadcast_when_to_empty() {
        let mut router = CollaborationRouter::new();
        let (tx_a, mut rx_a) = tokio::sync::mpsc::channel::<CollaborationMessage>(16);
        let (tx_b, mut rx_b) = tokio::sync::mpsc::channel::<CollaborationMessage>(16);

        router.subscribe(BrainId::reasoning(), tx_a);
        router.subscribe(BrainId::memory(), tx_b);

        let msg = CollaborationMessage {
            id: "msg_2".into(),
            from: BrainId::master(),
            to: vec![],
            correlation_id: None,
            hop_count: 0,
            priority: MessagePriority::Normal,
            content: "广播测试".into(),
            kind: CollaborationKind::Request,
        };

        router.route(msg).await.unwrap();

        assert!(rx_a.try_recv().is_ok());
        assert!(rx_b.try_recv().is_ok());
    }

    #[tokio::test]
    async fn test_hop_count_exceeded() {
        let router = CollaborationRouter::new();

        let msg = CollaborationMessage {
            id: "msg_3".into(),
            from: BrainId::reasoning(),
            to: vec![BrainId::memory()],
            correlation_id: None,
            hop_count: 4,
            priority: MessagePriority::Normal,
            content: "循环消息".into(),
            kind: CollaborationKind::Request,
        };

        let result = router.route(msg).await;
        assert!(result.is_err());
    }
}
