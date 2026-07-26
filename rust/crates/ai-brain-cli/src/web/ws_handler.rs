//! WebSocket Handler — 连接管理、消息解析和路由分发
//!
//! 负责:
//!   1. WebSocket 升级握手
//!   2. 连接建立后推送初始状态（会话列表、活跃会话、人格列表）
//!   3. 进入消息循环，路由客户端消息到 Orchestrator / SessionManager / PersonaManager
//!   4. 将 ProgressEvent 转发为 WebProgressEvent 给前端

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// 服务端 Ping 间隔（秒）— 防止中间代理/浏览器因空闲超时断开连接
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

use crate::orchestrator::Orchestrator;
use crate::runtime_trace::ExchangePhase;
use crate::web::collaboration::{
    InboxState, LegacyMessageSeed, MemberAddress, PostMessageResult, RoomInputMode, RoomSnapshot,
    DEFAULT_THREAD_KEY,
};
use crate::web::collaboration_runtime::CollaborationRuntime;
use crate::web::progress_adapter::{ChatMessage, PersonaInfo, SessionInfo, WebProgressEvent};
use crate::web::session_manager::{ConversationFork, SessionManager, UserQueryTurn};
use brain_core::types::{MainBrainOutput, ProgressEvent};
use brain_main::conversation::ChatMessageRestore;
use brain_memory::conversation_memory::{ConversationMemoryInvalidation, ConversationMemoryScope};

fn extract_modified_file_path(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    ["path", "file_path", "filePath"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(|item| item.as_str()))
        .filter(|path| !path.trim().is_empty())
        .map(str::to_string)
}

// ─── AppState ────────────────────────────────────────────────────────

/// WebSocket 共享状态
pub struct AppState {
    pub orch: Arc<Orchestrator>,
    pub sessions: Arc<Mutex<SessionManager>>,
    pub collaboration: Arc<CollaborationRuntime>,
    pub workspace_root: std::path::PathBuf,
}

// ─── 客户端消息枚举 ──────────────────────────────────────────────────

/// 客户端发送的 WebSocket 消息
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// 发起查询
    Query { input: String },
    /// 编辑一条用户消息并从该点重新生成
    EditUserMessage { message_id: String, content: String },
    /// 重新发送最后一条用户消息（不追加副本）
    RetryLastUserMessage { message_id: String },
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
    /// 删除当前会话窗口中的一整轮历史（仅隐藏 UI/上下文，不删除历史文件内容）
    DeleteTurn { message_index: usize },
    /// 获取当前房间的权威快照
    RequestRoomSnapshot,
    /// 加入房间并从客户端已确认的 sequence 之后重放。
    JoinRoom {
        room_id: String,
        after_sequence: u64,
    },
    /// 向一个或多个成员发送群聊消息/任务
    PostRoomMessage {
        recipients: Vec<MemberAddress>,
        content: String,
        mode: RoomInputMode,
        #[serde(default = "default_thread_key")]
        thread_key: String,
        expected_room_version: u64,
        #[serde(default)]
        command_id: String,
    },
    CreateMember {
        display_name: String,
        model_policy: Option<String>,
        reasoning_depth: Option<String>,
    },
    ConfigureMember {
        member_id: String,
        display_name: String,
        model_policy: String,
        reasoning_depth: String,
        expected_version: u64,
    },
    WakeMember {
        member_id: String,
        expected_version: u64,
    },
    SleepMember {
        member_id: String,
        expected_version: u64,
    },
    ArchiveMember {
        member_id: String,
        expected_version: u64,
    },
    RestoreMember {
        member_id: String,
        expected_version: u64,
    },
    InterruptMemberRun {
        member_id: String,
        run_id: String,
        expected_version: u64,
    },
    /// 应用层心跳（浏览器无法发送原生 Ping 帧）
    Heartbeat,
}

fn default_thread_key() -> String {
    DEFAULT_THREAD_KEY.into()
}

// ─── WebSocket 升级入口 ──────────────────────────────────────────────

/// WebSocket 升级处理器
pub async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

enum QueryAction {
    Edit { message_id: String, content: String },
}

struct PreparedWebQuery {
    turn: UserQueryTurn,
    memory_scope: ConversationMemoryScope,
    restore_history: Option<Vec<ChatMessageRestore>>,
    updated_messages: Option<Vec<ChatMessage>>,
}

#[derive(Debug)]
struct PendingLegacyQuery {
    room_id: String,
    inbox_item_id: String,
    run_id: Option<String>,
    answer_sent: bool,
    error_sent: bool,
}

impl PendingLegacyQuery {
    fn from_post(result: &PostMessageResult) -> Result<Self, String> {
        let inbox_item = result
            .inbox_items
            .first()
            .ok_or_else(|| "旧 Query 未创建默认成员 Inbox".to_string())?;
        Ok(Self {
            room_id: result.event.room_id.clone(),
            inbox_item_id: inbox_item.inbox_item_id.clone(),
            run_id: inbox_item.run_id.clone(),
            answer_sent: false,
            error_sent: false,
        })
    }
}

fn restore_messages_before_turn(
    messages: &[ChatMessage],
    message_id: &str,
) -> Vec<ChatMessageRestore> {
    messages
        .iter()
        .take_while(|message| message.id != message_id)
        .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
        .map(|message| ChatMessageRestore {
            role: message.role.clone(),
            content: message.content.clone(),
        })
        .collect()
}

