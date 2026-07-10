//! WebProgressEvent — ProgressEvent 的可序列化版本，供 WebSocket 传输。
//!
//! 去除不可序列化字段（`AskUser.response_tx`），并增加 WebSocket 控制消息。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::runtime_trace::RuntimeExchange;

// ─── 辅助结构体 ──────────────────────────────────────────────────────

/// 会话信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub message_count: usize,
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// 是否从当前 Web 会话窗口隐藏。
    ///
    /// 隐藏不会删除磁盘中的历史内容；后续恢复给模型的上下文会跳过隐藏消息。
    #[serde(default)]
    pub hidden: bool,
}

/// 人格信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

// ─── WebProgressEvent ───────────────────────────────────────────────

/// 可序列化的进度事件，用于 WebSocket 传输。
///
/// 使用 `#[serde(tag = "type")]` 做 tagged union 序列化，
/// 前端可根据 `type` 字段分发处理。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebProgressEvent {
    // ── 来自 ProgressEvent 的可序列化变体 ──
    Connecting {
        brain: String,
        model: String,
    },
    Thinking {
        brain: String,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        content: String,
    },
    IntermediateConclusion {
        brain: String,
        content: String,
    },
    ToolStart {
        call_id: String,
        brain: String,
        tool_name: String,
        input: String,
    },
    ToolDone {
        call_id: String,
        brain: String,
        tool_name: String,
        duration_ms: u64,
        output_preview: String,
        is_error: bool,
    },
    MemoryInjected {
        count: usize,
        preview: String,
    },
    MemoryDetail {
        memories: Vec<String>,
    },
    EvaluationStart,
    EvaluationResult {
        passed: bool,
        feedback: String,
    },
    Evaluating,
    LlmRetry {
        attempt: u32,
        max_attempts: u32,
        error: String,
    },
    /// AskUser 去掉 response_tx，前端通过独立的 WebSocket 消息回复
    AskUser {
        question: String,
        options: Option<Vec<String>>,
        multi_select: bool,
    },
    Done,

    /// Full request/result communication emitted outside the query channel.
    BrainCommunication {
        exchange: RuntimeExchange,
    },

    // ── WebSocket 控制消息 ──
    /// 会话列表
    SessionList {
        sessions: Vec<SessionInfo>,
    },
    /// 会话切换确认
    SessionSwitched {
        session_id: String,
        messages: Vec<ChatMessage>,
    },
    /// 当前会话消息发生变化（例如隐藏一整轮历史）
    SessionMessagesUpdated {
        session_id: String,
        messages: Vec<ChatMessage>,
    },
    /// 人格列表
    PersonaList {
        personas: Vec<PersonaInfo>,
        active: String,
    },
    /// 人格切换确认
    PersonaSwitched {
        persona_id: String,
    },
    /// 错误消息
    Error {
        message: String,
    },
}

// ─── ProgressEvent → WebProgressEvent 转换 ──────────────────────────

