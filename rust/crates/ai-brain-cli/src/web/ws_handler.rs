//! WebSocket Handler — 连接管理、消息解析和路由分发
//!
//! 负责:
//!   1. WebSocket 升级握手
//!   2. 连接建立后推送初始状态（会话列表、活跃会话、人格列表）
//!   3. 进入消息循环，路由客户端消息到 Orchestrator / SessionManager / PersonaManager
//!   4. 将 ProgressEvent 转发为 WebProgressEvent 给前端

use std::sync::Arc;

use axum::{
    extract::State,
    response::IntoResponse,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::orchestrator::Orchestrator;
use crate::web::progress_adapter::{
    ChatMessage, PersonaInfo, SessionInfo, WebProgressEvent,
};
use crate::web::session_manager::SessionManager;

// ─── AppState ────────────────────────────────────────────────────────

/// WebSocket 共享状态
pub struct AppState {
    pub orch: Arc<Orchestrator>,
    pub sessions: Arc<Mutex<SessionManager>>,
}

// ─── 客户端消息枚举 ──────────────────────────────────────────────────

/// 客户端发送的 WebSocket 消息
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// 发起查询
    Query { input: String },
    /// 取消当前查询（MVP 暂空）
    Cancel,
    /// 回复 AskUser 问题（MVP 暂空）
    AskResponse { response: String },
    /// 切换人格
    SwitchPersona { persona_id: String },
    /// 新建会话
    NewSession,
    /// 切换会话
    SwitchSession { session_id: String },
    /// 删除会话
    DeleteSession { session_id: String },
}

// ─── WebSocket 升级入口 ──────────────────────────────────────────────

/// WebSocket 升级处理器
pub async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

// ─── 核心处理 ────────────────────────────────────────────────────────

/// 处理一个完整的 WebSocket 连接生命周期
async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    // 1. 推送初始状态
    if let Err(e) = send_initial_state(&mut sender, &state).await {
        error!("发送初始状态失败: {e}");
        return;
    }

    info!("WebSocket 客户端已连接");

    // 2. 消息循环
    while let Some(msg_result) = receiver.next().await {
        match msg_result {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        handle_client_message(&mut sender, &state, client_msg).await;
                    }
                    Err(e) => {
                        warn!("解析客户端消息失败: {e}, 原始: {text}");
                        send_event(
                            &mut sender,
                            WebProgressEvent::Error {
                                message: format!("无法解析消息: {e}"),
                            },
                        )
                        .await
                        .ok();
                    }
                }
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket 客户端关闭连接");
                break;
            }
            Ok(Message::Ping(data)) => {
                // axum 自动处理 Ping/Pong
                let _ = sender.send(Message::Pong(data)).await;
            }
            Err(e) => {
                error!("WebSocket 接收错误: {e}");
                break;
            }
            _ => {
                // 忽略 Binary, Pong 等消息
            }
        }
    }

    info!("WebSocket 连接已断开");
}

// ─── 初始状态推送 ────────────────────────────────────────────────────

/// 推送初始状态: session_list + session_switched + persona_list
async fn send_initial_state(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
) -> Result<(), axum::Error> {
    // 会话列表
    {
        let sessions = state.sessions.lock().await;
        let list: Vec<SessionInfo> = sessions
            .list()
            .iter()
            .map(|s| SessionInfo {
                id: s.id.clone(),
                title: s.title.clone(),
                created_at: s.created_at.parse().unwrap_or_else(|_| {
                    chrono::Utc::now()
                }),
                message_count: s.messages.len(),
            })
            .collect();
        send_event(sender, WebProgressEvent::SessionList { sessions: list }).await?;
    }

    // 当前活跃会话（包含历史消息）
    {
        let sessions = state.sessions.lock().await;
        let active = sessions.active();
        send_event(
            sender,
            WebProgressEvent::SessionSwitched {
                session_id: active.id.clone(),
                messages: active.messages.clone(),
            },
        )
        .await?;
    }

    // 人格列表
    {
        let mem_brain = state.orch.memory_brain();
        let mem = mem_brain.lock().await;
        let pm = mem.persona_manager();
        let personas: Vec<PersonaInfo> = pm
            .list()
            .iter()
            .map(|p| PersonaInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                description: p.description.clone(),
            })
            .collect();
        send_event(
            sender,
            WebProgressEvent::PersonaList {
                personas,
                active: pm.active_id().to_string(),
            },
        )
        .await?;
    }

    Ok(())
}

// ─── 消息路由 ────────────────────────────────────────────────────────

/// 路由客户端消息到对应的处理函数
async fn handle_client_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
    msg: ClientMessage,
) {
    match msg {
        ClientMessage::Query { input } => handle_query(sender, state, input).await,
        ClientMessage::Cancel => {
            // MVP: 暂空实现
            send_event(
                sender,
                WebProgressEvent::Error {
                    message: "Cancel 功能尚未实现".into(),
                },
            )
            .await
            .ok();
        }
        ClientMessage::AskResponse { .. } => {
            // MVP: 暂空实现
            send_event(
                sender,
                WebProgressEvent::Error {
                    message: "AskResponse 功能尚未实现".into(),
                },
            )
            .await
            .ok();
        }
        ClientMessage::SwitchPersona { persona_id } => {
            handle_switch_persona(sender, state, persona_id).await
        }
        ClientMessage::NewSession => handle_new_session(sender, state).await,
        ClientMessage::SwitchSession { session_id } => {
            handle_switch_session(sender, state, session_id).await
        }
        ClientMessage::DeleteSession { session_id } => {
            handle_delete_session(sender, state, session_id).await
        }
    }
}