async fn prepare_web_query(
    state: &Arc<AppState>,
    action: QueryAction,
) -> Result<PreparedWebQuery, String> {
    let session_id = state.sessions.lock().await.active().id.clone();

    let prepared = match action {
        QueryAction::Edit {
            message_id,
            content,
        } => {
            prepare_conversation_fork(state, &session_id, |sessions| {
                sessions.edit_user_message(&message_id, &content)
            })
            .await
        }
    };

    match prepared {
        Ok((turn, restore_history, updated_messages)) => {
            let memory_scope = match ConversationMemoryScope::new(
                turn.session_id.clone(),
                turn.generation_id.clone(),
            ) {
                Ok(scope) => scope,
                Err(error) => return Err(format!("创建对话记忆代次失败: {error}")),
            };
            Ok(PreparedWebQuery {
                turn,
                memory_scope,
                restore_history,
                updated_messages,
            })
        }
        Err(error) => Err(error),
    }
}

fn default_member_address(snapshot: &RoomSnapshot) -> Result<MemberAddress, String> {
    snapshot
        .members
        .iter()
        .find(|member| member.member_id == snapshot.room.default_member_id)
        .map(|member| MemberAddress {
            member_id: member.member_id.clone(),
            expected_version: member.version,
        })
        .ok_or_else(|| format!("房间 {} 缺少默认成员", snapshot.room.room_id))
}

async fn submit_legacy_query(
    state: &Arc<AppState>,
    input: String,
) -> Result<PendingLegacyQuery, String> {
    let snapshot = active_room_snapshot(state).await?;
    let target = default_member_address(&snapshot)?;
    let result = state
        .collaboration
        .post_message_checked(
            snapshot.room.room_id,
            vec![target],
            input,
            RoomInputMode::Chat,
            DEFAULT_THREAD_KEY.into(),
            snapshot.room.version,
            format!("legacy-query-{}", uuid::Uuid::new_v4()),
        )
        .await?;
    PendingLegacyQuery::from_post(&result)
}

async fn cancel_default_member_run(state: &Arc<AppState>) -> Result<(), String> {
    let snapshot = active_room_snapshot(state).await?;
    let target = default_member_address(&snapshot)?;
    let member = snapshot
        .members
        .iter()
        .find(|member| member.member_id == target.member_id)
        .ok_or_else(|| "默认成员状态已变化".to_string())?;
    let run_id = member
        .active_run_id
        .clone()
        .ok_or_else(|| "默认成员当前没有可中断的运行".to_string())?;
    let inbox = snapshot
        .inbox
        .iter()
        .find(|item| item.run_id.as_deref() == Some(run_id.as_str()))
        .ok_or_else(|| "默认成员运行状态已变化".to_string())?;
    state
        .collaboration
        .interrupt_run_checked(
            snapshot.room.room_id,
            member.member_id.clone(),
            run_id,
            inbox.version,
        )
        .await
}

async fn prepare_conversation_fork(
    state: &Arc<AppState>,
    session_id: &str,
    operation: impl FnOnce(&mut SessionManager) -> Result<ConversationFork, String>,
) -> Result<
    (
        UserQueryTurn,
        Option<Vec<ChatMessageRestore>>,
        Option<Vec<ChatMessage>>,
    ),
    String,
> {
    let (snapshot, fork) = {
        let mut sessions = state.sessions.lock().await;
        if sessions.active().id != session_id {
            return Err("活跃会话已变化，请重试操作".into());
        }
        let snapshot = sessions.active().clone();
        let fork = operation(&mut sessions)?;
        (snapshot, fork)
    };

    let invalidation = ConversationMemoryInvalidation {
        conversation_id: fork.turn.session_id.clone(),
        generation_ids: fork.invalidated_generation_ids.clone(),
        includes_legacy_unscoped: fork.includes_legacy_unscoped,
    };
    if let Err(error) = state
        .orch
        .invalidate_conversation_memory(invalidation)
        .await
    {
        state
            .sessions
            .lock()
            .await
            .restore_active_snapshot(snapshot);
        return Err(error);
    }

    let restore_history = restore_messages_before_turn(&fork.messages, &fork.turn.message_id);
    Ok((fork.turn, Some(restore_history), Some(fork.messages)))
}

async fn active_room_snapshot(
    state: &Arc<AppState>,
) -> Result<crate::web::collaboration::RoomSnapshot, String> {
    let (room_id, title, legacy_messages) = {
        let sessions = state.sessions.lock().await;
        let active = sessions.active();
        let legacy_messages = active
            .messages
            .iter()
            .map(|message| LegacyMessageSeed {
                id: message.id.clone(),
                role: message.role.clone(),
                content: message.content.clone(),
                timestamp: message.timestamp,
                hidden: message.hidden,
            })
            .collect();
        (active.id.clone(), active.title.clone(), legacy_messages)
    };
    state
        .collaboration
        .ensure_room(room_id, title, legacy_messages)
        .await
}

async fn active_room_id(state: &Arc<AppState>) -> String {
    state.sessions.lock().await.active().id.clone()
}

