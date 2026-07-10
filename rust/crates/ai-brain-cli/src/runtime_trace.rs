use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeKind {
    Delegation,
    Memory,
    Evaluation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangePhase {
    Request,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeStatus {
    Running,
    Completed,
    Failed,
    Empty,
}

/// One authoritative request or response exchanged between runtime participants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeExchange {
    pub exchange_id: String,
    pub sender: String,
    pub sender_label: String,
    pub receiver: String,
    pub receiver_label: String,
    pub kind: ExchangeKind,
    pub phase: ExchangePhase,
    pub title: String,
    pub content: String,
    pub status: ExchangeStatus,
    pub duration_ms: Option<u64>,
    pub occurred_at: DateTime<Utc>,
}

impl RuntimeExchange {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exchange_id: impl Into<String>,
        sender: impl Into<String>,
        sender_label: impl Into<String>,
        receiver: impl Into<String>,
        receiver_label: impl Into<String>,
        kind: ExchangeKind,
        phase: ExchangePhase,
        title: impl Into<String>,
        content: impl Into<String>,
        status: ExchangeStatus,
        duration_ms: Option<u64>,
    ) -> Self {
        Self {
            exchange_id: exchange_id.into(),
            sender: sender.into(),
            sender_label: sender_label.into(),
            receiver: receiver.into(),
            receiver_label: receiver_label.into(),
            kind,
            phase,
            title: title.into(),
            content: content.into(),
            status,
            duration_ms,
            occurred_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_exchange_serializes_typed_protocol_fields() {
        let exchange = RuntimeExchange::new(
            "delegation-1",
            "main",
            "主脑",
            "novel:1",
            "小说脑",
            ExchangeKind::Delegation,
            ExchangePhase::Request,
            "创作章节",
            "完整任务正文",
            ExchangeStatus::Running,
            None,
        );

        let value = serde_json::to_value(exchange).unwrap();
        assert_eq!(value["exchange_id"], "delegation-1");
        assert_eq!(value["kind"], "delegation");
        assert_eq!(value["phase"], "request");
        assert_eq!(value["content"], "完整任务正文");
    }
}