// ─── Query 处理 ──────────────────────────────────────────────────────

/// 处理查询消息: 调用 query_streaming，转发 ProgressEvent
async fn handle_query(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
    input: String,
) {
    // 记录用户消息到会话
    {
        let mut sessions = state.sessions.lock().await;
        sessions.push_message("user", &input);
    }

    // 调用流式查询
    let (mut rx, _handle, _cancel) = state.orch.query_streaming(&input);

    // 持续转发 ProgressEvent
    let mut assistant_text = String::new();
    while let Some(event) = rx.recv().await {
        if let Some(web_event) = WebProgressEvent::from_progress(&event) {
            // 收集 assistant 文本
            if let WebProgressEvent::TextDelta { ref text } = web_event {
                assistant_text.push_str(text);
            }

            if send_event(sender, web_event).await.is_err() {
                // 发送失败（客户端断开），退出循环
                break;
            }
        }
    }

    // 查询完成后记录 assistant 回复到会话
    if !assistant_text.is_empty() {
        let mut sessions = state.sessions.lock().await;
        sessions.push_message("assistant", &assistant_text);
    }
}

// ─── 人格切换 ────────────────────────────────────────────────────────

/// 处理人格切换
async fn handle_switch_persona(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
    persona_id: String,
) {
    let result = {
        let mem_brain = state.orch.memory_brain();
        let mut mem = mem_brain.lock().await;
        mem.persona_manager_mut().switch(&persona_id).map(|_| ())
    };

    match result {
        Ok(()) => {
            send_event(
                sender,
                WebProgressEvent::PersonaSwitched { persona_id },
            )
            .await
            .ok();
        }
        Err(e) => {
            send_event(
                sender,
                WebProgressEvent::Error {
                    message: format!("切换人格失败: {e}"),
                },
            )
            .await
            .ok();
        }
    }
}

// ─── 会话管理 ────────────────────────────────────────────────────────

/// 新建会话
async fn handle_new_session(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
) {
    let (new_id, new_messages) = {
        let mut sessions = state.sessions.lock().await;
        let new_session = sessions.create("New Session".to_string());
        (new_session.id.clone(), new_session.messages.clone())
    };

    // 推送更新后的会话列表
    send_session_list(sender, state).await.ok();
    // 推送会话切换
    send_event(
        sender,
        WebProgressEvent::SessionSwitched {
            session_id: new_id,
            messages: new_messages,
        },
    )
    .await
    .ok();
}

/// 切换会话
async fn handle_switch_session(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
    session_id: String,
) {
    let result = {
        let mut sessions = state.sessions.lock().await;
        match sessions.switch(&session_id) {
            Some(s) => Some((s.id.clone(), s.messages.clone())),
            None => None,
        }
    };

    match result {
        Some((id, messages)) => {
            send_event(
                sender,
                WebProgressEvent::SessionSwitched {
                    session_id: id,
                    messages,
                },
            )
            .await
            .ok();
        }
        None => {
            send_event(
                sender,
                WebProgressEvent::Error {
                    message: format!("会话不存在: {session_id}"),
                },
            )
            .await
            .ok();
        }
    }
}

/// 删除会话
async fn handle_delete_session(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
    session_id: String,
) {
    let deleted = {
        let mut sessions = state.sessions.lock().await;
        sessions.delete(&session_id)
    };

    if deleted {
        // 推送更新后的会话列表
        send_session_list(sender, state).await.ok();
    } else {
        send_event(
            sender,
            WebProgressEvent::Error {
                message: format!("无法删除会话 {session_id}（可能是当前活跃会话或唯一会话）"),
            },
        )
        .await
        .ok();
    }
}

// ─── 辅助函数 ────────────────────────────────────────────────────────

/// 发送一个 WebProgressEvent 给客户端
async fn send_event(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: WebProgressEvent,
) -> Result<(), axum::Error> {
    match serde_json::to_string(&event) {
        Ok(json) => sender.send(Message::Text(json.into())).await,
        Err(e) => {
            error!("序列化 WebProgressEvent 失败: {e}");
            // 尝试发送一个简单的错误消息
            let fallback = serde_json::json!({
                "type": "error",
                "message": format!("内部序列化错误: {e}")
            });
            sender
                .send(Message::Text(fallback.to_string().into()))
                .await
        }
    }
}

/// 发送会话列表
async fn send_session_list(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
) -> Result<(), axum::Error> {
    let sessions = state.sessions.lock().await;
    let list: Vec<SessionInfo> = sessions
        .list()
        .iter()
        .map(|s| SessionInfo {
            id: s.id.clone(),
            title: s.title.clone(),
            created_at: s.created_at.parse().unwrap_or_else(|_| chrono::Utc::now()),
            message_count: s.messages.len(),
        })
        .collect();
    send_event(sender, WebProgressEvent::SessionList { sessions: list }).await
}