fn collaboration_event_room_id(event: &WebProgressEvent) -> Option<&str> {
    match event {
        WebProgressEvent::RoomSnapshot { snapshot } => Some(&snapshot.room.room_id),
        WebProgressEvent::RoomEventAppended { event } => Some(&event.room_id),
        WebProgressEvent::MemberChanged { member } => Some(&member.room_id),
        WebProgressEvent::RoomEventsReplayed { room_id, .. }
        | WebProgressEvent::InboxItemChanged { room_id, .. }
        | WebProgressEvent::MemberRunProgress { room_id, .. }
        | WebProgressEvent::MemberRunFinished { room_id, .. } => Some(room_id),
        _ => None,
    }
}

fn legacy_query_mirrors(
    pending: &mut Option<PendingLegacyQuery>,
    event: &WebProgressEvent,
) -> Vec<WebProgressEvent> {
    let Some(query) = pending.as_mut() else {
        return Vec::new();
    };
    if collaboration_event_room_id(event) != Some(query.room_id.as_str()) {
        return Vec::new();
    }

    let mut mirrors = Vec::new();
    let mut completed = false;
    match event {
        WebProgressEvent::RoomSnapshot { snapshot } => {
            let Some(item) = snapshot
                .inbox
                .iter()
                .find(|item| item.inbox_item_id == query.inbox_item_id)
            else {
                return mirrors;
            };
            if let Some(run_id) = &item.run_id {
                query.run_id = Some(run_id.clone());
            }
            match item.state {
                InboxState::Completed => {
                    if !query.answer_sent {
                        let answer = item.run_id.as_deref().and_then(|run_id| {
                            snapshot.events.iter().find(|room_event| {
                                room_event.kind == "member_message"
                                    && room_event.run_id.as_deref() == Some(run_id)
                            })
                        });
                        if let Some(answer) = answer {
                            mirrors.push(WebProgressEvent::FinalAnswer {
                                content: answer.content.clone(),
                            });
                            query.answer_sent = true;
                        }
                    }
                    if query.answer_sent {
                        mirrors.push(WebProgressEvent::Done);
                        completed = true;
                    }
                }
                InboxState::Failed => {
                    if !query.error_sent {
                        if let Some(message) = &item.error {
                            mirrors.push(WebProgressEvent::Error {
                                message: message.clone(),
                            });
                            query.error_sent = true;
                        }
                    }
                    mirrors.push(WebProgressEvent::Done);
                    completed = true;
                }
                InboxState::Cancelled => {
                    mirrors.push(WebProgressEvent::Done);
                    completed = true;
                }
                _ => {}
            }
        }
        WebProgressEvent::MemberRunProgress {
            run_id,
            event: run_event,
            ..
        } if query.run_id.as_deref() == Some(run_id.as_str()) => match run_event.as_ref() {
            WebProgressEvent::FinalAnswer { content } if !query.answer_sent => {
                mirrors.push(WebProgressEvent::FinalAnswer {
                    content: content.clone(),
                });
                query.answer_sent = true;
            }
            WebProgressEvent::Error { message } if !query.error_sent => {
                mirrors.push(WebProgressEvent::Error {
                    message: message.clone(),
                });
                query.error_sent = true;
            }
            WebProgressEvent::Done => {
                mirrors.push(WebProgressEvent::Done);
                completed = true;
            }
            _ => {}
        },
        _ => {}
    }
    if completed {
        *pending = None;
    }
    mirrors
}

// ─── 核心处理 ────────────────────────────────────────────────────────