impl WebProgressEvent {
    /// 从内部 `ProgressEvent` 转换为可序列化的 `WebProgressEvent`。
    ///
    /// - `AskUser` 去掉 `response_tx`（前端通过 WebSocket 回复）
    /// - `EvaluationDetail` 跳过（包含复杂不可序列化类型，前端暂不需要）
    /// - 其余变体一一对应
    pub fn from_progress(event: &brain_core::types::ProgressEvent) -> Option<Self> {
        use brain_core::types::ProgressEvent;

        Some(match event {
            ProgressEvent::Connecting { brain, model } => WebProgressEvent::Connecting {
                brain: brain.clone(),
                model: model.clone(),
            },
            ProgressEvent::Thinking { brain } => WebProgressEvent::Thinking {
                brain: brain.clone(),
            },
            ProgressEvent::TextDelta { text } => WebProgressEvent::TextDelta { text: text.clone() },
            ProgressEvent::ThinkingDelta { content } => WebProgressEvent::ThinkingDelta {
                content: content.clone(),
            },
            ProgressEvent::IntermediateConclusion { brain, content } => {
                WebProgressEvent::IntermediateConclusion {
                    brain: brain.clone(),
                    content: content.clone(),
                }
            }
            ProgressEvent::ToolStart {
                call_id,
                brain,
                tool_name,
                input,
            } => WebProgressEvent::ToolStart {
                call_id: call_id.clone(),
                brain: brain.clone(),
                tool_name: tool_name.clone(),
                input: input.clone(),
            },
            ProgressEvent::ToolDone {
                call_id,
                brain,
                tool_name,
                duration_ms,
                output_preview,
                is_error,
            } => WebProgressEvent::ToolDone {
                call_id: call_id.clone(),
                brain: brain.clone(),
                tool_name: tool_name.clone(),
                duration_ms: *duration_ms,
                output_preview: output_preview.clone(),
                is_error: *is_error,
            },
            ProgressEvent::MemoryInjected { count, preview } => WebProgressEvent::MemoryInjected {
                count: *count,
                preview: preview.clone(),
            },
            ProgressEvent::MemoryDetail { memories } => WebProgressEvent::MemoryDetail {
                memories: memories.clone(),
            },
            ProgressEvent::EvaluationStart => WebProgressEvent::EvaluationStart,
            ProgressEvent::EvaluationResult { passed, feedback } => {
                WebProgressEvent::EvaluationResult {
                    passed: *passed,
                    feedback: feedback.clone(),
                }
            }
            // 两类后台详情暂不进入 Web UI。
            ProgressEvent::EvaluationDetail { .. } | ProgressEvent::BacklogEntryDetected { .. } => {
                return None
            }
            ProgressEvent::Evaluating => WebProgressEvent::Evaluating,
            ProgressEvent::LlmRetry {
                attempt,
                max_attempts,
                error,
            } => WebProgressEvent::LlmRetry {
                attempt: *attempt,
                max_attempts: *max_attempts,
                error: error.clone(),
            },
            // AskUser 去掉 response_tx，前端通过 WebSocket 消息回复
            ProgressEvent::AskUser {
                question,
                options,
                multi_select,
                response_tx: _,
            } => WebProgressEvent::AskUser {
                question: question.clone(),
                options: options.clone(),
                multi_select: *multi_select,
            },
            ProgressEvent::Done => WebProgressEvent::Done,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_trace::{ExchangeKind, ExchangePhase, ExchangeStatus};

    #[test]
    fn serialize_connecting() {
        let event = WebProgressEvent::Connecting {
            brain: "main".into(),
            model: "gpt-4".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"connecting""#), "json: {json}");
        assert!(json.contains(r#""brain":"main""#));
    }

    #[test]
    fn serialize_text_delta() {
        let event = WebProgressEvent::TextDelta {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"text_delta""#));
    }

    #[test]
    fn serialize_done() {
        let event = WebProgressEvent::Done;
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, r#"{"type":"done"}"#);
    }

    #[test]
    fn serialize_error() {
        let event = WebProgressEvent::Error {
            message: "something went wrong".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"error""#));
    }

    #[test]
    fn serialize_session_list() {
        let event = WebProgressEvent::SessionList {
            sessions: vec![SessionInfo {
                id: "s1".into(),
                title: "Test Session".into(),
                created_at: Utc::now(),
                message_count: 5,
            }],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"session_list""#));
    }

    #[test]
    fn from_progress_text_delta() {
        let pe = brain_core::types::ProgressEvent::TextDelta {
            text: "hello world".into(),
        };
        let web = WebProgressEvent::from_progress(&pe).unwrap();
        match web {
            WebProgressEvent::TextDelta { text } => assert_eq!(text, "hello world"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    #[test]
    fn from_progress_intermediate_conclusion() {
        let event = brain_core::types::ProgressEvent::IntermediateConclusion {
            brain: "main".into(),
            content: "已了解文件结构，现在继续检查调用方。".into(),
        };
        let web = WebProgressEvent::from_progress(&event).unwrap();
        let json = serde_json::to_value(web).unwrap();
        assert_eq!(json["type"], "intermediate_conclusion");
        assert_eq!(json["brain"], "main");
        assert_eq!(json["content"], "已了解文件结构，现在继续检查调用方。");
    }

    #[test]
    fn brain_communication_keeps_full_content() {
        let event = WebProgressEvent::BrainCommunication {
            exchange: RuntimeExchange::new(
                "exchange-1",
                "main",
                "主脑",
                "agent:1",
                "子代理",
                ExchangeKind::Delegation,
                ExchangePhase::Request,
                "检查实现",
                "完整任务内容，不应截断",
                ExchangeStatus::Running,
                None,
            ),
        };
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["type"], "brain_communication");
        assert_eq!(json["exchange"]["content"], "完整任务内容，不应截断");
    }

    #[test]
    fn from_progress_evaluation_detail_skipped() {
        let pe = brain_core::types::ProgressEvent::EvaluationDetail {
            score: 0.9,
            reports: vec![],
            instructions: vec![],
        };
        assert!(WebProgressEvent::from_progress(&pe).is_none());
    }

    #[test]
    fn from_progress_ask_user_drops_tx() {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let pe = brain_core::types::ProgressEvent::AskUser {
            question: "Continue?".into(),
            options: Some(vec!["yes".into(), "no".into()]),
            multi_select: false,
            response_tx: brain_core::types::UserResponseSender(tx),
        };
        let web = WebProgressEvent::from_progress(&pe).unwrap();
        match web {
            WebProgressEvent::AskUser {
                question,
                options,
                multi_select,
            } => {
                assert_eq!(question, "Continue?");
                assert_eq!(options.unwrap().len(), 2);
                assert!(!multi_select);
            }
            other => panic!("expected AskUser, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_deserialize() {
        let event = WebProgressEvent::ToolDone {
            call_id: "call-1".into(),
            brain: "eval".into(),
            tool_name: "check".into(),
            duration_ms: 123,
            output_preview: "ok".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: WebProgressEvent = serde_json::from_str(&json).unwrap();
        match back {
            WebProgressEvent::ToolDone {
                brain,
                tool_name,
                duration_ms,
                ..
            } => {
                assert_eq!(brain, "eval");
                assert_eq!(tool_name, "check");
                assert_eq!(duration_ms, 123);
            }
            other => panic!("expected ToolDone, got {other:?}"),
        }
    }
}