/// 处理一个完整的 WebSocket 连接生命周期
///
/// 使用 `tokio::select!` 并发处理两个分支：
///   - 客户端消息（新建/切换会话、人格切换等）
///   - 查询流式事件（TextDelta、ThinkingDelta 等）
///
/// 这样即使在流式查询期间，客户端仍可执行新建会话等操作，不会卡住。
async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    // 1. 推送初始状态
    if let Err(e) = send_initial_state(&mut sender, &state).await {
        error!("发送初始状态失败: {e}");
        return;
    }

    info!("WebSocket 客户端已连接");

    let mut query_rx: Option<tokio::sync::mpsc::Receiver<ProgressEvent>> = None;
    let mut query_handle: Option<tokio::task::JoinHandle<Result<MainBrainOutput, String>>> = None;
    let mut assistant_text = String::new();
    let mut query_session_id: Option<String> = None;
    let mut query_generation_id: Option<String> = None;
    let mut cancel_token: Option<tokio_util::sync::CancellationToken> = None;
    let mut runtime_trace_rx = state.orch.subscribe_runtime_trace();
    let mut collaboration_rx = state.collaboration.subscribe();
    let mut pending_legacy_query: Option<PendingLegacyQuery> = None;
    let mut exchange_sessions = HashMap::<String, (String, Option<String>)>::new();
    let mut pending_modified_files = HashMap::<String, String>::new();

    // 心跳定时器 — 定期发送 Ping 防止连接因空闲被中间代理/浏览器断开
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    heartbeat.tick().await; // 消耗首次立即触发

    // 2. 并发消息循环 — tokio::select! 同时处理客户端消息、查询流式事件、心跳
    loop {
        tokio::select! {
            // ── 分支0: 心跳保活（定时 Ping） ──
            _ = heartbeat.tick() => {
                if sender.send(Message::Ping(vec![].into())).await.is_err() {
                    warn!("心跳 Ping 发送失败，连接可能已断开");
                    break;
                }
            }

            exchange = runtime_trace_rx.recv() => {
                match exchange {
                    Ok(exchange) => {
                        let exchange_id = exchange.exchange_id.clone();
                        let exchange_phase = exchange.phase;
                        let exchange_target = match exchange_phase {
                            ExchangePhase::Request => {
                                if let Some(session_id) = query_session_id.clone() {
                                    let target = (session_id, query_generation_id.clone());
                                    exchange_sessions.insert(exchange_id.clone(), target.clone());
                                    Some(target)
                                } else {
                                    None
                                }
                            }
                            ExchangePhase::Response => exchange_sessions
                                .get(&exchange_id)
                                .cloned()
                                .or_else(|| {
                                    query_session_id
                                        .clone()
                                        .map(|session_id| (session_id, query_generation_id.clone()))
                                }),
                        };
                        if let Some((session_id, generation_id)) = exchange_target {
                            let mut sessions = state.sessions.lock().await;
                            if !sessions.upsert_exchange_to(
                                &session_id,
                                generation_id.as_deref(),
                                exchange.clone(),
                            ) {
                                warn!("通信轨迹对应的会话或消息代次已失效: {session_id}");
                            }
                        }
                        if send_event(
                            &mut sender,
                            WebProgressEvent::BrainCommunication { exchange },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        if exchange_phase == ExchangePhase::Response {
                            exchange_sessions.remove(&exchange_id);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!("WebSocket 通信轨迹落后，跳过 {skipped} 条事件");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        warn!("运行时通信轨迹通道已关闭");
                        break;
                    }
                }
            }

            collaboration_event = collaboration_rx.recv() => {
                match collaboration_event {
                    Ok(event) => {
                        let mirrors = legacy_query_mirrors(&mut pending_legacy_query, &event);
                        let current_room_id = active_room_id(&state).await;
                        let belongs_to_current_room = collaboration_event_room_id(&event)
                            .is_some_and(|room_id| room_id == current_room_id);
                        if belongs_to_current_room
                            && send_event(&mut sender, event).await.is_err()
                        {
                            break;
                        }
                        let mut mirror_send_failed = false;
                        for mirror in mirrors {
                            if send_event(&mut sender, mirror).await.is_err() {
                                mirror_send_failed = true;
                                break;
                            }
                        }
                        if mirror_send_failed {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!("WebSocket 协作事件落后，跳过 {skipped} 条并刷新快照");
                        match active_room_snapshot(&state).await {
                            Ok(snapshot) => {
                                if send_event(
                                    &mut sender,
                                    WebProgressEvent::RoomSnapshot { snapshot },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                warn!("协作事件落后后刷新快照失败: {error}");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        warn!("协作事件通道已关闭");
                        break;
                    }
                }
            }

            // ── 分支1: 客户端消息（始终活跃） ──
            maybe_msg = receiver.next() => {
                match maybe_msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(client_msg) => {
                                let query_action = match client_msg {
                                    ClientMessage::Query { input } => {
                                        if pending_legacy_query.is_some()
                                            || query_rx.is_some()
                                            || query_handle.is_some()
                                        {
                                            send_event(&mut sender, WebProgressEvent::Error {
                                                message: "当前连接已有查询正在进行，请等待完成".into(),
                                            }).await.ok();
                                        } else {
                                            match submit_legacy_query(&state, input).await {
                                                Ok(pending) => pending_legacy_query = Some(pending),
                                                Err(message) => {
                                                    send_event(
                                                        &mut sender,
                                                        WebProgressEvent::Error { message },
                                                    )
                                                    .await
                                                    .ok();
                                                    send_event(&mut sender, WebProgressEvent::Done)
                                                        .await
                                                        .ok();
                                                }
                                            }
                                        }
                                        None
                                    }
                                    ClientMessage::EditUserMessage { message_id, content } => {
                                        Some(QueryAction::Edit { message_id, content })
                                    }
                                    ClientMessage::RetryLastUserMessage { message_id } => {
                                        if message_id.trim().is_empty() {
                                            send_event(
                                                &mut sender,
                                                WebProgressEvent::Error {
                                                    message: "重试消息 ID 不能为空".into(),
                                                },
                                            )
                                            .await
                                            .ok();
                                        } else {
                                            let room_id = active_room_id(&state).await;
                                            match state
                                                .collaboration
                                                .retry_last_user_message(room_id, message_id)
                                                .await
                                            {
                                                Ok(snapshot) => {
                                                    send_event(
                                                        &mut sender,
                                                        WebProgressEvent::RoomSnapshot { snapshot },
                                                    )
                                                    .await
                                                    .ok();
                                                }
                                                Err(message) => {
                                                    send_event(
                                                        &mut sender,
                                                        WebProgressEvent::Error { message },
                                                    )
                                                    .await
                                                    .ok();
                                                }
                                            }
                                        }
                                        None
                                    }
                                    ClientMessage::Cancel => {
                                        let direct_query = cancel_token.is_some()
                                            || query_handle.is_some()
                                            || query_rx.is_some();
                                        if direct_query {
                                            if let Some(ref ct) = cancel_token {
                                                ct.cancel();
                                                info!("已取消当前连接的兼容查询 (CancellationToken)");
                                            }
                                            if let Some(handle) = query_handle.take() {
                                                handle.abort();
                                            }
                                            if !assistant_text.is_empty() {
                                                let mut sessions = state.sessions.lock().await;
                                                if let Some(ref sid) = query_session_id {
                                                    sessions.push_message_to(
                                                        sid,
                                                        "assistant",
                                                        &assistant_text,
                                                    );
                                                }
                                            }
                                        } else if let Err(message) = cancel_default_member_run(&state).await {
                                            send_event(
                                                &mut sender,
                                                WebProgressEvent::Error { message },
                                            )
                                            .await
                                            .ok();
                                        }
                                        query_rx = None;
                                        query_session_id = None;
                                        query_generation_id = None;
                                        cancel_token = None;
                                        pending_legacy_query = None;
                                        assistant_text.clear();
                                        send_event(&mut sender, WebProgressEvent::Done).await.ok();
                                        None
                                    }
                                    other => {
                                        handle_client_message(
                                            &mut sender,
                                            &state,
                                            other,
                                        )
                                        .await;
                                        None
                                    }
                                };

                                let Some(query_action) = query_action else {
                                    continue;
                                };
                                if query_rx.is_some()
                                    || query_handle.is_some()
                                    || pending_legacy_query.is_some()
                                {
                                    send_event(&mut sender, WebProgressEvent::Error {
                                        message: "当前有查询正在进行，请等待完成".into(),
                                    }).await.ok();
                                    continue;
                                }

                                match prepare_web_query(
                                    &state,
                                    query_action,
                                ).await {
                                    Ok(prepared) => {
                                        if let Some(messages) = prepared.updated_messages.clone() {
                                            send_session_list(&mut sender, &state).await.ok();
                                            if send_event(
                                                &mut sender,
                                                WebProgressEvent::SessionMessagesUpdated {
                                                    session_id: prepared.turn.session_id.clone(),
                                                    messages,
                                                },
                                            ).await.is_err() {
                                                break;
                                            }
                                        }
                                        if let Some(history) = prepared.restore_history {
                                            state.orch.restore_session_history(history).await;
                                            info!("已恢复会话 {} 的历史到 MainBrain", prepared.turn.session_id);
                                        }

                                        let current_id = prepared.turn.session_id.clone();
                                        let generation_id = prepared.turn.generation_id.clone();
                                        let input = prepared.turn.input;
                                        let (rx, handle, cancel) = state
                                            .orch
                                            .query_streaming_scoped(&input, prepared.memory_scope);
                                        query_session_id = Some(current_id);
                                        query_generation_id = Some(generation_id);
                                        query_rx = Some(rx);
                                        query_handle = Some(handle);
                                        cancel_token = Some(cancel);
                                        assistant_text.clear();
                                    }
                                    Err(message) => {
                                        send_event(&mut sender, WebProgressEvent::Error { message })
                                            .await
                                            .ok();
                                    }
                                }
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
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket 客户端关闭连接");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sender.send(Message::Pong(data)).await;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket 接收错误: {e}");
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }

            // ── 分支2: 查询流式事件（仅当查询活跃时） ──
            maybe_event = async {
                match &mut query_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match maybe_event {
                    Some(event) => {
                        match &event {
                            ProgressEvent::ToolStart { call_id, tool_name, input, .. }
                                if matches!(tool_name.as_str(), "write_file" | "edit_file" | "Write" | "Edit") =>
                            {
                                if let Some(path) = extract_modified_file_path(input) {
                                    pending_modified_files.insert(call_id.clone(), path);
                                }
                            }
                            ProgressEvent::ToolDone { call_id, is_error, .. } if !is_error => {
                                if let (Some(path), Some(session_id)) = (
                                    pending_modified_files.remove(call_id),
                                    query_session_id.as_deref(),
                                ) {
                                    let files = state.sessions.lock().await
                                        .record_modified_file_to(session_id, &path);
                                    if send_event(&mut sender, WebProgressEvent::SessionFilesUpdated {
                                        session_id: session_id.to_string(),
                                        files,
                                    }).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                        if let Some(web_event) = WebProgressEvent::from_progress(&event) {
                            if let WebProgressEvent::TextDelta { ref text } = web_event {
                                assistant_text.push_str(text);
                            }
                            // Done 必须排在权威 FinalAnswer 之后，由 query_handle 分支发送。
                            if matches!(web_event, WebProgressEvent::Done) {
                                continue;
                            }
                            if send_event(&mut sender, web_event).await.is_err() {
                                break;
                            }
                        }
                    }
                    None => {
                        // 最终答案和会话提交由 query_handle 分支统一处理。
                        query_rx = None;
                    }
                }
            }

            completed_query = async {
                match &mut query_handle {
                    Some(handle) => Some(handle.await),
                    None => std::future::pending().await,
                }
            } => {
                let session_id = query_session_id.take();
                query_generation_id = None;
                query_handle = None;
                query_rx = None;
                cancel_token = None;
                match completed_query {
                    Some(Ok(Ok(output))) => {
                        let final_answer = output.answer;
                        if let Some(ref sid) = session_id {
                            let mut sessions = state.sessions.lock().await;
                            sessions.push_message_to(sid, "assistant", &final_answer);
                        }
                        if send_event(
                            &mut sender,
                            WebProgressEvent::FinalAnswer { content: final_answer },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Err(error))) => {
                        send_event(&mut sender, WebProgressEvent::Error { message: error })
                            .await
                            .ok();
                    }
                    Some(Err(error)) if !error.is_cancelled() => {
                        send_event(
                            &mut sender,
                            WebProgressEvent::Error {
                                message: format!("查询任务异常结束: {error}"),
                            },
                        )
                        .await
                        .ok();
                    }
                    _ => {}
                }
                assistant_text.clear();
                if send_event(&mut sender, WebProgressEvent::Done).await.is_err() {
                    break;
                }
            }
        }
    }

    if let Some(cancel) = cancel_token.take() {
        cancel.cancel();
    }
    if let Some(handle) = query_handle.take() {
        handle.abort();
    }
    query_session_id.take();

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
                created_at: s.created_at.parse().unwrap_or_else(|_| chrono::Utc::now()),
                message_count: s.messages.iter().filter(|m| !m.hidden).count(),
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
                messages: sessions.active_visible_messages(),
                files: active.modified_files.clone(),
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

    let snapshot = active_room_snapshot(state)
        .await
        .map_err(|error| axum::Error::new(std::io::Error::other(error)))?;
    send_event(sender, WebProgressEvent::RoomSnapshot { snapshot }).await?;

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
        ClientMessage::Query { .. }
        | ClientMessage::EditUserMessage { .. }
        | ClientMessage::RetryLastUserMessage { .. }
        | ClientMessage::Cancel
        | ClientMessage::Heartbeat => {
            // 查询控制已在 select! 中处理；应用层心跳无需额外动作。
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
        ClientMessage::DeleteTurn { message_index } => {
            handle_delete_turn(sender, state, message_index).await
        }
        ClientMessage::RequestRoomSnapshot => match active_room_snapshot(state).await {
            Ok(snapshot) => {
                send_event(sender, WebProgressEvent::RoomSnapshot { snapshot })
                    .await
                    .ok();
            }
            Err(error) => send_collaboration_error(sender, error).await,
        },
        ClientMessage::JoinRoom {
            room_id,
            after_sequence,
        } => {
            let active_room = active_room_id(state).await;
            if room_id != active_room {
                send_collaboration_error(
                    sender,
                    format!("房间 {room_id} 不是当前活跃会话，请先切换会话"),
                )
                .await;
                return;
            }
            match active_room_snapshot(state).await {
                Ok(snapshot) => {
                    let through_sequence = snapshot.room.latest_event_seq;
                    if send_event(sender, WebProgressEvent::RoomSnapshot { snapshot })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    match state
                        .collaboration
                        .events_after(room_id.clone(), after_sequence)
                        .await
                    {
                        Ok(events) => {
                            send_event(
                                sender,
                                WebProgressEvent::RoomEventsReplayed {
                                    room_id,
                                    after_sequence,
                                    through_sequence,
                                    events,
                                },
                            )
                            .await
                            .ok();
                        }
                        Err(error) => send_collaboration_error(sender, error).await,
                    }
                }
                Err(error) => send_collaboration_error(sender, error).await,
            }
        }
        ClientMessage::PostRoomMessage {
            recipients,
            content,
            mode,
            thread_key,
            expected_room_version,
            command_id,
        } => {
            let room_id = active_room_id(state).await;
            let command_id = if command_id.trim().is_empty() {
                format!("web-{}", uuid::Uuid::new_v4())
            } else {
                command_id
            };
            if let Err(error) = state
                .collaboration
                .post_message_checked(
                    room_id,
                    recipients,
                    content,
                    mode,
                    thread_key,
                    expected_room_version,
                    command_id,
                )
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
        ClientMessage::CreateMember {
            display_name,
            model_policy,
            reasoning_depth,
        } => {
            let room_id = active_room_id(state).await;
            if let Err(error) = state
                .collaboration
                .create_member(room_id, display_name, model_policy, reasoning_depth)
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
        ClientMessage::ConfigureMember {
            member_id,
            display_name,
            model_policy,
            reasoning_depth,
            expected_version,
        } => {
            let room_id = active_room_id(state).await;
            if let Err(error) = state
                .collaboration
                .configure_member(
                    room_id,
                    member_id,
                    display_name,
                    model_policy,
                    reasoning_depth,
                    expected_version,
                )
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
        ClientMessage::WakeMember {
            member_id,
            expected_version,
        } => {
            let room_id = active_room_id(state).await;
            if let Err(error) = state
                .collaboration
                .wake_member_checked(room_id, member_id, expected_version)
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
        ClientMessage::SleepMember {
            member_id,
            expected_version,
        } => {
            let room_id = active_room_id(state).await;
            if let Err(error) = state
                .collaboration
                .sleep_member_checked(room_id, member_id, expected_version)
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
        ClientMessage::ArchiveMember {
            member_id,
            expected_version,
        } => {
            let room_id = active_room_id(state).await;
            if let Err(error) = state
                .collaboration
                .archive_member_checked(room_id, member_id, expected_version)
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
        ClientMessage::RestoreMember {
            member_id,
            expected_version,
        } => {
            let room_id = active_room_id(state).await;
            if let Err(error) = state
                .collaboration
                .restore_member_checked(room_id, member_id, expected_version)
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
        ClientMessage::InterruptMemberRun {
            member_id,
            run_id,
            expected_version,
        } => {
            let room_id = active_room_id(state).await;
            if let Err(error) = state
                .collaboration
                .interrupt_run_checked(room_id, member_id, run_id, expected_version)
                .await
            {
                send_collaboration_error(sender, error).await;
            }
        }
    }
}

async fn send_collaboration_error(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: String,
) {
    send_event(sender, WebProgressEvent::Error { message })
        .await
        .ok();
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
            send_event(sender, WebProgressEvent::PersonaSwitched { persona_id })
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
    let (new_id, new_messages, files) = {
        let mut sessions = state.sessions.lock().await;
        let (new_id, files) = {
            let new_session = sessions.create("New Session".to_string());
            (new_session.id.clone(), new_session.modified_files.clone())
        };
        (new_id, sessions.active_visible_messages(), files)
    };

    // 推送更新后的会话列表
    send_session_list(sender, state).await.ok();
    // 推送会话切换
    send_event(
        sender,
        WebProgressEvent::SessionSwitched {
            session_id: new_id,
            messages: new_messages,
            files,
        },
    )
    .await
    .ok();
    match active_room_snapshot(state).await {
        Ok(snapshot) => {
            send_event(sender, WebProgressEvent::RoomSnapshot { snapshot })
                .await
                .ok();
        }
        Err(error) => send_collaboration_error(sender, error).await,
    }
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
            Some(s) => Some((
                s.id.clone(),
                s.messages.iter().filter(|m| !m.hidden).cloned().collect(),
                s.modified_files.clone(),
            )),
            None => None,
        }
    };

    match result {
        Some((id, messages, files)) => {
            send_event(
                sender,
                WebProgressEvent::SessionSwitched {
                    session_id: id,
                    messages,
                    files,
                },
            )
            .await
            .ok();
            match active_room_snapshot(state).await {
                Ok(snapshot) => {
                    send_event(sender, WebProgressEvent::RoomSnapshot { snapshot })
                        .await
                        .ok();
                }
                Err(error) => send_collaboration_error(sender, error).await,
            }
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

/// 删除当前会话窗口中的一整轮历史。
async fn handle_delete_turn(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &Arc<AppState>,
    message_index: usize,
) {
    let result = {
        let mut sessions = state.sessions.lock().await;
        let session_id = sessions.active().id.clone();
        sessions
            .hide_turn_by_visible_index(message_index)
            .map(|messages| (session_id, messages))
    };

    match result {
        Some((session_id, messages)) => {
            send_session_list(sender, state).await.ok();
            send_event(
                sender,
                WebProgressEvent::SessionMessagesUpdated {
                    session_id,
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
                    message: format!("无法删除第 {message_index} 条历史消息"),
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
            message_count: s.messages.iter().filter(|m| !m.hidden).count(),
        })
        .collect();
    send_event(sender, WebProgressEvent::SessionList { sessions: list }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_and_retry_client_messages_deserialize_with_stable_ids() {
        let edit: ClientMessage = serde_json::from_str(
            r#"{"type":"edit_user_message","message_id":"msg_1","content":"修订内容"}"#,
        )
        .unwrap();
        match edit {
            ClientMessage::EditUserMessage {
                message_id,
                content,
            } => {
                assert_eq!(message_id, "msg_1");
                assert_eq!(content, "修订内容");
            }
            other => panic!("expected edit message, got {other:?}"),
        }

        let retry: ClientMessage =
            serde_json::from_str(r#"{"type":"retry_last_user_message","message_id":"msg_2"}"#)
                .unwrap();
        match retry {
            ClientMessage::RetryLastUserMessage { message_id } => {
                assert_eq!(message_id, "msg_2");
            }
            other => panic!("expected retry message, got {other:?}"),
        }
    }

    #[test]
    fn room_timeline_exposes_retry_only_for_last_user_event() {
        let script = include_str!("static/app.js");

        assert!(script.contains("lastVisibleUserEventId"));
        assert!(script.contains("candidate.sequence <= event.sequence"));
        assert!(script.contains("send('retry_last_user_message', { message_id: event.event_id })"));
    }

    #[test]
    fn collaboration_client_messages_deserialize_with_explicit_targets() {
        let post: ClientMessage = serde_json::from_str(
            r#"{"type":"post_room_message","recipients":[{"member_id":"member-a","expected_version":3},{"member_id":"member-b","expected_version":5}],"content":"分别检查接口和界面","mode":"task","thread_key":"review","expected_room_version":8,"command_id":"command-1"}"#,
        )
        .unwrap();
        match post {
            ClientMessage::PostRoomMessage {
                recipients,
                content,
                mode,
                thread_key,
                expected_room_version,
                command_id,
            } => {
                assert_eq!(
                    recipients,
                    vec![
                        MemberAddress {
                            member_id: "member-a".into(),
                            expected_version: 3,
                        },
                        MemberAddress {
                            member_id: "member-b".into(),
                            expected_version: 5,
                        },
                    ]
                );
                assert_eq!(content, "分别检查接口和界面");
                assert_eq!(mode, RoomInputMode::Task);
                assert_eq!(thread_key, "review");
                assert_eq!(expected_room_version, 8);
                assert_eq!(command_id, "command-1");
            }
            other => panic!("expected room message, got {other:?}"),
        }

        let interrupt: ClientMessage = serde_json::from_str(
            r#"{"type":"interrupt_member_run","member_id":"member-a","run_id":"run-1","expected_version":11}"#,
        )
        .unwrap();
        assert!(matches!(
            interrupt,
            ClientMessage::InterruptMemberRun { member_id, run_id, expected_version }
                if member_id == "member-a" && run_id == "run-1" && expected_version == 11
        ));

        let join: ClientMessage =
            serde_json::from_str(r#"{"type":"join_room","room_id":"room-1","after_sequence":42}"#)
                .unwrap();
        assert!(matches!(
            join,
            ClientMessage::JoinRoom { room_id, after_sequence }
                if room_id == "room-1" && after_sequence == 42
        ));
    }

    #[test]
    fn history_restore_stops_before_regenerated_user_turn() {
        let message = |id: &str, role: &str, content: &str| ChatMessage {
            id: id.into(),
            role: role.into(),
            content: content.into(),
            timestamp: chrono::Utc::now(),
            hidden: false,
            exchange: None,
            memory_generation_id: None,
        };
        let messages = vec![
            message("user_1", "user", "第一问"),
            message("trace_1", "brain_communication", "内部轨迹"),
            message("assistant_1", "assistant", "第一答"),
            message("user_2", "user", "第二问"),
        ];

        let restored = restore_messages_before_turn(&messages, "user_2");
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].role, "user");
        assert_eq!(restored[0].content, "第一问");
        assert_eq!(restored[1].role, "assistant");
        assert_eq!(restored[1].content, "第一答");
    }

    #[test]
    fn legacy_query_targets_the_authoritative_default_member() {
        let directory = tempfile::tempdir().unwrap();
        let repository = crate::web::collaboration::CollaborationRepository::new(
            directory.path(),
            crate::web::collaboration::CollaborationConfig::default(),
        )
        .unwrap();
        let snapshot = repository.ensure_room("room-1", "Legacy", &[]).unwrap();

        let target = default_member_address(&snapshot).unwrap();

        assert_eq!(target.member_id, snapshot.room.default_member_id);
        assert_eq!(target.expected_version, snapshot.members[0].version);
        assert!(snapshot.inbox.is_empty());
    }

    #[test]
    fn legacy_query_mirrors_only_its_bound_run_completion() {
        let directory = tempfile::tempdir().unwrap();
        let repository = crate::web::collaboration::CollaborationRepository::new(
            directory.path(),
            crate::web::collaboration::CollaborationConfig::default(),
        )
        .unwrap();
        let snapshot = repository.ensure_room("room-1", "Legacy", &[]).unwrap();
        let posted = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "兼容查询",
                RoomInputMode::Chat,
                "legacy-mirror-command",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();
        let mut pending = Some(PendingLegacyQuery::from_post(&posted).unwrap());

        let running_snapshot = WebProgressEvent::RoomSnapshot {
            snapshot: repository.snapshot("room-1").unwrap(),
        };
        assert!(legacy_query_mirrors(&mut pending, &running_snapshot).is_empty());
        assert_eq!(
            pending.as_ref().and_then(|query| query.run_id.as_deref()),
            Some(claim.run_id.as_str())
        );

        let unrelated = WebProgressEvent::MemberRunProgress {
            room_id: "room-1".into(),
            member_id: claim.member_id.clone(),
            run_id: "run-unrelated".into(),
            event: Box::new(WebProgressEvent::FinalAnswer {
                content: "错误目标".into(),
            }),
        };
        assert!(legacy_query_mirrors(&mut pending, &unrelated).is_empty());

        let final_answer = WebProgressEvent::MemberRunProgress {
            room_id: "room-1".into(),
            member_id: claim.member_id.clone(),
            run_id: claim.run_id.clone(),
            event: Box::new(WebProgressEvent::FinalAnswer {
                content: "兼容答案".into(),
            }),
        };
        let mirrored = legacy_query_mirrors(&mut pending, &final_answer);
        assert!(matches!(
            mirrored.as_slice(),
            [WebProgressEvent::FinalAnswer { content }] if content == "兼容答案"
        ));

        let done = WebProgressEvent::MemberRunProgress {
            room_id: "room-1".into(),
            member_id: claim.member_id,
            run_id: claim.run_id,
            event: Box::new(WebProgressEvent::Done),
        };
        assert!(matches!(
            legacy_query_mirrors(&mut pending, &done).as_slice(),
            [WebProgressEvent::Done]
        ));
        assert!(pending.is_none());
    }

    #[test]
    fn legacy_query_recovers_completion_from_authoritative_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let repository = crate::web::collaboration::CollaborationRepository::new(
            directory.path(),
            crate::web::collaboration::CollaborationConfig::default(),
        )
        .unwrap();
        let snapshot = repository.ensure_room("room-1", "Legacy", &[]).unwrap();
        let posted = repository
            .post_message(
                "room-1",
                std::slice::from_ref(&snapshot.room.default_member_id),
                "断线边界查询",
                RoomInputMode::Chat,
                "legacy-snapshot-command",
            )
            .unwrap();
        let claim = repository.claim_next().unwrap().unwrap();
        repository.complete_item(&claim, "持久答案").unwrap();
        let mut pending = Some(PendingLegacyQuery::from_post(&posted).unwrap());

        let completed_snapshot = WebProgressEvent::RoomSnapshot {
            snapshot: repository.snapshot("room-1").unwrap(),
        };
        let mirrored = legacy_query_mirrors(&mut pending, &completed_snapshot);
        assert!(matches!(
            mirrored.as_slice(),
            [WebProgressEvent::FinalAnswer { content }, WebProgressEvent::Done]
                if content == "持久答案"
        ));
        assert!(pending.is_none());
    }
}
